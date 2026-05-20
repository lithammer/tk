const std = @import("std");
const clap = @import("clap");
const proc = @import("proc/runner.zig");
const clock_mod = @import("clock.zig");

/// Runtime services available to command handlers.
///
/// Commands receive these explicitly instead of reaching for process globals so
/// unit tests and txtar scenarios can substitute writers, clocks, working
/// directories, and subprocess runners.
pub const Deps = struct {
    /// Primary command output. Commands write user data here.
    stdout: *std.Io.Writer,
    /// Diagnostics and usage errors. Commands avoid writing normal output here.
    stderr: *std.Io.Writer,
    /// Primary command input. Commands read this only when their contract
    /// explicitly accepts stdin, such as `tk add -F -`.
    stdin: *std.Io.Reader,
    /// Allocator used for parsing and short-lived command work.
    gpa: std.mem.Allocator,
    /// I/O implementation handle, threaded through filesystem and subprocess
    /// calls. Tests use `std.testing.io`; main uses the runtime's init.io.
    io: std.Io,
    /// User's working directory at invocation time. Passed as the cwd for
    /// subprocess discovery (e.g. `git rev-parse`); filesystem operations on
    /// absolute paths (the Repository Store location) bypass it.
    cwd: std.Io.Dir,
    /// Captured-output subprocess runner. Real impl wraps `std.process.run`;
    /// tests inject a `FakeRunner`.
    runner: proc.Runner,
    /// UTC millisecond clock. Tests inject a `FakeClock` so timestamps stay
    /// deterministic.
    clock: clock_mod.Clock,
    /// Random source used for opaque internal IDs.
    random: std.Random,
};

/// Metadata every command module exports for dispatcher registration and help.
pub const CommandMeta = struct {
    /// CLI spelling, also used as the generated `SubCommand` tag name.
    name: [:0]const u8,
    /// One-line description shown in `tk --help`.
    description: []const u8,
};

/// Metadata for a planned subcommand whose surface is documented in `tk prime`
/// and `docs/cli.md` but whose implementation has not yet shipped.
///
/// Registered in `unimplemented_commands` so an agent that follows the
/// workflow vision from `tk prime` and reaches for a future command receives a
/// deliberate "not yet implemented" diagnostic instead of an unknown-
/// subcommand error from the top-level parser. The slot is removed when the
/// real command module lands in `all_commands`.
pub const UnimplementedMeta = struct {
    /// CLI spelling, also used as the generated `SubCommand` tag name.
    name: [:0]const u8,
    /// One-line description shown under the "Planned" section of `tk --help`.
    description: []const u8,
    /// One-line tracking note pointing at the slice that will ship the
    /// command. Rendered after the "not yet implemented" line on stderr so
    /// agents see where to look.
    tracking: []const u8,
};

const all_commands = .{
    @import("commands/init.zig"),
    @import("commands/add.zig"),
    @import("commands/block.zig"),
    @import("commands/done.zig"),
    @import("commands/list.zig"),
    @import("commands/manpage.zig"),
    @import("commands/next.zig"),
    @import("commands/prime.zig"),
    @import("commands/show.zig"),
    @import("commands/start.zig"),
    @import("commands/stop.zig"),
    @import("commands/unblock.zig"),
    @import("commands/update.zig"),
    @import("commands/worktree.zig"),
    @import("commands/remote.zig"),
    @import("commands/sync.zig"),
};

const remote_sync_slice = "Planned: remote and sync skeleton slice.";
const post_sync_slice = "Planned: later slice once Remote and sync are in place.";

const unimplemented_commands = [_]UnimplementedMeta{
    .{ .name = "promote", .description = "Promote a Local Ticket or Epic through the configured Remote", .tracking = post_sync_slice },
};

