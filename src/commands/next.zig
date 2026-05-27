//! `tk next` — select the next ready Ticket.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const resolver = @import("resolver.zig");
const worktree_scope = @import("../worktree/scope.zig");
const init_command = @import("init.zig");
const add_command = @import("add.zig");
const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
const zqlite = @import("zqlite");

/// Dispatcher metadata for `tk next`.
pub const meta: cli.CommandMeta = .{
    .name = "next",
    .description = "Select the next ready Ticket",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\
);

/// Parse `tk next` flags, read the Repository Store, and print one ready Ticket.
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

    const store = (resolver.open(deps.gpa, deps.runner, deps.cwd, deps.stderr, open_msgs) orelse return 1).store;
    defer store.close();

    const raw = worktree_scope.readGitSide(deps.gpa, deps.runner, deps.cwd) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    defer worktree_scope.freeRaw(deps.gpa, raw);
    const scope_outcome = worktree_scope.resolveAgainstStore(store, deps.gpa, raw) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    var scope_payload: ?worktree_scope.Scope = null;
    defer if (scope_payload) |s| s.deinit(deps.gpa);
    const next_scope: repository.NextScope = switch (scope_outcome) {
        .none => .none,
        .scope => |s| blk: {
            scope_payload = s;
            break :blk .{ .display_arg = s.display_id };
        },
        .configured_unresolved => |stored| {
            defer deps.gpa.free(stored);
            deps.stderr.print(
                "{s}{s}{s}\n",
                .{ messages.next_scope_unresolved_prefix, stored, messages.next_scope_unresolved_suffix },
            ) catch {};
            return 1;
        },
    };

    var outcome: repository.NextOutcome = undefined;
    repository.nextReadyTicket(store, deps.gpa, .{ .scope = next_scope }, &outcome) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    switch (outcome) {
        .ticket => |ticket| {
            defer ticket.deinit(deps.gpa);
            try deps.stdout.print("{s}\n", .{ticket.display_id});
            if (ticket.rationale) |r| {
                // Per ADR 0015, rationale lands on stderr so
                // `id="$(tk next)"` keeps a clean stdout capture.
                deps.stderr.print(
                    "{s}{s}{s}{s}{s}{s}\n",
                    .{
                        ticket.display_id,
                        messages.next_rationale_infix_effective,
                        r.effective_priority,
                        messages.next_rationale_infix_via,
                        r.blocked_display_id,
                        messages.next_rationale_suffix,
                    },
                ) catch {};
            }
            return 0;
        },
        .no_ready_ticket => {
            const message = if (scope_payload != null) messages.next_no_ready_ticket_in_scope else messages.next_no_ready_ticket;
            deps.stderr.writeAll(message) catch {};
            deps.stderr.writeAll("\n") catch {};
            return 1;
        },
        .scope_not_found => {
            deps.stderr.writeAll(messages.next_scope_not_found ++ "\n") catch {};
            return 1;
        },
    }
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk next - select the next ready Ticket
        \\
        \\Usage:
        \\  tk next [options]
        \\
        \\Options:
        \\  -h, --help  Display this help and exit.
        \\
        \\Ordering:
        \\  Ranks ready Tickets by Effective Priority (lowest first), then
        \\  own Priority, then created_seq, within the active Workspace
        \\  Scope. Effective Priority lifts a ready Ticket above its own
        \\  Priority when it transitively blocks a higher-Priority item.
        \\
        \\Output:
        \\  Prints one Display ID to stdout. When the pick's Effective
        \\  Priority is lower than its own Priority, also writes a
        \\  rationale line to stderr (suppress with 2>/dev/null).
        \\
    );
}

const storage_msgs: resolver.StorageErrorMessages = .{
    .busy_retry = messages.next_store_busy_retry,
    .out_of_memory = messages.next_out_of_memory,
    .fallback = messages.next_read_failed,
};

const open_msgs: resolver.OpenMessages = .{
    .command_name = "next",
    .missing_store = messages.next_missing_store,
    .storage = storage_msgs,
};

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    resolver.renderStorageError(deps.stderr, err, storage_msgs);
}

