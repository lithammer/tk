//! `tk manpage` — print or install the embedded `tk(1)` manpage.
//!
//! The manpage is embedded at compile time via `@embedFile` so the binary
//! ships a single source of truth for `tk(1)`. Default behavior writes the
//! bytes to stdout; `--install` copies them next to the running executable
//! under `<exe-dir>/../share/man/man1/tk.1` using an atomic stage-and-rename
//! that never deletes an existing target on failure.

const std = @import("std");
const builtin = @import("builtin");
const clap = @import("clap");
const cli = @import("../cli.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");
const embed = @import("../embed.zig");
const messages = @import("../messages.zig");

// The manpage source lives at `man/tk.1` in the repository root, outside the
// `src/` package boundary set by `build.zig`. The build wires it in as an
// anonymous module under the name `manpage_data` so `@embedFile` can resolve
// it without per-call-site `..` path tricks.
const manpage_bytes = @embedFile("manpage_data");
comptime {
    embed.assertNoCR(manpage_bytes);
}

/// Dispatcher metadata for `tk manpage`.
pub const meta: cli.CommandMeta = .{
    .name = "manpage",
    .description = "Print or install the tk manpage",
};

const params = clap.parseParamsComptime(
    \\-h, --help     Display this help and exit.
    \\    --install  Install the manpage next to the running tk binary.
    \\
);

/// Parse `tk manpage` flags and dispatch to print or install.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    var res = (try parse_diagnostic.parseOrReportUsage(clap.Help, &params, clap.parsers.default, args_iter, .{
        .stderr = deps.stderr,
        .allocator = deps.gpa,
        .command = .{ .subcommand = meta.name },
    })) orelse return 2;
    defer res.deinit();

    if (res.args.help != 0) {
        writeHelp(deps) catch {};
        return 0;
    }

    if (res.args.install != 0) return installManpage(deps);

    try deps.stdout.writeAll(manpage_bytes);
    return 0;
}

/// Install the embedded manpage at `<exe-dir>/../share/man/man1/tk.1`.
///
/// The install never deletes an existing target on failure; on a rename
/// error the staged tmp file is best-effort removed but the destination
/// is left untouched. On Windows the install is a documented no-op so
/// scripted post-install steps do not fail.
fn installManpage(deps: cli.Deps) !u8 {
    if (builtin.os.tag == .windows) {
        deps.stderr.writeAll(messages.manpage_skip_windows ++ "\n") catch {};
        return 0;
    }

    var exe_buf: [std.Io.Dir.max_path_bytes]u8 = undefined;
    const exe_len = std.process.executablePath(deps.io, &exe_buf) catch |err| {
        renderExeResolveFailure(deps, @errorName(err));
        return 1;
    };
    const exe_dir = std.fs.path.dirname(exe_buf[0..exe_len]) orelse {
        renderExeResolveFailure(deps, "executable path has no parent directory");
        return 1;
    };

    const target_dir_path = try std.fs.path.join(deps.gpa, &.{ exe_dir, "..", "share", "man", "man1" });
    defer deps.gpa.free(target_dir_path);

    const target_path = try std.fs.path.join(deps.gpa, &.{ target_dir_path, "tk.1" });
    defer deps.gpa.free(target_path);

    std.Io.Dir.cwd().createDirPath(deps.io, target_dir_path) catch |err| {
        renderInstallFailure(deps, target_path, @errorName(err));
        return 1;
    };

    var target_dir = std.Io.Dir.cwd().openDir(deps.io, target_dir_path, .{}) catch |err| {
        renderInstallFailure(deps, target_path, @errorName(err));
        return 1;
    };
    defer target_dir.close(deps.io);

    // Stage-and-rename happens inside the target directory so the rename
    // stays within one filesystem (no EXDEV). The suffix uses 64 random
    // bits from `deps.random` to keep concurrent installs from colliding;
    // the file is unlinked on success and best-effort on failure.
    var stage_name_buf: [32]u8 = undefined;
    const stage_name = renderStageName(&stage_name_buf, deps.random);

    {
        var file = target_dir.createFile(deps.io, stage_name, .{ .exclusive = true }) catch |err| {
            renderInstallFailure(deps, target_path, @errorName(err));
            return 1;
        };
        defer file.close(deps.io);
        file.writeStreamingAll(deps.io, manpage_bytes) catch |err| {
            target_dir.deleteFile(deps.io, stage_name) catch {};
            renderInstallFailure(deps, target_path, @errorName(err));
            return 1;
        };
    }

    std.Io.Dir.rename(target_dir, stage_name, target_dir, "tk.1", deps.io) catch |err| {
        target_dir.deleteFile(deps.io, stage_name) catch {};
        renderInstallFailure(deps, target_path, @errorName(err));
        return 1;
    };

    deps.stdout.print(messages.manpage_install_success ++ "{s}\n", .{target_path}) catch {};
    return 0;
}

/// Render a target-path install failure line. Centralizes the messages.zig
/// prefix and suffix so every install-path branch that reached a concrete
/// target path emits the same shape, keeping the "left unchanged" contract
/// honest.
fn renderInstallFailure(deps: cli.Deps, path: []const u8, reason: []const u8) void {
    deps.stderr.print(
        messages.manpage_install_failure_prefix ++ "{s}: {s}" ++ messages.manpage_install_failure_suffix ++ "\n",
        .{ path, reason },
    ) catch {};
}

