const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // Zrpc does not export a module from its package, and it imports itself so
    // generated code can say `@import("zrpc")`. Rebuild that graph here.
    const zrpc_dep = b.dependency("zrpc", .{
        .target = target,
        .optimize = optimize,
    });
    const zinet_dep = zrpc_dep.builder.dependency("zinet", .{
        .target = target,
        .optimize = optimize,
    });
    const zrpc_mod = b.createModule(.{
        .root_source_file = zrpc_dep.path("src/root.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "zinet", .module = zinet_dep.module("zinet") },
        },
    });
    zrpc_mod.addImport("zrpc", zrpc_mod);

    const sdk = b.addModule("lampo_plugin_sdk", .{
        .root_source_file = b.path("src/root.zig"),
        .target = target,
        .optimize = optimize,
        .imports = &.{
            .{ .name = "zrpc", .module = zrpc_mod },
        },
    });

    const tests = b.addTest(.{ .root_module = sdk });
    const test_step = b.step("test", "Run plugin SDK tests");
    test_step.dependOn(&b.addRunArtifact(tests).step);

    const hello = b.option(bool, "example", "Also build the hello example") orelse false;
    if (hello) {
        const exe = b.addExecutable(.{
            .name = "hello",
            .root_module = b.createModule(.{
                .root_source_file = b.path("src/main.zig"),
                .target = target,
                .optimize = optimize,
                .imports = &.{
                    .{ .name = "lampo_plugin_sdk", .module = sdk },
                },
            }),
        });
        b.installArtifact(exe);
    }
}
