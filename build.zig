const std = @import("std");

/// SQLite amalgamation compile flags shared by every `tk` build. Held at file
/// scope so the regular exe, the test binary, and every release cross-compile
/// see the same SQLite configuration.
const sqlite_cflags: []const []const u8 = &.{
    "-std=c99",
    "-DSQLITE_THREADSAFE=2",
    "-DSQLITE_DQS=0",
    "-DSQLITE_DEFAULT_MEMSTATUS=0",
    "-DSQLITE_OMIT_DEPRECATED",
    "-DSQLITE_OMIT_SHARED_CACHE",
};

/// One row per shipped binary, in the order described by
/// [ADR 0011](docs/adr/0011-single-host-cross-compile-release.md). `linkage`
/// is forced static on musl Linux so the artifact is self-contained; left
/// `null` elsewhere so Zig picks the platform default (dynamic libSystem on
/// macOS — Apple forbids static `libSystem`; default linkage on glibc Linux
/// and windows-gnu).
const ReleaseTarget = struct {
    triple: []const u8,
    query: std.Target.Query,
    linkage: ?std.builtin.LinkMode,
};

const release_targets = [_]ReleaseTarget{
    .{
        .triple = "x86_64-linux-musl",
        .query = .{ .cpu_arch = .x86_64, .os_tag = .linux, .abi = .musl },
        .linkage = .static,
    },
    .{
        .triple = "x86_64-linux-gnu",
        .query = .{
            .cpu_arch = .x86_64,
            .os_tag = .linux,
            .abi = .gnu,
            .glibc_version = .{ .major = 2, .minor = 28, .patch = 0 },
        },
        .linkage = null,
    },
    .{
        .triple = "aarch64-linux-musl",
        .query = .{ .cpu_arch = .aarch64, .os_tag = .linux, .abi = .musl },
        .linkage = .static,
    },
    .{
        .triple = "aarch64-macos",
        .query = .{
            .cpu_arch = .aarch64,
            .os_tag = .macos,
            .os_version_min = .{ .semver = .{ .major = 11, .minor = 0, .patch = 0 } },
        },
        .linkage = null,
    },
    .{
        .triple = "x86_64-windows-gnu",
        .query = .{ .cpu_arch = .x86_64, .os_tag = .windows, .abi = .gnu },
        .linkage = null,
    },
    .{
        .triple = "aarch64-windows-gnu",
        .query = .{ .cpu_arch = .aarch64, .os_tag = .windows, .abi = .gnu },
        .linkage = null,
    },
};

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
        .sqlite3 = sqlite_cflags,
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
    // The manpage source lives at the repository root in man/tk.1, which is
    // outside the package boundary set by root_source_file. Expose it as an
    // anonymous module so commands/manpage.zig can pull it in with
    // `@embedFile("manpage_data")`.
    root_mod.addAnonymousImport("manpage_data", .{ .root_source_file = b.path("man/tk.1") });

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
    test_mod.addAnonymousImport("manpage_data", .{ .root_source_file = b.path("man/tk.1") });

    const test_options = b.addOptions();
    test_options.addOptionPath("tk_exe_path", exe.getEmittedBin());
    test_mod.addOptions("build_options", test_options);

    const unit_tests = b.addTest(.{
        .root_module = test_mod,
    });

    const run_unit_tests = b.addRunArtifact(unit_tests);

    const test_step = b.step("test", "Run unit tests");
    test_step.dependOn(&run_unit_tests.step);

    // Release step: cross-compile tk for every supported triple. Outputs land
    // under `zig-out/release/<triple>/tk[.exe]` so the release workflow can
    // upload one artifact per row. See ADR 0011 for the strategy.
    const release_step = b.step("release", "Cross-compile tk for all release triples");
    const release_version = b.option(
        []const u8,
        "release-version",
        "Version string embedded in release binaries (defaults to \"dev\")",
    ) orelse "dev";

    for (release_targets) |rt| {
        const rt_target = b.resolveTargetQuery(rt.query);
        const rt_optimize: std.builtin.OptimizeMode = .ReleaseSafe;

        const rt_clap = b.dependency("clap", .{
            .target = rt_target,
            .optimize = rt_optimize,
        });
        const rt_zqlite = b.dependency("zqlite", .{
            .target = rt_target,
            .optimize = rt_optimize,
            .sqlite3 = sqlite_cflags,
        });

        const rt_options = b.addOptions();
        rt_options.addOption([]const u8, "version", release_version);

        const rt_mod = b.createModule(.{
            .root_source_file = b.path("src/main.zig"),
            .target = rt_target,
            .optimize = rt_optimize,
            .link_libc = true,
            .strip = true,
        });
        rt_mod.addImport("clap", rt_clap.module("clap"));
        rt_mod.addImport("zqlite", rt_zqlite.module("zqlite"));
        rt_mod.addAnonymousImport("manpage_data", .{ .root_source_file = b.path("man/tk.1") });
        rt_mod.addOptions("build_options", rt_options);

        const rt_exe = b.addExecutable(.{
            .name = "tk",
            .root_module = rt_mod,
            .linkage = rt.linkage,
        });

        const install = b.addInstallArtifact(rt_exe, .{
            .dest_dir = .{ .override = .{ .custom = b.fmt("release/{s}", .{rt.triple}) } },
        });
        release_step.dependOn(&install.step);
    }
}