/// Render a pre-target install failure line. Used for failures that happen
/// before any target path has been computed (executable-path resolution
/// failure, no parent directory). Deliberately does NOT include the
/// "left unchanged" suffix because no target was identified to leave alone.
fn renderExeResolveFailure(deps: cli.Deps, reason: []const u8) void {
    deps.stderr.print(
        messages.manpage_install_exe_resolve_failure_prefix ++ "{s}\n",
        .{reason},
    ) catch {};
}

/// Fixed prefix of `renderStageName`'s output; shared with the unit test
/// so a future tweak to the stage-filename shape stays lock-step.
const stage_name_prefix = ".tk.1.tmp.";

/// Build the hex-suffixed staged filename used by `tk manpage --install`.
/// Returns a slice into `buf`. The 16-byte hex suffix (64 random bits)
/// makes concurrent installs collision-free without needing pid sniffing.
fn renderStageName(buf: *[32]u8, random: std.Random) []const u8 {
    var rand_bytes: [8]u8 = undefined;
    random.bytes(&rand_bytes);
    const hex = std.fmt.bytesToHex(rand_bytes, .lower);
    return std.fmt.bufPrint(buf, stage_name_prefix ++ "{s}", .{hex[0..]}) catch unreachable;
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk manpage - print or install the tk manpage
        \\
        \\Without flags, prints the embedded tk(1) manpage source to stdout
        \\so it can be piped into `man -l -` or written to a file by the
        \\caller. With `--install`, copies the embedded manpage to
        \\`<exe-dir>/../share/man/man1/tk.1` using an atomic stage-and-rename
        \\that never deletes an existing target on failure.
        \\
        \\Usage:
        \\  tk manpage [options]
        \\
        \\Options:
        \\
    );
    try clap.help(deps.stdout, clap.Help, &params, .{
        .description_on_new_line = false,
        .description_indent = 2,
        .indent = 2,
        .spacing_between_parameters = 0,
    });
}

const Harness = @import("../testing/test_cli.zig").Harness;

test "manpage writes the embedded manpage bytes to stdout" {
    var h = Harness.init(std.testing.allocator, &.{});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expectEqualStrings(manpage_bytes, h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "manpage rejects an unknown flag" {
    var h = Harness.init(std.testing.allocator, &.{"--bad-flag"});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(h.stderr().len > 0);
}

test "manpage --help prints help to stdout and exits 0" {
    var h = Harness.init(std.testing.allocator, &.{"--help"});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "tk manpage") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Usage:") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--help") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--install") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "renderStageName produces a hex-suffixed name with a stable prefix" {
    var prng = std.Random.DefaultPrng.init(0);
    var buf: [32]u8 = undefined;
    const name = renderStageName(&buf, prng.random());
    try std.testing.expect(std.mem.startsWith(u8, name, stage_name_prefix));
    try std.testing.expectEqual(stage_name_prefix.len + 16, name.len);
    for (name[stage_name_prefix.len..]) |b| {
        try std.testing.expect((b >= '0' and b <= '9') or (b >= 'a' and b <= 'f'));
    }
}

test "manpage --install renders success or a structured diagnostic" {
    // `std.process.executablePath` is not injectable through Deps, so this
    // test runs against the real test-binary path: writes land inside
    // `.zig-cache/o/share/man/man1/tk.1` next to the test binary, which is
    // a build artifact directory cleaned by `zig build clean`. The test
    // pins three properties that hold regardless of where the test runner
    // lives:
    //
    //   1. The command exits 0 (success) or 1 (well-formed failure).
    //   2. Success prints "Installed manpage at " on stdout.
    //   3. Failure prints the structured prefix on stderr; the existing
    //      target is left unchanged language is part of the contract.
    //
    // This is the narrowest layer that observes the install plumbing
    // without faking the executable-path resolution.
    if (builtin.os.tag == .windows) return;

    var h = Harness.init(std.testing.allocator, &.{"--install"});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    switch (code) {
        0 => {
            try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.manpage_install_success) != null);
            try std.testing.expectEqualStrings("", h.stderr());
        },
        1 => {
            try std.testing.expectEqualStrings("", h.stdout());
            try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.manpage_install_failure_prefix) != null);
            try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "existing file (if any) left unchanged") != null);
        },
        else => return error.UnexpectedExitCode,
    }
}

test "manpage --install writes the embedded bytes to the resolved target" {
    // Stronger sibling of the previous test: when the install succeeds, the
    // file at the target path must contain exactly `manpage_bytes`. This
    // pins the stage-and-rename plumbing — a regression that wrote the
    // wrong content (e.g. an off-by-one slice) would slip past a substring
    // check on the stdout message.
    if (builtin.os.tag == .windows) return;

    var h = Harness.init(std.testing.allocator, &.{"--install"});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    if (code != 0) return; // covered by the previous test's failure branch

    const stdout = h.stdout();
    const prefix = messages.manpage_install_success;
    const start = (std.mem.indexOf(u8, stdout, prefix) orelse return error.MissingPrefix) + prefix.len;
    const end = std.mem.indexOfScalarPos(u8, stdout, start, '\n') orelse return error.MissingNewline;
    const target_path = stdout[start..end];

    const installed = try std.Io.Dir.cwd().readFileAlloc(
        std.testing.io,
        target_path,
        std.testing.allocator,
        .unlimited,
    );
    defer std.testing.allocator.free(installed);
    try std.testing.expectEqualStrings(manpage_bytes, installed);
}

test "manpage --install is a no-op on Windows" {
    if (builtin.os.tag != .windows) return;

    var h = Harness.init(std.testing.allocator, &.{"--install"});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.manpage_skip_windows ++ "\n", h.stderr());
}
