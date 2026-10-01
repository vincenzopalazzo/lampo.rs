//! Loopback gRPC server for a lampo plugin, on Zrpc.
//!
//! The daemon spawns this process with `--lampo-listen 127.0.0.1:0`, reads
//! `lampo-listen <addr>` from stdout, and dials that address with tonic.
//! The service is `lampo.plugin.v1.LampoPlugin`. Messages are the flat
//! field lists in `plugin.proto`.

const std = @import("std");
const zrpc = @import("zrpc");
const sdk = @import("root.zig");

const Empty = struct {
    unused: []const u8 = "",
    pub const proto = .{ .unused = 1 };
};

const ManifestRequest = Empty;
const NotifyResponse = Empty;
const ShutdownRequest = Empty;
const ShutdownResponse = Empty;

const ManifestResponse = struct {
    manifest_json: []const u8 = "",
    pub const proto = .{ .manifest_json = 1 };
};

const InitRequest = struct {
    config_json: []const u8 = "",
    pub const proto = .{ .config_json = 1 };
};

const InitResponse = struct {
    disable_message: []const u8 = "",
    pub const proto = .{ .disable_message = 1 };
};

const RpcRequest = struct {
    method: []const u8 = "",
    params_json: []const u8 = "",
    pub const proto = .{ .method = 1, .params_json = 2 };
};

const RpcResponse = struct {
    result_json: []const u8 = "",
    error_message: []const u8 = "",
    error_code: i32 = 0,
    pub const proto = .{ .result_json = 1, .error_message = 2, .error_code = 3 };
};

const HookRequest = struct {
    hook_name: []const u8 = "",
    payload_json: []const u8 = "",
    pub const proto = .{ .hook_name = 1, .payload_json = 2 };
};

const HookResponseMsg = struct {
    response_json: []const u8 = "",
    pub const proto = .{ .response_json = 1 };
};

const NotifyRequest = struct {
    topic: []const u8 = "",
    payload_json: []const u8 = "",
    pub const proto = .{ .topic = 1, .payload_json = 2 };
};

const LampoPlugin = struct {
    plugin: *sdk.Plugin,
    stop: *std.atomic.Value(bool),

    pub const service_name = "lampo.plugin.v1.LampoPlugin";
    pub const methods = .{
        .GetManifest = .{ .kind = .unary, .Request = ManifestRequest, .Response = ManifestResponse },
        .Init = .{ .kind = .unary, .Request = InitRequest, .Response = InitResponse },
        .HandleRpc = .{ .kind = .unary, .Request = RpcRequest, .Response = RpcResponse },
        .HandleHook = .{ .kind = .unary, .Request = HookRequest, .Response = HookResponseMsg },
        .Notify = .{ .kind = .unary, .Request = NotifyRequest, .Response = NotifyResponse },
        .Shutdown = .{ .kind = .unary, .Request = ShutdownRequest, .Response = ShutdownResponse },
    };

    pub fn GetManifest(self: *LampoPlugin, call: *zrpc.Call, _: *const ManifestRequest) !ManifestResponse {
        const json = try manifestJson(self.plugin, call.arena);
        return .{ .manifest_json = json };
    }

    pub fn Init(self: *LampoPlugin, call: *zrpc.Call, request: *const InitRequest) !InitResponse {
        const raw = if (request.config_json.len == 0) "{}" else request.config_json;
        var parsed = std.json.parseFromSlice(std.json.Value, call.arena, raw, .{}) catch {
            return .{ .disable_message = "BadInit" };
        };
        defer parsed.deinit();
        if (self.plugin.on_init) |handler| {
            handler(self.plugin.ctx, self.plugin.allocator, parsed.value) catch |err| {
                return .{ .disable_message = @errorName(err) };
            };
        }
        return .{};
    }

    pub fn HandleRpc(self: *LampoPlugin, call: *zrpc.Call, request: *const RpcRequest) !RpcResponse {
        const raw = if (request.params_json.len == 0) "{}" else request.params_json;
        var parsed = std.json.parseFromSlice(std.json.Value, call.arena, raw, .{}) catch {
            return .{ .error_message = "BadParams", .error_code = -32602 };
        };
        defer parsed.deinit();
        const handler = self.plugin.findRpc(request.method) orelse {
            return .{ .error_message = "method not found", .error_code = -32601 };
        };
        const result = handler(self.plugin.ctx, self.plugin.allocator, parsed.value) catch |err| {
            return .{ .error_message = @errorName(err), .error_code = -32000 };
        };
        var body: std.Io.Writer.Allocating = .init(call.arena);
        var jw: std.json.Stringify = .{ .writer = &body.writer };
        try jw.write(result);
        return .{ .result_json = body.written() };
    }

    pub fn HandleHook(self: *LampoPlugin, call: *zrpc.Call, request: *const HookRequest) !HookResponseMsg {
        var name = request.hook_name;
        if (std.mem.startsWith(u8, name, "hook/")) name = name["hook/".len..];
        const raw = if (request.payload_json.len == 0) "{}" else request.payload_json;
        var parsed = std.json.parseFromSlice(std.json.Value, call.arena, raw, .{}) catch {
            return .{ .response_json = "{\"result\":\"reject\",\"message\":\"BadParams\"}" };
        };
        defer parsed.deinit();
        const hook_resp: sdk.HookResponse = if (self.plugin.findHook(name)) |handler|
            handler(self.plugin.ctx, self.plugin.allocator, parsed.value) catch |err| .{
                .result = .reject,
                .message = @errorName(err),
            }
        else
            .{};
        var body: std.Io.Writer.Allocating = .init(call.arena);
        var jw: std.json.Stringify = .{ .writer = &body.writer };
        try jw.beginObject();
        try jw.objectField("result");
        try jw.write(switch (hook_resp.result) {
            .@"continue" => "continue",
            .complete => "complete",
            .reject => "reject",
        });
        if (hook_resp.result == .reject) {
            try jw.objectField("message");
            try jw.write(hook_resp.message);
        }
        try jw.endObject();
        return .{ .response_json = body.written() };
    }

    pub fn Notify(self: *LampoPlugin, _: *zrpc.Call, request: *const NotifyRequest) !NotifyResponse {
        const raw = if (request.payload_json.len == 0) "{}" else request.payload_json;
        var parsed = std.json.parseFromSlice(std.json.Value, self.plugin.allocator, raw, .{}) catch {
            return .{};
        };
        defer parsed.deinit();
        if (self.plugin.findNotify(request.topic)) |handler| {
            handler(self.plugin.ctx, self.plugin.allocator, parsed.value);
        }
        return .{};
    }

    pub fn Shutdown(self: *LampoPlugin, _: *zrpc.Call, _: *const ShutdownRequest) !ShutdownResponse {
        self.stop.store(true, .release);
        return .{};
    }
};

