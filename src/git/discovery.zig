//! Git rev-parse path discovery used to locate the Repository Store.
//!
//! `tk init` (and future `tk worktree` commands) need Git's common directory
//! and toplevel to place or find `<git-common-dir>/tk/ticket.db` (the
//! Repository Store, per docs/implementation.md). This module owns the
//! `git rev-parse` invocation and returns a typed `Outcome` so callers can
//! render diagnostics without this module reaching for stderr.

const std = @import("std");
const proc = @import("../proc/runner.zig");

const Allocator = std.mem.Allocator;

/// Paths reported by Git for the current repository.
///
/// Both fields are heap-allocated by `discoverPaths` from the caller-supplied
/// allocator. Callers that destructure `Outcome.ok` take ownership of this
/// `DiscoveredPaths` value and must call `deinit` themselves; `Outcome.deinit`
/// is a no-op for the `.ok` arm so the values survive the switch.
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
/// Ownership: `Outcome.deinit` frees only the error-arm payloads. On the
/// `.ok` arm the caller takes ownership of the inner `DiscoveredPaths` and
/// must call `DiscoveredPaths.deinit` themselves after extracting it.
pub const Outcome = union(enum) {
    ok: DiscoveredPaths,
    /// `git` is not on PATH (proc.Runner returned ExecutableNotFound).
    git_missing,
    /// `git` could not be spawned for a reason other than missing binary.
    spawn_failed,
    /// `git rev-parse` exited non-zero. Payload is the trimmed stderr if Git
    /// produced any (caller renders it verbatim); null means Git failed
    /// silently and the caller should render the default not-in-repo message.
    git_rejected: ?[]u8,
    /// `git rev-parse` exited zero but the stdout did not contain both
    /// expected lines. The "missing toplevel" sub-case is unreachable in
    /// practice because `--show-toplevel` always pairs with
    /// `--git-common-dir` when the repo is a worktree; the variant covers
    /// the missing-common-dir branch and serves as a defensive fallback.
    git_output_unparseable,

    pub fn deinit(self: *Outcome, gpa: Allocator) void {
        switch (self.*) {
            .ok => {}, // caller owns the DiscoveredPaths; see doc comment.
            .git_rejected => |maybe_msg| if (maybe_msg) |m| gpa.free(m),
            .git_missing, .spawn_failed, .git_output_unparseable => {},
        }
    }
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

// ---- Tests ---------------------------------------------------------------

const fake_proc = @import("../proc/fake.zig");

/// Minimal hand-rolled runner used to exercise the runner-error branches that
/// `FakeRunner` cannot return today. The argv is ignored; the runner returns
/// the configured error on every call.
const ErrorInjectingRunner = struct {
    err: proc.Error,

    fn runner(self: *ErrorInjectingRunner) proc.Runner {
        return .{ .context = self, .vtable = &vtable };
    }

    const vtable: proc.Runner.VTable = .{ .run = runImpl };

    fn runImpl(context: *anyopaque, gpa: Allocator, options: proc.Options) proc.Error!proc.Result {
        _ = gpa;
        _ = options;
        const self: *ErrorInjectingRunner = @ptrCast(@alignCast(context));
        return self.err;
    }
};

test "discoverPaths: returns .ok with both paths on success" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "rev-parse" }, .{
        .exit_code = 0,
        .stdout = "/repo/.git\n/repo\n",
    });

    // Mirror the production caller's idiom: extract DiscoveredPaths from
    // the .ok arm and free it directly, since Outcome.deinit is a no-op
    // for that arm.
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

    var outcome = try discoverPaths(gpa, fake.runner(), std.Io.Dir.cwd());
    defer outcome.deinit(gpa);
    switch (outcome) {
        .git_rejected => |maybe_msg| {
            const msg = maybe_msg orelse return error.ExpectedNonNullMessage;
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

    var outcome = try discoverPaths(gpa, fake.runner(), std.Io.Dir.cwd());
    defer outcome.deinit(gpa);
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

    var outcome = try discoverPaths(gpa, fake.runner(), std.Io.Dir.cwd());
    defer outcome.deinit(gpa);
    try std.testing.expect(outcome == .git_output_unparseable);
}

test "discoverPaths: returns .git_output_unparseable when stdout has only one line" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = "/repo/.git\n" });

    var outcome = try discoverPaths(gpa, fake.runner(), std.Io.Dir.cwd());
    defer outcome.deinit(gpa);
    try std.testing.expect(outcome == .git_output_unparseable);
}

test "discoverPaths: maps ExecutableNotFound to .git_missing" {
    const gpa = std.testing.allocator;
    var injector = ErrorInjectingRunner{ .err = error.ExecutableNotFound };

    var outcome = try discoverPaths(gpa, injector.runner(), std.Io.Dir.cwd());
    defer outcome.deinit(gpa);
    try std.testing.expect(outcome == .git_missing);
}

test "discoverPaths: maps SpawnFailed to .spawn_failed" {
    const gpa = std.testing.allocator;
    var injector = ErrorInjectingRunner{ .err = error.SpawnFailed };

    var outcome = try discoverPaths(gpa, injector.runner(), std.Io.Dir.cwd());
    defer outcome.deinit(gpa);
    try std.testing.expect(outcome == .spawn_failed);
}

test "discoverPaths: propagates OutOfMemory from runner" {
    var injector = ErrorInjectingRunner{ .err = error.OutOfMemory };

    try std.testing.expectError(
        error.OutOfMemory,
        discoverPaths(std.testing.allocator, injector.runner(), std.Io.Dir.cwd()),
    );
}