test "next: configured Workspace Scope selects within scope and overrides default ordering" {
    const gpa = std.testing.allocator;
    var tmp_store = try TmpStore.init(gpa, "project");
    defer tmp_store.deinit(gpa);
    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
    defer cwd.close(std.testing.io);
    const rev_parse = try tmp_store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "First", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "t2", .display = "project-2", .title = "Second", .created_seq = 2 });

    // Without scope, default ordering picks `project-1` first. With
    // `tk.scope = project-2` configured, `tk next` must return `project-2`.
    var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try h.fake_runner.expect(
        &.{ "git", "config", "--worktree", "--get", "tk.scope" },
        .{ .exit_code = 0, .stdout = "project-2\n" },
    );
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("project-2\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "next: with a blocked non-epic ticket scope returns exit 1" {
    const gpa = std.testing.allocator;
    var tmp_store = try TmpStore.init(gpa, "project");
    defer tmp_store.deinit(gpa);
    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
    defer cwd.close(std.testing.io);
    const rev_parse = try tmp_store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "a", .display = "project-1", .title = "Scoped Ticket", .priority = "P3", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "b", .display = "project-2", .title = "Blocking ready Ticket", .priority = "P0", .created_seq = 2 });
    try TmpStore.insertDependency(conn, "b", "a"); // b blocks a

    // Configure workspace scope to project-1 (the blocked ticket).
    var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try h.fake_runner.expect(
        &.{ "git", "config", "--worktree", "--get", "tk.scope" },
        .{ .exit_code = 0, .stdout = "project-1\n" },
    );
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.next_no_ready_ticket_in_scope ++ "\n", h.stderr());
}

test "next: prints the first ready Ticket from the Repository Store" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    try cwd.writeFile(std.testing.io, .{
        .sub_path = "item.md",
        .data = "Write next command\n",
    });

    {
        var h = Harness.init(gpa, &.{ "-F", "item.md" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try add_command.run(h.deps(), &h.iter));
    }

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try expectNoWorkspaceScope(&h);

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("project-1\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "next: returns exit 1 when no ready Ticket exists" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try expectNoWorkspaceScope(&h);

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.next_no_ready_ticket ++ "\n", h.stderr());
}

/// Register the pair of git invocations `readGitSide` makes when no
/// Workspace Scope is configured. Each call exits non-zero, so both
/// `tk.scope` lookup and branch-name inference collapse to `null`.
fn expectNoWorkspaceScope(h: *Harness) !void {
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });
}

test "next: prints stderr rationale when Effective Priority comes from a Blocked Item" {
    const gpa = std.testing.allocator;
    var tmp_store = try TmpStore.init(gpa, "project");
    defer tmp_store.deinit(gpa);
    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
    defer cwd.close(std.testing.io);
    const rev_parse = try tmp_store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    // Direct fixture inserts skip the absent `tk block` Mutation surface and
    // exercise the Effective Priority SQL through the command renderer.
    const conn = try zqlite.open(tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocker", .display = "project-1", .title = "Blocker", .priority = "P3", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked", .display = "project-2", .title = "Blocked P1", .priority = "P1", .created_seq = 2 });
    try TmpStore.insertDependency(conn, "blocker", "blocked");

    var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try expectNoWorkspaceScope(&h);

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("project-1\n", h.stdout());
    try std.testing.expectEqualStrings(
        "project-1: Effective Priority P1 (via project-2)\n",
        h.stderr(),
    );
}

test "next: no stderr rationale when Effective Priority equals own Priority" {
    const gpa = std.testing.allocator;
    var tmp_store = try TmpStore.init(gpa, "project");
    defer tmp_store.deinit(gpa);
    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
    defer cwd.close(std.testing.io);
    const rev_parse = try tmp_store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "solo", .display = "project-1", .title = "Standalone", .priority = "P1", .created_seq = 1 });

    var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try expectNoWorkspaceScope(&h);

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("project-1\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "next: rejects explicit scope arguments" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"tk-1"}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expect(std.mem.startsWith(u8, h.stderr(), "tk next: "));
}

test "next: reports missing store after successful Git discovery" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.next_missing_store ++ "\n", h.stderr());
}
