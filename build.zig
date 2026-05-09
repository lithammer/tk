const std = @import("std");

/// Build the `tk` executable and the unified test binary.
///
/// Command-owned static assets are embedded by relative path from their owning
/// Zig module, so missing assets fail at build time without extra build wiring.
pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const clap = b.dependency("clap", .{
        .target = target,
        .optimize = optimize,
    });

    // The Repository Store uses zqlite as a typed wrapper over SQLite.
    // zqlite vendors and statically links its own SQLite 3.53.0 amalgamation
    // when its build.zig is invoked as a dependency, so the tk binary
    // links exactly one copy of SQLite shared between exe and tests.
    const zqlite_dep = b.dependency("zqlite", .{
        .target = target,
        .optimize = optimize,
        .sqlite3 = @as([]const []const u8, &.{
            "-std=c99",
            "-DSQLITE_THREADSAFE=2",
            "-DSQLITE_DQS=0",
            "-DSQLITE_DEFAULT_MEMSTATUS=0",
            "-DSQLITE_OMIT_DEPRECATED",
            "-DSQLITE_OMIT_SHARED_CACHE",
        }),
    });
    const zqlite_module = zqlite_dep.module("zqlite");

    const root_mod = b.createModule(.{
        .root_source_file = b.path("src/main.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    root_mod.addImport("clap", clap.module("clap"));
    root_mod.addImport("zqlite", zqlite_module);

    const exe = b.addExecutable(.{
        .name = "tk",
        .root_module = root_mod,
    });
    b.installArtifact(exe);

    const run_cmd = b.addRunArtifact(exe);
    run_cmd.step.dependOn(b.getInstallStep());
    if (b.args) |args| {
        run_cmd.addArgs(args);
    }
    const run_step = b.step("run", "Run tk");
    run_step.dependOn(&run_cmd.step);

    const test_mod = b.createModule(.{
        .root_source_file = b.path("src/test_root.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    test_mod.addImport("clap", clap.module("clap"));
    test_mod.addImport("zqlite", zqlite_module);

    const test_options = b.addOptions();
    test_options.addOptionPath("tk_exe_path", exe.getEmittedBin());
    test_mod.addOptions("build_options", test_options);

    const unit_tests = b.addTest(.{
        .root_module = test_mod,
    });

    const run_unit_tests = b.addRunArtifact(unit_tests);

    const test_step = b.step("test", "Run unit tests");
    test_step.dependOn(&run_unit_tests.step);
}