/// Top-level subcommand enum generated from `all_commands` and
/// `unimplemented_commands`.
///
/// The two compile-time tables are the only dispatcher touchpoints: the
/// enum, zig-clap parser, help listing, and dispatch switch all derive from
/// them. A new command starts as an `UnimplementedMeta` row (auto-registered
/// for "not yet implemented" diagnostics) and graduates to `all_commands`
/// when its module ships.
pub const SubCommand = blk: {
    const total = all_commands.len + unimplemented_commands.len;
    const Tag = std.math.IntFittingRange(0, total -| 1);
    var names: [total][]const u8 = undefined;
    var values: [total]Tag = undefined;
    for (all_commands, 0..) |cmd, i| {
        names[i] = cmd.meta.name;
        values[i] = @intCast(i);
    }
    for (unimplemented_commands, 0..) |stub, i| {
        names[all_commands.len + i] = stub.name;
        values[all_commands.len + i] = @intCast(all_commands.len + i);
    }
    break :blk @Enum(Tag, .exhaustive, &names, &values);
};

const VERSION = "v0.0.1";

const top_flag_text =
    \\-h, --help     Display this help and exit.
    \\-v, --version  Print version and exit.
    \\
;
const top_flags = clap.parseParamsComptime(top_flag_text);
const top_params = clap.parseParamsComptime(top_flag_text ++
    \\<command>
    \\
);
const top_parsers = .{ .command = clap.parsers.enumeration(SubCommand) };

const help_options: clap.HelpOptions = .{
    .description_on_new_line = false,
    .description_indent = 2,
    .indent = 2,
    .spacing_between_parameters = 0,
};

/// Parse top-level args and dispatch to a command handler.
///
/// Returns Ticket's process exit code contract: 0 success, 1 logical failure,
/// 2 usage error, and propagated unexpected errors for `main.zig` to translate
/// to exit 3.
pub fn runArgv(deps: Deps, args_iter: anytype) !u8 {
    var diag: clap.Diagnostic = .{};
    // Treat every clap parse error as exit 2. The contract pins
    // error.OutOfMemory as exit 3, but Zig 0.16 with clap v0.12.0 and our
    // `enumeration` + `terminating_positional` config has OOM compiled out
    // of the inferred error set; if a future command's parser becomes able
    // to allocate fallibly, this catch must be split to let OOM propagate.
    var res = clap.parseEx(clap.Help, &top_params, top_parsers, args_iter, .{
        .diagnostic = &diag,
        .allocator = deps.gpa,
        .terminating_positional = 0,
    }) catch |err| {
        // TODO(ticket-2): prefix clap diagnostics with the command name —
        // applies symmetrically here for top-level parse failures.
        diag.report(deps.stderr, err) catch {};
        return 2;
    };
    defer res.deinit();

    if (res.args.help != 0) {
        writeTopLevelHelp(deps) catch {};
        return 0;
    }
    if (res.args.version != 0) {
        deps.stdout.print(VERSION ++ "\n", .{}) catch {};
        return 0;
    }

    const subcmd = res.positionals[0] orelse {
        deps.stderr.writeAll("tk: missing subcommand; run 'tk --help' for usage\n") catch {};
        return 2;
    };

    return switch (subcmd) {
        inline else => |tag| blk: {
            inline for (all_commands) |cmd| {
                if (comptime std.mem.eql(u8, cmd.meta.name, @tagName(tag))) {
                    break :blk cmd.run(deps, args_iter);
                }
            }
            inline for (unimplemented_commands) |stub| {
                if (comptime std.mem.eql(u8, stub.name, @tagName(tag))) {
                    break :blk runUnimplemented(deps, stub);
                }
            }
            unreachable;
        },
    };
}

/// Render the "not yet implemented" diagnostic for a planned subcommand.
///
/// Returns exit 1 to keep the contract honest: an agent that invoked the
/// command did request work, the work did not happen, and exit 0 would mask
/// the gap. The body names the tracking slice so the agent knows where to
/// look without grepping the codebase.
fn runUnimplemented(deps: Deps, stub: UnimplementedMeta) !u8 {
    deps.stderr.print(
        "tk {s}: not yet implemented\n{s}\n",
        .{ stub.name, stub.tracking },
    ) catch {};
    return 1;
}