fn manifestJson(plugin: *sdk.Plugin, arena: std.mem.Allocator) ![]u8 {
    var envelope: std.Io.Writer.Allocating = .init(arena);
    try plugin.writeManifest(&envelope.writer, .null);
    const text = std.mem.trim(u8, envelope.written(), " \t\r\n");
    var parsed = try std.json.parseFromSlice(std.json.Value, arena, text, .{});
    defer parsed.deinit();
    const result = parsed.value.object.get("result") orelse return error.NoManifest;
    var body: std.Io.Writer.Allocating = .init(arena);
    var jw: std.json.Stringify = .{ .writer = &body.writer };
    try jw.write(result);
    return body.written();
}

pub fn runPlugin(plugin: *sdk.Plugin, proc: std.process.Init) !void {
    if (listenFlag(proc) == null) return plugin.runIo(proc.io);

    var stop = std.atomic.Value(bool).init(false);
    var service = LampoPlugin{ .plugin = plugin, .stop = &stop };
    const services = [_]zrpc.server.Registration{zrpc.register(LampoPlugin, &service)};
    const server = try zrpc.Server.listen(.{
        .gpa = proc.gpa,
        .io = proc.io,
        .address = .{ .ip4 = .loopback(0) },
        .services = &services,
    });
    defer server.deinit();
    try server.serve();

    var out_buf: [64]u8 = undefined;
    var stdout = std.Io.File.stdout().writerStreaming(proc.io, &out_buf);
    try stdout.interface.print("lampo-listen 127.0.0.1:{d}\n", .{server.port()});
    try stdout.interface.flush();

    while (!stop.load(.acquire)) {
        proc.io.sleep(.fromMilliseconds(50), .awake) catch break;
    }
    _ = server.shutdownGracefully(.{ .timeout = .fromSeconds(2) });
}

fn listenFlag(proc: std.process.Init) ?[]const u8 {
    var it = proc.minimal.args.iterate();
    while (it.next()) |arg| {
        if (std.mem.eql(u8, arg, "--help") or std.mem.eql(u8, arg, "-h")) {
            std.debug.print("Usage: plugin --lampo-listen 127.0.0.1:0\n", .{});
            std.debug.print("  --lampo-listen <addr>   loopback gRPC address\n", .{});
            std.process.exit(0);
        }
        if (std.mem.eql(u8, arg, "--lampo-listen")) return it.next() orelse "127.0.0.1:0";
        if (std.mem.startsWith(u8, arg, "--lampo-listen=")) return arg["--lampo-listen=".len..];
    }
    return null;
}
