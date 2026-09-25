//! Bitcoind chain backend, written as a lampo plugin in Zig 0.16.
//!
//! The daemon spawns this binary and sends `core-url`, `core-user`, and
//! `core-pass` in `init`. Chain sync then calls the RPC methods below
//! instead of opening the bitcoind port itself.
//!
//! Same methods as the Rust `lampo-bitcoind`: `sendrawtransaction`,
//! `getblock`, `getblockheader`, `getblockchaininfo`, `estimatesmartfee`,
//! `getmempoolinfo`. It is `important`: if it fails, the daemon does not start.
//!
//! ```sh
//! zig build -Doptimize=ReleaseSafe
//! # binary: zig-out/bin/lampo-bitcoind
//! ```

const std = @import("std");
const sdk = @import("lampo_plugin_sdk");

const Node = struct {
    io: std.Io,
    allocator: std.mem.Allocator,
    endpoint: []u8 = &.{},
    authorization: []u8 = &.{},
    next_id: u64 = 1,
    client: std.http.Client = undefined,
    ready: bool = false,
    /// Last RPC result handed to the SDK. Freed on the next call and on exit.
    /// The protocol writes the value before the next request is read.
    last_result: ?std.json.Value = null,

    fn deinit(self: *Node) void {
        self.freeLast();
        if (self.ready) self.client.deinit();
        if (self.endpoint.len != 0) self.allocator.free(self.endpoint);
        if (self.authorization.len != 0) self.allocator.free(self.authorization);
    }

    fn freeLast(self: *Node) void {
        if (self.last_result) |value| {
            freeValue(self.allocator, value);
            self.last_result = null;
        }
    }

    fn fromInit(self: *Node, params: std.json.Value) !void {
        const options = sdk.objectGet(params, "options") orelse return error.MissingCoreUrl;
        const url = sdk.objectGetString(options, "core-url") orelse return error.MissingCoreUrl;
        if (url.len == 0) return error.MissingCoreUrl;
        const user = sdk.objectGetString(options, "core-user") orelse "";
        const pass = sdk.objectGetString(options, "core-pass") orelse "";

        self.endpoint = try self.allocator.dupe(u8, url);
        errdefer self.allocator.free(self.endpoint);
        self.authorization = try basicAuth(self.allocator, user, pass);
        self.client = .{ .allocator = self.allocator, .io = self.io };
        self.ready = true;
    }

    fn call(self: *Node, method: []const u8, args: []const std.json.Value) !std.json.Value {
        self.freeLast();
        if (!self.ready) return error.InitHasNotRun;
        const id = self.next_id;
        self.next_id += 1;

        var payload: std.Io.Writer.Allocating = .init(self.allocator);
        defer payload.deinit();
        var jw: std.json.Stringify = .{ .writer = &payload.writer };
        try jw.beginObject();
        try jw.objectField("jsonrpc");
        try jw.write("2.0");
        try jw.objectField("id");
        try jw.write(id);
        try jw.objectField("method");
        try jw.write(method);
        try jw.objectField("params");
        try jw.beginArray();
        for (args) |arg| try jw.write(arg);
        try jw.endArray();
        try jw.endObject();

        var response: std.Io.Writer.Allocating = .init(self.allocator);
        defer response.deinit();
        _ = self.client.fetch(.{
            .location = .{ .url = self.endpoint },
            .method = .POST,
            .payload = payload.written(),
            .headers = .{
                .content_type = .{ .override = "application/json" },
                .authorization = .{ .override = self.authorization },
            },
            .response_writer = &response.writer,
        }) catch return error.TransportError;

        const body = response.written();
        if (body.len == 0) return error.EmptyBody;
        var parsed = std.json.parseFromSlice(std.json.Value, self.allocator, body, .{}) catch {
            return error.InvalidJson;
        };
        errdefer parsed.deinit();

        if (sdk.objectGet(parsed.value, "error")) |err_value| {
            if (err_value != .null) {
                // Keep the parsed tree alive only for the error name. The
                // daemon sees the JSON-RPC error code; the message is the
                // error name because Zig errors cannot carry the bitcoind string.
                parsed.deinit();
                return error.BitcoindError;
            }
        }
        const result = sdk.objectGet(parsed.value, "result") orelse {
            parsed.deinit();
            return error.NoResult;
        };
        // The handler must return a Value whose strings outlive the response.
        // Leak the parse arena into the process allocator by cloning.
        const cloned = try cloneValue(self.allocator, result);
        parsed.deinit();
        self.last_result = cloned;
        return cloned;
    }
};

