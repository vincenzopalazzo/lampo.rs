//! Lampo plugin SDK for Zig 0.16.
//!
//! Speaks the same newline-delimited JSON-RPC 2.0 protocol as
//! `plugin-sdk/rust`: `getmanifest`, `init`, `shutdown`, `hook/<name>`,
//! registered RPC methods, and id-less notifications.
//!
//! gRPC (`--lampo-listen`) is not implemented. A plugin started with that
//! flag disables itself so the daemon does not treat a silent process as
//! a live transport.

const std = @import("std");

pub const HookResult = enum {
    @"continue",
    complete,
    reject,
};

pub const HookResponse = struct {
    result: HookResult = .@"continue",
    /// Copied into the response. Owned by the caller until `respond` returns.
    payload: ?std.json.Value = null,
    message: []const u8 = "",
};

pub const FailureMode = enum {
    fail_open,
    fail_closed,

    pub fn wire(self: FailureMode) []const u8 {
        return switch (self) {
            .fail_open => "fail_open",
            .fail_closed => "fail_closed",
        };
    }
};

pub const OptionType = enum {
    string,
    int,
    bool,

    pub fn wire(self: OptionType) []const u8 {
        return switch (self) {
            .string => "string",
            .int => "int",
            .bool => "bool",
        };
    }
};

pub const RpcHandler = *const fn (
    ctx: *anyopaque,
    allocator: std.mem.Allocator,
    params: std.json.Value,
) anyerror!std.json.Value;

pub const HookHandler = *const fn (
    ctx: *anyopaque,
    allocator: std.mem.Allocator,
    params: std.json.Value,
) anyerror!HookResponse;

pub const NotifyHandler = *const fn (
    ctx: *anyopaque,
    allocator: std.mem.Allocator,
    params: std.json.Value,
) void;

pub const InitHandler = *const fn (
    ctx: *anyopaque,
    allocator: std.mem.Allocator,
    params: std.json.Value,
) anyerror!void;

const RpcDecl = struct {
    name: []const u8,
    description: []const u8,
    usage: []const u8,
    handler: RpcHandler,
};

const HookDecl = struct {
    name: []const u8,
    handler: HookHandler,
};

const NotifyDecl = struct {
    topic: []const u8,
    handler: NotifyHandler,
};

const OptionDecl = struct {
    name: []const u8,
    opt_type: OptionType,
    description: []const u8,
};

