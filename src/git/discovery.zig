//! Git rev-parse path discovery used to locate the Repository Store.
//!
//! `tk init` (and future `tk worktree` commands) need Git's common directory
//! and toplevel to place or find `<git-common-dir>/tk/ticket.db` (the
//! Repository Store, per ARCHITECTURE.md). This module owns the
//! `git rev-parse` invocation and returns a typed `Outcome` so callers can
//! render diagnostics without this module reaching for stderr.

const std = @import("std");
const proc = @import("../proc/runner.zig");
const messages = @import("../messages.zig");

const Allocator = std.mem.Allocator;

/// Paths reported by Git for the current repository.
///
/// Both fields are heap-allocated by `discoverPaths` from the caller-supplied
/// allocator. Callers extract this from `Outcome.ok` and free it via
/// `DiscoveredPaths.deinit`.
pub const DiscoveredPaths = struct {
    /// Shared Git common directory; used as the parent of the Repository Store.
    git_common_dir: []u8,
    /// Repository working-tree root; used only for display_prefix derivation.
    toplevel: []u8,

    pub fn deinit(self: *DiscoveredPaths, gpa: Allocator) void {
        gpa.free(self.git_common_dir);
        gpa.free(self.toplevel);
    }
};

/// Outcome of `discoverPaths`. Each variant maps to a stable, caller-rendered
/// stderr message; this module never writes to stderr itself.
///
/// Ownership: each switch arm frees its own payload. `.ok` carries a
/// `DiscoveredPaths` whose two slices live on the caller's allocator and
/// must be released via `DiscoveredPaths.deinit`. `.git_rejected` may carry
/// an owned trimmed-stderr slice (also on the caller's allocator) which the
/// caller must `gpa.free` after rendering. The remaining variants carry no
/// payload. There is no `Outcome.deinit`; per-arm freeing keeps test and
/// production call sites identical.
pub const Outcome = union(enum) {
    ok: DiscoveredPaths,
    /// `git` is not on PATH (proc.Runner returned ExecutableNotFound).
    git_missing,
    /// `git` could not be spawned for a reason other than missing binary.
    spawn_failed,
    /// `git rev-parse` exited non-zero. Payload is the trimmed stderr if Git
    /// produced any (caller renders it verbatim and frees the slice); null
    /// means Git failed silently and the caller should render the default
    /// not-in-repo message.
    git_rejected: ?[]u8,
    /// `git rev-parse` exited zero but the stdout did not contain both
    /// expected lines. The "missing toplevel" sub-case is unreachable in
    /// practice because `--show-toplevel` always pairs with
    /// `--git-common-dir` when the repo is a worktree; the variant covers
    /// the missing-common-dir branch and serves as a defensive fallback.
    git_output_unparseable,
};

/// Errors returned by `discoverPaths` itself. Runner errors are mapped to
/// `Outcome` variants; only `OutOfMemory` from the allocator escapes.
pub const Error = error{OutOfMemory};

/// Run `git rev-parse --path-format=absolute --git-common-dir --show-toplevel`
/// through the supplied runner and classify the result.
pub fn discoverPaths(gpa: Allocator, runner: proc.Runner, cwd: std.Io.Dir) Error!Outcome {
    var run_result = runner.run(gpa, .{
        .argv = &.{ "git", "rev-parse", "--path-format=absolute", "--git-common-dir", "--show-toplevel" },
        .cwd = cwd,
    }) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.ExecutableNotFound => return .git_missing,
        error.SpawnFailed => return .spawn_failed,
    };
    defer run_result.deinit(gpa);

    if (run_result.exit_code != 0) {
        const trimmed = std.mem.trim(u8, run_result.stderr, " \t\r\n");
        if (trimmed.len == 0) return .{ .git_rejected = null };
        const owned = try gpa.dupe(u8, trimmed);
        return .{ .git_rejected = owned };
    }

    var lines = std.mem.tokenizeScalar(u8, run_result.stdout, '\n');
    const common = lines.next() orelse return .git_output_unparseable;
    const toplevel = lines.next() orelse return .git_output_unparseable;

    const common_owned = try gpa.dupe(u8, std.mem.trim(u8, common, " \t\r\n"));
    errdefer gpa.free(common_owned);
    const toplevel_owned = try gpa.dupe(u8, std.mem.trim(u8, toplevel, " \t\r\n"));
    errdefer gpa.free(toplevel_owned);

    return .{ .ok = .{ .git_common_dir = common_owned, .toplevel = toplevel_owned } };
}

/// Render an Outcome failure to stderr with a `tk <command>: ` prefix and free
/// any heap-owned payload it carries. Callers branch on `.ok` themselves
/// before delegating here; this helper exists to keep the four discovery
/// failure arms identical across every command that opens a Repository Store.
///
/// `command` is the bare subcommand name (`"init"`, `"add"`), without the `tk`
/// or the trailing colon — those are formatted in.
pub fn renderFailure(
    stderr: *std.Io.Writer,
    gpa: Allocator,
    command: []const u8,
    outcome: Outcome,
) void {
    switch (outcome) {
        .ok => unreachable,
        .git_missing => stderr.print("tk {s}: {s}\n", .{ command, messages.init_git_missing }) catch {},
        .spawn_failed => stderr.print("tk {s}: {s}\n", .{ command, messages.init_git_spawn_failed }) catch {},
        .git_rejected => |maybe_msg| {
            if (maybe_msg) |msg| {
                defer gpa.free(msg);
                stderr.print("tk {s}: {s}\n", .{ command, msg }) catch {};
            } else {
                stderr.print("tk {s}: {s}\n", .{ command, messages.init_outside_git_default }) catch {};
            }
        },
        .git_output_unparseable => stderr.print("tk {s}: {s}\n", .{ command, messages.init_git_unparseable }) catch {},
    }
}

