const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const mod = b.addModule("lampo_plugin_sdk", .{
        .root_source_file = b.path("src/root.zig"),
        .target = target,
    });

    const tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/root.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    const run_tests = b.addRunArtifact(tests);
    const test_step = b.step("test", "Run plugin SDK tests");
    test_step.dependOn(&run_tests.step);

    const hello = b.option(bool, "example", "Also build the hello example") orelse false;
    if (hello) {
        const exe = b.addExecutable(.{
            .name = "hello",
            .root_module = b.createModule(.{
                .root_source_file = b.path("src/main.zig"),
                .target = target,
                .optimize = optimize,
                .imports = &.{
                    .{ .name = "lampo_plugin_sdk", .module = mod },
                },
            }),
        });
        b.installArtifact(exe);
    }
}
