//! Hello-world plugin. Proves the stdio handshake without talking to bitcoind.
//!
//! ```sh
//! zig build -Dexample
//! printf '%s\n' \
//!   '{"jsonrpc":"2.0","id":1,"method":"getmanifest","params":{}}' \
//!   '{"jsonrpc":"2.0","id":2,"method":"init","params":{}}' \
//!   '{"jsonrpc":"2.0","id":3,"method":"hello","params":{"name":"lampo"}}' \
//!   | zig-out/bin/hello
//! ```

const std = @import("std");
const sdk = @import("lampo_plugin_sdk");

const State = struct {
    greeting: []u8 = &.{},
    allocator: std.mem.Allocator,

    fn deinit(self: *State) void {
        if (self.greeting.len != 0) self.allocator.free(self.greeting);
    }
};

fn hello(ctx: *anyopaque, allocator: std.mem.Allocator, params: std.json.Value) !std.json.Value {
    const state: *State = @ptrCast(@alignCast(ctx));
    if (state.greeting.len != 0) allocator.free(state.greeting);
    const name = sdk.objectGetString(params, "name") orelse "world";
    state.greeting = try std.fmt.allocPrint(allocator, "hello, {s}!", .{name});
    return .{ .string = state.greeting };
}

pub fn main(init: std.process.Init) !void {
    var state = State{ .allocator = init.gpa };
    defer state.deinit();

    var plugin = sdk.Plugin.init(init.gpa);
    defer plugin.deinit();
    plugin.setContext(@ptrCast(&state));
    plugin.setDynamic(true);
    try plugin.rpcMethod("hello", "Says hello", "[name]", hello);
    try plugin.run(init);
}