/// Builder for a stdio lampo plugin. Strings passed to the builder must
/// outlive `run` (string literals do).
pub const Plugin = struct {
    allocator: std.mem.Allocator,
    ctx: *anyopaque = undefined,
    rpc_methods: std.ArrayList(RpcDecl) = .empty,
    hooks: std.ArrayList(HookDecl) = .empty,
    subscriptions: std.ArrayList(NotifyDecl) = .empty,
    options: std.ArrayList(OptionDecl) = .empty,
    on_init: ?InitHandler = null,
    dynamic: bool = false,
    important: bool = false,
    failure_mode: FailureMode = .fail_open,

    pub fn init(allocator: std.mem.Allocator) Plugin {
        return .{ .allocator = allocator };
    }

    pub fn deinit(self: *Plugin) void {
        self.rpc_methods.deinit(self.allocator);
        self.hooks.deinit(self.allocator);
        self.subscriptions.deinit(self.allocator);
        self.options.deinit(self.allocator);
    }

    pub fn setContext(self: *Plugin, ctx: *anyopaque) void {
        self.ctx = ctx;
    }

    pub fn rpcMethod(
        self: *Plugin,
        name: []const u8,
        description: []const u8,
        usage: []const u8,
        handler: RpcHandler,
    ) !void {
        try self.rpc_methods.append(self.allocator, .{
            .name = name,
            .description = description,
            .usage = usage,
            .handler = handler,
        });
    }

    pub fn hook(self: *Plugin, name: []const u8, handler: HookHandler) !void {
        try self.hooks.append(self.allocator, .{ .name = name, .handler = handler });
    }

    pub fn subscribe(self: *Plugin, topic: []const u8, handler: NotifyHandler) !void {
        try self.subscriptions.append(self.allocator, .{ .topic = topic, .handler = handler });
    }

    pub fn option(self: *Plugin, name: []const u8, opt_type: OptionType, description: []const u8) !void {
        try self.options.append(self.allocator, .{
            .name = name,
            .opt_type = opt_type,
            .description = description,
        });
    }

    pub fn onInit(self: *Plugin, handler: InitHandler) void {
        self.on_init = handler;
    }

    pub fn setDynamic(self: *Plugin, dynamic: bool) void {
        self.dynamic = dynamic;
    }

    pub fn setImportant(self: *Plugin, important: bool) void {
        self.important = important;
    }

    pub fn setFailureMode(self: *Plugin, mode: FailureMode) void {
        self.failure_mode = mode;
    }

    /// Read the daemon from stdin and write responses to stdout.
    /// `--lampo-listen` is the gRPC transport; this SDK does not speak it.
    pub fn run(self: *Plugin, proc: std.process.Init) !void {
        if (hasListenFlag(proc)) {
            std.log.err("plugin-sdk/zig does not implement --lampo-listen (gRPC); use the Rust SDK", .{});
            return error.GrpcUnsupported;
        }
        try self.runIo(proc.io);
    }

    pub fn runIo(self: *Plugin, io: std.Io) !void {
        var in_buf: [64 * 1024]u8 = undefined;
        var input = std.Io.File.stdin().readerStreaming(io, &in_buf);
        var out_buf: [64 * 1024]u8 = undefined;
        var output = std.Io.File.stdout().writerStreaming(io, &out_buf);

        while (true) {
            const raw = input.interface.takeDelimiterExclusive('\n') catch |err| switch (err) {
                error.EndOfStream => return,
                else => return err,
            };
            // takeDelimiterExclusive stops on the newline and leaves it buffered.
            input.interface.toss(1);
            const line = std.mem.trim(u8, raw, " \t\r");
            if (line.len == 0) continue;

            var parsed = std.json.parseFromSlice(std.json.Value, self.allocator, line, .{}) catch |err| {
                std.log.warn("invalid JSON: {s}", .{@errorName(err)});
                continue;
            };
            defer parsed.deinit();

            if (parsed.value != .object) continue;
            const msg = parsed.value.object;
            const method = if (msg.get("method")) |m| switch (m) {
                .string => |s| s,
                else => "",
            } else "";
            const id = msg.get("id");
            const params = msg.get("params") orelse .null;

            if (id == null) {
                if (self.findNotify(method)) |handler| {
                    handler(self.ctx, self.allocator, params);
                }
                continue;
            }

            const stop = try self.dispatch(&output.interface, method, id.?, params);
            try output.interface.flush();
            if (stop) return;
        }
    }

    fn dispatch(
        self: *Plugin,
        out: *std.Io.Writer,
        method: []const u8,
        id: std.json.Value,
        params: std.json.Value,
    ) !bool {
        if (std.mem.eql(u8, method, "getmanifest")) {
            try self.writeManifest(out, id);
            return false;
        }
        if (std.mem.eql(u8, method, "init")) {
            try self.writeInit(out, id, params);
            return false;
        }
        if (std.mem.eql(u8, method, "shutdown")) {
            try writeResult(out, id, null);
            return true;
        }
        if (std.mem.startsWith(u8, method, "hook/")) {
            const name = method["hook/".len..];
            if (self.findHook(name)) |handler| {
                const hook_resp = handler(self.ctx, self.allocator, params) catch |err| HookResponse{
                    .result = .reject,
                    .message = @errorName(err),
                };
                try writeHook(out, id, hook_resp);
            } else {
                try writeRawResult(out, id, "{\"result\":\"continue\"}");
            }
            return false;
        }
        if (self.findRpc(method)) |handler| {
            const result = handler(self.ctx, self.allocator, params) catch |err| {
                try writeError(out, id, -32000, @errorName(err));
                return false;
            };
            try writeResult(out, id, result);
            return false;
        }
        try writeError(out, id, -32601, "method not found");
        return false;
    }

    fn writeManifest(self: *Plugin, out: *std.Io.Writer, id: std.json.Value) !void {
        var body: std.Io.Writer.Allocating = .init(self.allocator);
        defer body.deinit();
        var jw: std.json.Stringify = .{ .writer = &body.writer };
        try jw.beginObject();
        try jw.objectField("jsonrpc");
        try jw.write("2.0");
        try jw.objectField("id");
        try jw.write(id);
        try jw.objectField("result");
        try jw.beginObject();

        try jw.objectField("rpc_methods");
        try jw.beginArray();
        for (self.rpc_methods.items) |decl| {
            try jw.beginObject();
            try jw.objectField("name");
            try jw.write(decl.name);
            try jw.objectField("description");
            try jw.write(decl.description);
            try jw.objectField("usage");
            try jw.write(decl.usage);
            try jw.endObject();
        }
        try jw.endArray();

        try jw.objectField("subscriptions");
        try jw.beginArray();
        for (self.subscriptions.items) |decl| try jw.write(decl.topic);
        try jw.endArray();

        try jw.objectField("hooks");
        try jw.beginArray();
        for (self.hooks.items) |decl| {
            try jw.beginObject();
            try jw.objectField("name");
            try jw.write(decl.name);
            try jw.objectField("before");
            try jw.beginArray();
            try jw.endArray();
            try jw.objectField("after");
            try jw.beginArray();
            try jw.endArray();
            try jw.endObject();
        }
        try jw.endArray();

        try jw.objectField("options");
        try jw.beginArray();
        for (self.options.items) |decl| {
            try jw.beginObject();
            try jw.objectField("name");
            try jw.write(decl.name);
            try jw.objectField("type");
            try jw.write(decl.opt_type.wire());
            try jw.objectField("description");
            try jw.write(decl.description);
            try jw.endObject();
        }
        try jw.endArray();

        try jw.objectField("dynamic");
        try jw.write(self.dynamic);
        try jw.objectField("failure_mode");
        try jw.write(self.failure_mode.wire());
        try jw.objectField("important");
        try jw.write(self.important);
        try jw.endObject();
        try jw.endObject();

        try out.writeAll(body.written());
        try out.writeByte('\n');
    }

    fn writeInit(self: *Plugin, out: *std.Io.Writer, id: std.json.Value, params: std.json.Value) !void {
        if (self.on_init) |handler| {
            handler(self.ctx, self.allocator, params) catch |err| {
                try writeDisable(out, id, @errorName(err));
                return;
            };
        }
        try writeResult(out, id, null);
    }

    fn findRpc(self: *Plugin, name: []const u8) ?RpcHandler {
        for (self.rpc_methods.items) |decl| {
            if (std.mem.eql(u8, decl.name, name)) return decl.handler;
        }
        return null;
    }

    fn findHook(self: *Plugin, name: []const u8) ?HookHandler {
        for (self.hooks.items) |decl| {
            if (std.mem.eql(u8, decl.name, name)) return decl.handler;
        }
        return null;
    }

    fn findNotify(self: *Plugin, topic: []const u8) ?NotifyHandler {
        for (self.subscriptions.items) |decl| {
            if (std.mem.eql(u8, decl.topic, topic) or std.mem.eql(u8, decl.topic, "*")) {
                return decl.handler;
            }
        }
        return null;
    }
};