// ---- Tests ---------------------------------------------------------------

const fake_proc = @import("../proc/fake.zig");

test "discoverPaths: returns .ok with both paths on success" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "rev-parse" }, .{
        .exit_code = 0,
        .stdout = "/repo/.git\n/repo\n",
    });

    const outcome = try discoverPaths(gpa, fake.runner(), std.Io.Dir.cwd());
    if (outcome != .ok) return error.UnexpectedOutcome;
    var paths = outcome.ok;
    defer paths.deinit(gpa);
    try std.testing.expectEqualStrings("/repo/.git", paths.git_common_dir);
    try std.testing.expectEqualStrings("/repo", paths.toplevel);
}

test "discoverPaths: returns .git_rejected with trimmed stderr on exit 128" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "rev-parse" }, .{
        .exit_code = 128,
        .stderr = "fatal: not a git repository (or any of the parent directories): .git\n",
    });

    const outcome = try discoverPaths(gpa, fake.runner(), std.Io.Dir.cwd());
    switch (outcome) {
        .git_rejected => |maybe_msg| {
            const msg = maybe_msg orelse return error.ExpectedNonNullMessage;
            defer gpa.free(msg);
            // Pin the exact trimmed value: trailing newline stripped, no
            // other modifications. Drift in trim semantics fails here.
            try std.testing.expectEqualStrings(
                "fatal: not a git repository (or any of the parent directories): .git",
                msg,
            );
        },
        else => return error.UnexpectedOutcome,
    }
}

test "discoverPaths: returns .git_rejected with null payload when stderr is whitespace" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 128, .stderr = "  \r\n  " });

    const outcome = try discoverPaths(gpa, fake.runner(), std.Io.Dir.cwd());
    switch (outcome) {
        .git_rejected => |maybe_msg| try std.testing.expect(maybe_msg == null),
        else => return error.UnexpectedOutcome,
    }
}

test "discoverPaths: returns .git_output_unparseable when stdout has no lines" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = "" });

    const outcome = try discoverPaths(gpa, fake.runner(), std.Io.Dir.cwd());
    try std.testing.expect(outcome == .git_output_unparseable);
}

test "discoverPaths: returns .git_output_unparseable when stdout has only one line" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = "/repo/.git\n" });

    const outcome = try discoverPaths(gpa, fake.runner(), std.Io.Dir.cwd());
    try std.testing.expect(outcome == .git_output_unparseable);
}

test "discoverPaths: maps ExecutableNotFound to .git_missing" {
    const gpa = std.testing.allocator;
    var injector = fake_proc.ErrorInjectingRunner{ .err = error.ExecutableNotFound };

    const outcome = try discoverPaths(gpa, injector.runner(), std.Io.Dir.cwd());
    try std.testing.expect(outcome == .git_missing);
}

test "discoverPaths: maps SpawnFailed to .spawn_failed" {
    const gpa = std.testing.allocator;
    var injector = fake_proc.ErrorInjectingRunner{ .err = error.SpawnFailed };

    const outcome = try discoverPaths(gpa, injector.runner(), std.Io.Dir.cwd());
    try std.testing.expect(outcome == .spawn_failed);
}

test "discoverPaths: propagates OutOfMemory from runner" {
    var injector = fake_proc.ErrorInjectingRunner{ .err = error.OutOfMemory };

    try std.testing.expectError(
        error.OutOfMemory,
        discoverPaths(std.testing.allocator, injector.runner(), std.Io.Dir.cwd()),
    );
}

test "renderFailure: formats `tk <command>: <fragment>` for each failure arm" {
    const gpa = std.testing.allocator;
    var buf: std.Io.Writer.Allocating = .init(gpa);
    defer buf.deinit();

    renderFailure(&buf.writer, gpa, "init", .git_missing);
    try std.testing.expectEqualStrings("tk init: " ++ messages.init_git_missing ++ "\n", buf.written());

    buf.clearRetainingCapacity();
    renderFailure(&buf.writer, gpa, "add", .spawn_failed);
    try std.testing.expectEqualStrings("tk add: " ++ messages.init_git_spawn_failed ++ "\n", buf.written());

    buf.clearRetainingCapacity();
    renderFailure(&buf.writer, gpa, "init", .git_output_unparseable);
    try std.testing.expectEqualStrings("tk init: " ++ messages.init_git_unparseable ++ "\n", buf.written());

    buf.clearRetainingCapacity();
    renderFailure(&buf.writer, gpa, "init", .{ .git_rejected = null });
    try std.testing.expectEqualStrings("tk init: " ++ messages.init_outside_git_default ++ "\n", buf.written());
}

test "renderFailure: frees the git_rejected payload it was handed" {
    // The testing allocator detects leaks, so if renderFailure forgets to
    // free the owned message slice this test fails. The test also pins that
    // the payload is rendered verbatim with the command prefix.
    const gpa = std.testing.allocator;
    var buf: std.Io.Writer.Allocating = .init(gpa);
    defer buf.deinit();

    const owned = try gpa.dupe(u8, "fatal: not a git repository");
    renderFailure(&buf.writer, gpa, "add", .{ .git_rejected = owned });
    try std.testing.expectEqualStrings("tk add: fatal: not a git repository\n", buf.written());
}
