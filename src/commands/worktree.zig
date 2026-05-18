//! `tk worktree` — inspect and configure Workspace Scope.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");

/// Dispatcher metadata for `tk worktree`.
pub const meta: cli.CommandMeta = .{
    .name = "worktree",
    .description = "Inspect or configure the current Workspace Scope",
};

/// Parse `tk worktree` args, dispatch to subcommand or report status.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    // Manual subcommand peek before zig-clap parses anything else, so each
    // arm can own its own parameter shape (`set <id>`, `start <id> [path]
    // [--no-status]`). With no subcommand we fall through to the read-only
    // status report.
    const sub = args_iter.next();
    if (sub) |s| {
        if (std.mem.eql(u8, s, "clear")) return try runClear(deps);
    }
    return 0;
}

fn runClear(deps: cli.Deps) !u8 {
    var result = deps.runner.run(deps.gpa, .{
        .argv = &.{ "git", "config", "--worktree", "--unset", "tk.scope" },
        .cwd = deps.cwd,
    }) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.ExecutableNotFound, error.SpawnFailed => {
            deps.stderr.print("{s}\n{s}\n", .{ messages.worktree_clear_failed, @errorName(err) }) catch {};
            return 1;
        },
    };
    defer result.deinit(deps.gpa);

    // Exit code 5 means the key was already absent; per Q8 that is the
    // idempotent no-op success path. Any other non-zero exit is a real
    // failure worth surfacing.
    if (result.exit_code != 0 and result.exit_code != 5) {
        deps.stderr.print("{s}\n", .{messages.worktree_clear_failed}) catch {};
        return 1;
    }
    try deps.stdout.print("{s}\n", .{messages.worktree_cleared});
    return 0;
}

const Harness = @import("../testing/test_cli.zig").Harness;

test "worktree clear: runs git config --unset and prints success" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--unset", "tk.scope" }, .{ .exit_code = 0 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Workspace Scope cleared\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree clear: exit code 5 from git config --unset is the idempotent no-op" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--unset", "tk.scope" }, .{ .exit_code = 5 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Workspace Scope cleared\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree clear: other non-zero git exit surfaces as exit 1" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--unset", "tk.scope" }, .{ .exit_code = 128 });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.worktree_clear_failed ++ "\n", h.stderr());
}