fn hasListenFlag(proc: std.process.Init) bool {
    var it = proc.minimal.args.iterate();
    while (it.next()) |arg| {
        if (std.mem.eql(u8, arg, "--lampo-listen")) return true;
        if (std.mem.startsWith(u8, arg, "--lampo-listen=")) return true;
    }
    return false;
}

fn writeResult(out: *std.Io.Writer, id: std.json.Value, result: ?std.json.Value) !void {
    var jw: std.json.Stringify = .{ .writer = out };
    try jw.beginObject();
    try jw.objectField("jsonrpc");
    try jw.write("2.0");
    try jw.objectField("id");
    try jw.write(id);
    try jw.objectField("result");
    if (result) |value| {
        try jw.write(value);
    } else {
        try jw.beginObject();
        try jw.endObject();
    }
    try jw.endObject();
    try out.writeByte('\n');
}

fn writeRawResult(out: *std.Io.Writer, id: std.json.Value, result_json: []const u8) !void {
    var jw: std.json.Stringify = .{ .writer = out };
    try jw.beginObject();
    try jw.objectField("jsonrpc");
    try jw.write("2.0");
    try jw.objectField("id");
    try jw.write(id);
    try jw.objectField("result");
    try jw.beginWriteRaw();
    try out.writeAll(result_json);
    jw.endWriteRaw();
    try jw.endObject();
    try out.writeByte('\n');
}