fn writeTopLevelHelp(deps: Deps) !void {
    try deps.stdout.writeAll(
        \\tk - an agent-first CLI for tracking work items
        \\
        \\Usage:
        \\  tk <command> [options]
        \\  tk [-h | --help]
        \\  tk [-v | --version]
        \\
        \\Commands:
        \\
    );
    inline for (all_commands) |cmd| {
        try deps.stdout.print("  {s:<10} {s}\n", .{ cmd.meta.name, cmd.meta.description });
    }
    try deps.stdout.writeAll(
        \\
        \\Planned (not yet implemented):
        \\
    );
    inline for (unimplemented_commands) |stub| {
        try deps.stdout.print("  {s:<10} {s}\n", .{ stub.name, stub.description });
    }
    try deps.stdout.writeAll(
        \\
        \\Options:
        \\
    );
    try clap.help(deps.stdout, clap.Help, &top_flags, help_options);
    try deps.stdout.writeAll(
        \\
        \\Run 'tk <command> --help' for command-specific help.
        \\
    );
}

const Harness = @import("testing/test_cli.zig").Harness;
const TmpStore = @import("testing/tmp_store.zig").TmpStore;
const zqlite = @import("zqlite");

test "runArgv routes prime" {
    var h = Harness.init(std.testing.allocator, &.{"prime"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(h.stdout().len > 0);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "runArgv returns 2 on unknown subcommand" {
    var h = Harness.init(std.testing.allocator, &.{"bogus"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(h.stderr().len > 0);
}

test "runArgv returns 2 on extra positional after a subcommand" {
    // Trailing positionals after a known subcommand surface through the
    // subcommand parser; this test pins the dispatcher contract once
    // (exit 2 + non-empty stderr) so individual commands don't each have
    // to retest it.
    var h = Harness.init(std.testing.allocator, &.{ "prime", "unexpected" });
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(h.stderr().len > 0);
}

test "runArgv returns 2 on missing subcommand" {
    var h = Harness.init(std.testing.allocator, &.{});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(h.stderr().len > 0);
}

test "runArgv prints version" {
    var h = Harness.init(std.testing.allocator, &.{"--version"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expectEqualStrings("v0.0.1\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "runArgv prints help" {
    var h = Harness.init(std.testing.allocator, &.{"--help"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Usage:") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Commands:") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "init") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "add") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "done") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "list") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "manpage") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "prime") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--version") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Planned (not yet implemented):") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "worktree") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "sync") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "runArgv routes a planned subcommand to a not-yet-implemented stub" {
    var h = Harness.init(std.testing.allocator, &.{"promote"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        "tk promote: not yet implemented\n" ++ post_sync_slice ++ "\n",
        h.stderr(),
    );
}

test "runArgv block creates a Dependency that affects next ready Ticket" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked", .display = "project-1", .title = "Blocked work", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocking", .display = "project-2", .title = "Blocking work", .created_seq = 2 });

    {
        var h = Harness.initWith(gpa, &.{ "block", "project-1", "project-2" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("Added Dependency: project-1 blocked by project-2\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }

    {
        var h = Harness.initWith(gpa, &.{"next"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
        try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("project-2\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "runArgv unblock removes a Dependency so the blocked Ticket is ready again" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked", .display = "project-1", .title = "Blocked work", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocking", .display = "project-2", .title = "Blocking work", .created_seq = 2 });
    try TmpStore.insertDependency(conn, "blocking", "blocked");

    {
        var h = Harness.initWith(gpa, &.{ "unblock", "project-1", "project-2" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("Removed Dependency: project-1 no longer blocked by project-2\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }

    {
        var h = Harness.initWith(gpa, &.{"next"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
        try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("project-1\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "runArgv block rejects a self Dependency with a role-specific diagnostic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "item", .display = "project-1", .title = "One item", .created_seq = 1 });

    var h = Harness.initWith(gpa, &.{ "block", "project-1", "project-1" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings("tk block: an item cannot depend on itself\n", h.stderr());
}

test "runArgv unblock rejects a self Dependency argument pair" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "item", .display = "project-1", .title = "One item", .created_seq = 1 });

    var h = Harness.initWith(gpa, &.{ "unblock", "project-1", "project-1" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings("tk unblock: an item cannot depend on itself\n", h.stderr());
}

test "runArgv block rejects a done Blocked Item" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked", .display = "project-1", .title = "Finished work", .status = "done", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocking", .display = "project-2", .title = "Blocking work", .created_seq = 2 });

    var h = Harness.initWith(gpa, &.{ "block", "project-1", "project-2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings("tk block: blocked item 'project-1' is done\n", h.stderr());
}

test "runArgv block rejects a Dependency cycle with a domain diagnostic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "a", .display = "project-1", .title = "First item", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "b", .display = "project-2", .title = "Second item", .created_seq = 2 });
    try TmpStore.insertDependency(conn, "b", "a");

    var h = Harness.initWith(gpa, &.{ "block", "project-2", "project-1" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings("tk block: Dependency would create a cycle\n", h.stderr());
}

test "runArgv block emits add_dependency Mutation for same-backend items" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "Backend blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocking",
        .display = "GH#2",
        .title = "Backend blocking",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 2,
    });

    var h = Harness.initWith(gpa, &.{ "block", "GH#1", "GH#2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Added Dependency: GH#1 blocked by GH#2\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());

    const row = (try conn.row(
        \\select mutation_type, item_id, item_class, payload_json
        \\  from mutations
        \\ where sequence = 1
    , .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("add_dependency", row.text(0));
    try std.testing.expectEqualStrings("blocked", row.text(1));
    try std.testing.expectEqualStrings("ticket", row.text(2));
    try std.testing.expectEqualStrings("{\"blocking_id\":\"blocking\"}", row.text(3));
}

test "runArgv block rejects Backend Blocked Item depending on Local Blocking Item" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "Backend blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocking", .display = "project-1", .title = "Local blocking", .created_seq = 2 });

    var h = Harness.initWith(gpa, &.{ "block", "GH#1", "project-1" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        "tk block: Backend blocked item 'GH#1' cannot depend on Local blocking item 'project-1'\n",
        h.stderr(),
    );
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "runArgv block rejects Backend Dependency across Backend kinds" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "GitHub blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocking",
        .display = "JIRA-2",
        .title = "Jira blocking",
        .origin = "backend",
        .backend_kind = "jira",
        .backend_key = "2",
        .created_seq = 2,
    });

    var h = Harness.initWith(gpa, &.{ "block", "GH#1", "JIRA-2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        "tk block: Backend blocked item 'GH#1' cannot depend on blocking item 'JIRA-2' from another Backend kind\n",
        h.stderr(),
    );
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "runArgv unblock emits remove_dependency Mutation for same-backend items" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "Backend blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocking",
        .display = "GH#2",
        .title = "Backend blocking",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 2,
    });
    try TmpStore.insertDependency(conn, "blocking", "blocked");

    var h = Harness.initWith(gpa, &.{ "unblock", "GH#1", "GH#2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Removed Dependency: GH#1 no longer blocked by GH#2\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());

    const row = (try conn.row(
        \\select mutation_type, item_id, item_class, payload_json
        \\  from mutations
        \\ where sequence = 1
    , .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("remove_dependency", row.text(0));
    try std.testing.expectEqualStrings("blocked", row.text(1));
    try std.testing.expectEqualStrings("ticket", row.text(2));
    try std.testing.expectEqualStrings("{\"blocking_id\":\"blocking\"}", row.text(3));
}

test "runArgv block existing Dependency is idempotent and emits no Mutation" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "Backend blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocking",
        .display = "GH#2",
        .title = "Backend blocking",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 2,
    });
    try TmpStore.insertDependency(conn, "blocking", "blocked");

    var h = Harness.initWith(gpa, &.{ "block", "GH#1", "GH#2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Added Dependency: GH#1 blocked by GH#2\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}