fn freeValue(allocator: std.mem.Allocator, value: std.json.Value) void {
    switch (value) {
        .null, .bool, .integer, .float => {},
        .number_string, .string => |s| allocator.free(s),
        .array => |items| {
            for (items.items) |item| freeValue(allocator, item);
            var copy = items;
            copy.deinit();
        },
        .object => |obj| {
            var it = obj.iterator();
            while (it.next()) |entry| {
                allocator.free(entry.key_ptr.*);
                freeValue(allocator, entry.value_ptr.*);
            }
            var copy = obj;
            copy.deinit(allocator);
        },
    }
}

fn basicAuth(allocator: std.mem.Allocator, user: []const u8, pass: []const u8) ![]u8 {
    const raw = try std.fmt.allocPrint(allocator, "{s}:{s}", .{ user, pass });
    defer allocator.free(raw);
    const enc_len = std.base64.standard.Encoder.calcSize(raw.len);
    const out = try allocator.alloc(u8, "Basic ".len + enc_len);
    @memcpy(out[0.."Basic ".len], "Basic ");
    _ = std.base64.standard.Encoder.encode(out["Basic ".len..], raw);
    return out;
}

fn cloneValue(allocator: std.mem.Allocator, value: std.json.Value) !std.json.Value {
    return switch (value) {
        .null => .null,
        .bool => |b| .{ .bool = b },
        .integer => |n| .{ .integer = n },
        .float => |n| .{ .float = n },
        .number_string => |s| .{ .number_string = try allocator.dupe(u8, s) },
        .string => |s| .{ .string = try allocator.dupe(u8, s) },
        .array => |items| blk: {
            var copy = std.json.Array.init(allocator);
            errdefer copy.deinit();
            try copy.ensureTotalCapacity(items.items.len);
            for (items.items) |item| {
                copy.appendAssumeCapacity(try cloneValue(allocator, item));
            }
            break :blk .{ .array = copy };
        },
        .object => |obj| blk: {
            var copy: std.json.ObjectMap = .empty;
            errdefer copy.deinit(allocator);
            var it = obj.iterator();
            while (it.next()) |entry| {
                const key = try allocator.dupe(u8, entry.key_ptr.*);
                try copy.put(allocator, key, try cloneValue(allocator, entry.value_ptr.*));
            }
            break :blk .{ .object = copy };
        },
    };
}

fn makeRpc(comptime method: []const u8) sdk.RpcHandler {
    return struct {
        fn handle(ctx: *anyopaque, _: std.mem.Allocator, params: std.json.Value) !std.json.Value {
            const node: *Node = @ptrCast(@alignCast(ctx));
            const args = sdk.rpcArray(params) catch return error.BadParams;
            return node.call(method, args);
        }
    }.handle;
}

pub fn main(init: std.process.Init) !void {
    var node = Node{
        .io = init.io,
        .allocator = init.gpa,
    };
    defer node.deinit();

    var plugin = sdk.Plugin.init(init.gpa);
    defer plugin.deinit();
    plugin.setContext(@ptrCast(&node));
    plugin.setImportant(true);
    plugin.setFailureMode(.fail_closed);
    plugin.onInit(struct {
        fn handle(ctx: *anyopaque, _: std.mem.Allocator, params: std.json.Value) !void {
            const node_ptr: *Node = @ptrCast(@alignCast(ctx));
            try node_ptr.fromInit(params);
        }
    }.handle);

    try plugin.rpcMethod("sendrawtransaction", "Broadcast a raw transaction", "hex", makeRpc("sendrawtransaction"));
    try plugin.rpcMethod("getblock", "Fetch a block by hash", "hash verbosity", makeRpc("getblock"));
    try plugin.rpcMethod("getblockheader", "Fetch a block header by hash", "hash", makeRpc("getblockheader"));
    try plugin.rpcMethod("getblockchaininfo", "Chain tip and height", "", makeRpc("getblockchaininfo"));
    try plugin.rpcMethod("estimatesmartfee", "Fee estimate in BTC/kvB", "blocks mode", makeRpc("estimatesmartfee"));
    try plugin.rpcMethod("getmempoolinfo", "Mempool minimum fee", "", makeRpc("getmempoolinfo"));

    try plugin.run(init);
}