fn writeDisable(out: *std.Io.Writer, id: std.json.Value, reason: []const u8) !void {
    var jw: std.json.Stringify = .{ .writer = out };
    try jw.beginObject();
    try jw.objectField("jsonrpc");
    try jw.write("2.0");
    try jw.objectField("id");
    try jw.write(id);
    try jw.objectField("result");
    try jw.beginObject();
    try jw.objectField("disable");
    try jw.write(reason);
    try jw.endObject();
    try jw.endObject();
    try out.writeByte('\n');
}

fn writeError(out: *std.Io.Writer, id: std.json.Value, code: i32, message: []const u8) !void {
    var jw: std.json.Stringify = .{ .writer = out };
    try jw.beginObject();
    try jw.objectField("jsonrpc");
    try jw.write("2.0");
    try jw.objectField("id");
    try jw.write(id);
    try jw.objectField("error");
    try jw.beginObject();
    try jw.objectField("code");
    try jw.write(code);
    try jw.objectField("message");
    try jw.write(message);
    try jw.endObject();
    try jw.endObject();
    try out.writeByte('\n');
}

fn writeHook(out: *std.Io.Writer, id: std.json.Value, hook_resp: HookResponse) !void {
    var jw: std.json.Stringify = .{ .writer = out };
    try jw.beginObject();
    try jw.objectField("jsonrpc");
    try jw.write("2.0");
    try jw.objectField("id");
    try jw.write(id);
    try jw.objectField("result");
    try jw.beginObject();
    try jw.objectField("result");
    try jw.write(switch (hook_resp.result) {
        .@"continue" => "continue",
        .complete => "complete",
        .reject => "reject",
    });
    switch (hook_resp.result) {
        .@"continue", .complete => if (hook_resp.payload) |payload| {
            try jw.objectField("payload");
            try jw.write(payload);
        },
        .reject => {
            try jw.objectField("message");
            try jw.write(hook_resp.message);
        },
    }
    try jw.endObject();
    try jw.endObject();
    try out.writeByte('\n');
}

/// Object field lookup. Returns null when `value` is not an object or the key is missing.
pub fn objectGet(value: std.json.Value, key: []const u8) ?std.json.Value {
    return switch (value) {
        .object => |obj| obj.get(key),
        else => null,
    };
}

pub fn objectGetString(value: std.json.Value, key: []const u8) ?[]const u8 {
    const field = objectGet(value, key) orelse return null;
    return switch (field) {
        .string => |s| s,
        else => null,
    };
}

/// `params` must be a JSON array, or an empty object (the daemon's "no args").
/// `params` must be a JSON array. An empty object is the daemon's "no args"
/// only for methods that take none; callers that always expect an array
/// should use this and treat a missing field as empty.
pub fn rpcArray(params: std.json.Value) error{BadParams}![]const std.json.Value {
    return switch (params) {
        .array => |items| items.items,
        .null => &.{},
        else => error.BadParams,
    };
}

test "manifest lists rpc methods and important" {
    const gpa = std.testing.allocator;
    var plugin = Plugin.init(gpa);
    defer plugin.deinit();
    plugin.setImportant(true);
    plugin.setFailureMode(.fail_closed);
    try plugin.rpcMethod("getblock", "Fetch a block by hash", "hash verbosity", struct {
        fn handle(_: *anyopaque, _: std.mem.Allocator, _: std.json.Value) !std.json.Value {
            return .null;
        }
    }.handle);

    var body: std.Io.Writer.Allocating = .init(gpa);
    defer body.deinit();
    try plugin.writeManifest(&body.writer, .{ .integer = 1 });
    const text = body.written();
    try std.testing.expect(std.mem.indexOf(u8, text, "\"name\":\"getblock\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, text, "\"important\":true") != null);
    try std.testing.expect(std.mem.indexOf(u8, text, "\"failure_mode\":\"fail_closed\"") != null);
    try std.testing.expect(std.mem.endsWith(u8, text, "\n"));
}

test "rpcArray accepts an array and null" {
    const gpa = std.testing.allocator;
    var parsed = try std.json.parseFromSlice(std.json.Value, gpa, "[\"aa\",0]", .{});
    defer parsed.deinit();
    const args = try rpcArray(parsed.value);
    try std.testing.expectEqual(@as(usize, 2), args.len);

    try std.testing.expectEqual(@as(usize, 0), (try rpcArray(.null)).len);
    var obj = try std.json.parseFromSlice(std.json.Value, gpa, "{}", .{});
    defer obj.deinit();
    try std.testing.expectError(error.BadParams, rpcArray(obj.value));
}
