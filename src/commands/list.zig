//! `tk list` — render the Repository Store List Tree.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const ItemStatus = @import("../domain/status.zig").ItemStatus;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;
const init_command = @import("init.zig");
const add_command = @import("add.zig");
const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
const styler_mod = @import("../render/styler.zig");
const palette = @import("../render/palette.zig");
const Priority = @import("../domain/priority.zig").Priority;
const Style = @import("../render/style.zig").Style;

/// Dispatcher metadata for `tk list`.
pub const meta: cli.CommandMeta = .{
    .name = "list",
    .description = "Render the Repository Store List Tree",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\--ready     Show ready Tickets.
    \\--blocked   Show blocked Tickets.
    \\--active    Show active Tickets and Epics.
    \\--local     Show local items.
    \\--remote    Show Remote-backed items.
    \\
);

/// Parse `tk list` flags, read the Repository Store, and render the List Tree.
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
    const options = parseOptions(deps, res.args) orelse return 2;

    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, open_msgs) orelse return 1;
    defer store.close();

    const rows = repository.listRows(store, deps.gpa, options) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    defer repository.freeListRows(deps.gpa, rows);

    try render(deps.stdout, rows, options, deps.styler.forStdout());
    return 0;
}

fn parseOptions(deps: cli.Deps, args: anytype) ?repository.ListOptions {
    const view_count: u8 =
        @as(u8, @intFromBool(args.ready != 0)) +
        @as(u8, @intFromBool(args.blocked != 0)) +
        @as(u8, @intFromBool(args.active != 0));
    if (view_count > 1) {
        deps.stderr.writeAll(messages.list_conflicting_readiness_filters ++ "\n") catch {};
        return null;
    }
    if (args.local != 0 and args.remote != 0) {
        deps.stderr.writeAll(messages.list_conflicting_origin_filters ++ "\n") catch {};
        return null;
    }

    const view: repository.ListView = if (args.ready != 0)
        .ready
    else if (args.blocked != 0)
        .blocked
    else if (args.active != 0)
        .active
    else
        .default;

    const origin: repository.ListOriginFilter = if (args.local != 0)
        .local
    else if (args.remote != 0)
        .remote
    else
        .any;

    return .{ .view = view, .origin = origin };
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk list - render the Repository Store List Tree
        \\
        \\Usage:
        \\  tk list [options]
        \\
        \\Options:
        \\  -h, --help  Display this help and exit.
        \\  --ready     Show ready Tickets.
        \\  --blocked   Show blocked Tickets.
        \\  --active    Show active Tickets and Epics.
        \\  --local     Show local items.
        \\  --remote    Show Remote-backed items.
        \\
    );
}

fn render(stdout: *std.Io.Writer, rows: []const repository.ListRow, options: repository.ListOptions, styler: styler_mod.SubStyler) !void {
    if (rows.len == 0) {
        try stdout.print("{s}\n", .{emptyMessage(options)});
        return;
    }

    var counts: StatusCounts = .{};
    for (rows) |*row| {
        counts.add(row.status);
    }

    for (rows) |*row| {
        if (parentIsRendered(rows, row)) continue;
        try renderRow(stdout, row, "", styler);
        try renderChildren(stdout, rows, row, styler);
    }

    // Cleanly print separator using styler.wrap
    try styler.wrap(palette.separator, "--------------------------------------------------------------------------------").format(stdout);
    try stdout.writeAll("\n");

    try renderTotal(stdout, rows.len, counts);
    try stdout.writeAll("\n");

    try stdout.writeAll(messages.list_status_label);

    // Render status icons using canonical glyphs and SubStyler wrappers
    try styler.wrap(palette.status_open, ItemStatus.open.glyph()).format(stdout);
    try stdout.writeAll(" open  ");

    try styler.wrap(palette.status_active, ItemStatus.active.glyph()).format(stdout);
    try stdout.writeAll(" active  ");

    try styler.wrap(palette.status_done, ItemStatus.done.glyph()).format(stdout);
    try stdout.writeAll(" done\n");

    try stdout.writeAll("Blocked: ");
    try styler.wrap(palette.blocked, "⊘").format(stdout);
    try stdout.writeAll(" blocked\n");
}

fn renderChildren(stdout: *std.Io.Writer, rows: []const repository.ListRow, parent: *const repository.ListRow, styler: styler_mod.SubStyler) !void {
    const child_count = countRenderedChildren(rows, parent.id);
    var child_index: usize = 0;
    for (rows) |*child| {
        const container_id = child.container_id orelse continue;
        if (!std.mem.eql(u8, container_id, parent.id)) continue;
        child_index += 1;
        const prefix = if (child_index == child_count) "└── " else "├── ";
        try renderRow(stdout, child, prefix, styler);
    }
}

fn parentIsRendered(rows: []const repository.ListRow, row: *const repository.ListRow) bool {
    const container_id = row.container_id orelse return false;
    return findRowById(rows, container_id) != null;
}

fn countRenderedChildren(rows: []const repository.ListRow, parent_id: []const u8) usize {
    var count: usize = 0;
    for (rows) |*row| {
        const container_id = row.container_id orelse continue;
        if (std.mem.eql(u8, container_id, parent_id)) count += 1;
    }
    return count;
}

fn findRowById(rows: []const repository.ListRow, id: []const u8) ?*const repository.ListRow {
    for (rows) |*row| {
        if (std.mem.eql(u8, row.id, id)) return row;
    }
    return null;
}

fn emptyMessage(options: repository.ListOptions) []const u8 {
    return switch (options.view) {
        .default => switch (options.origin) {
            .local => messages.list_empty_local,
            .remote => messages.list_empty_remote,
            .any => messages.list_empty_default,
        },
        .ready => messages.list_empty_ready,
        .blocked => messages.list_empty_blocked,
        .active => messages.list_empty_active,
    };
}

fn priorityStyle(priority: Priority) Style {
    return switch (priority) {
        .P0 => palette.priority_p0,
        .P1 => palette.priority_p1,
        .P2 => palette.priority_p2,
        .P3 => palette.priority_p3,
        .P4 => palette.priority_p4,
    };
}

fn renderRow(stdout: *std.Io.Writer, row: *const repository.ListRow, tree_prefix: []const u8, styler: styler_mod.SubStyler) !void {
    try stdout.writeAll(tree_prefix);

    if (row.has_unresolved_blocker) {
        try stdout.writeAll(styler.open(palette.blocked_row));
    }

    const status_style = switch (row.status) {
        .open => palette.status_open,
        .active => palette.status_active,
        .done => palette.status_done,
    };
    try styler.wrap(status_style, row.status.glyph()).format(stdout);
    try stdout.writeAll(" ");

    const id_style = switch (row.item_class) {
        .epic => palette.id_epic,
        .ticket => palette.id_ticket,
    };
    try styler.wrap(id_style, row.display_id).format(stdout);

    if (row.has_unresolved_blocker) {
        try stdout.writeAll(" ");
        try styler.wrap(palette.blocked, "⊘").format(stdout);
    }

    switch (row.item_class) {
        .ticket => {
            const priority = row.priority orelse unreachable;
            const p_style = priorityStyle(priority);

            try stdout.writeAll(" ");
            try styler.wrap(p_style, "●").format(stdout);
            try stdout.writeAll(" ");
            try styler.wrap(p_style, priority.text()).format(stdout);

            if (row.ticket_kind == TicketKind.bug) {
                try stdout.writeAll(" ");
                try styler.wrap(palette.kind_bug, "[bug]").format(stdout);
            }
            try stdout.writeAll(" ");
            try stdout.writeAll(row.title);
        },
        .epic => {
            try stdout.writeAll(" ");
            try styler.wrap(palette.kind_epic, "[epic]").format(stdout);
            try stdout.writeAll(" ");
            try stdout.writeAll(row.title);
        },
    }

    if (row.has_unresolved_blocker) {
        try stdout.writeAll(styler.close(palette.blocked_row));
    }
    try stdout.writeAll("\n");
}

fn renderTotal(stdout: *std.Io.Writer, total: usize, counts: StatusCounts) !void {
    try stdout.print(messages.list_total_label ++ "{d} {s} (", .{ total, if (total == 1) "item" else "items" });
    var wrote = false;
    try renderCount(stdout, &wrote, counts.open, "open");
    try renderCount(stdout, &wrote, counts.active, "active");
    try renderCount(stdout, &wrote, counts.done, "done");
    try stdout.writeAll(")\n");
}

fn renderCount(stdout: *std.Io.Writer, wrote: *bool, count: usize, label: []const u8) !void {
    if (count == 0) return;
    if (wrote.*) try stdout.writeAll(", ");
    try stdout.print("{d} {s}", .{ count, label });
    wrote.* = true;
}

const StatusCounts = struct {
    open: usize = 0,
    active: usize = 0,
    done: usize = 0,

    fn add(self: *StatusCounts, status: ItemStatus) void {
        switch (status) {
            .open => self.open += 1,
            .active => self.active += 1,
            .done => self.done += 1,
        }
    }
};

const storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.list_store_busy_retry,
    .out_of_memory = messages.list_out_of_memory,
    .fallback = messages.list_read_failed,
};

const open_msgs: repository.OpenMessages = .{
    .command_name = "list",
    .missing_store = messages.list_missing_store,
    .storage = storage_msgs,
};

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, storage_msgs);
}

test "list: renders an unparented local Ticket from the Repository Store" {
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
        .data = "Write list command\n",
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

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ project-1 ● P2 Write list command
            \\--------------------------------------------------------------------------------
            \\Total: 1 item (1 open)
            \\
            \\Status: ○ open  ◐ active  ✓ done
            \\Blocked: ⊘ blocked
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "list: --ready renders ready Tickets with Epic containers" {
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

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic-ready",
        .display = "project-1",
        .item_class = "epic",
        .priority = null,
        .ticket_kind = null,
        .title = "Ship list command",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket-ready",
        .display = "project-2",
        .title = "Render ready list",
        .priority = "P1",
        .container_id = "epic-ready",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket-ready-second",
        .display = "project-5",
        .title = "Check tree glyphs",
        .priority = "P3",
        .container_id = "epic-ready",
        .created_seq = 5,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket-blocked",
        .display = "project-3",
        .title = "Wait for blocked fixture",
        .container_id = "epic-ready",
        .created_seq = 3,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic-blocker",
        .display = "project-4",
        .item_class = "epic",
        .priority = null,
        .ticket_kind = null,
        .title = "External decision",
        .created_seq = 4,
    });
    try TmpStore.insertDependency(conn, "epic-blocker", "ticket-blocked");

    var h = Harness.init(gpa, &.{"--ready"}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        \\○ project-1 [epic] Ship list command
        \\├── ○ project-2 ● P1 Render ready list
        \\└── ○ project-5 ● P3 Check tree glyphs
        \\--------------------------------------------------------------------------------
        \\Total: 3 items (3 open)
        \\
        \\Status: ○ open  ◐ active  ✓ done
        \\Blocked: ⊘ blocked
        \\
    , h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "list: --blocked --remote promotes a matching child when Origin hides its Epic" {
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

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "local-epic",
        .display = "project-1",
        .item_class = "epic",
        .priority = null,
        .ticket_kind = null,
        .title = "Local parent",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "remote-ticket",
        .display = "GH#9",
        .ticket_kind = "bug",
        .priority = "P0",
        .title = "Fix remote blocker",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "9",
        .container_id = "local-epic",
        .created_seq = 2,
    });
    try TmpStore.insertExternalBlocker(conn, "blocker-1", "remote-ticket", null);

    var h = Harness.init(gpa, &.{ "--blocked", "--remote" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        \\○ GH#9 ⊘ ● P0 [bug] Fix remote blocker
        \\--------------------------------------------------------------------------------
        \\Total: 1 item (1 open)
        \\
        \\Status: ○ open  ◐ active  ✓ done
        \\Blocked: ⊘ blocked
        \\
    , h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "list: marks rendered rows with unresolved blockers" {
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

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked", .display = "project-1", .title = "Blocked work", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocking", .display = "project-2", .title = "Blocking work", .created_seq = 2 });
    try TmpStore.insertDependency(conn, "blocking", "blocked");

    var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        \\○ project-1 ⊘ ● P2 Blocked work
        \\○ project-2 ● P2 Blocking work
        \\--------------------------------------------------------------------------------
        \\Total: 2 items (2 open)
        \\
        \\Status: ○ open  ◐ active  ✓ done
        \\Blocked: ⊘ blocked
        \\
    , h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "list: default view excludes done items" {
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

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{ .id = "open", .display = "project-1", .title = "Open work", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "active", .display = "project-2", .title = "Active work", .status = "active", .created_seq = 2 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "done", .display = "project-3", .title = "Done work", .status = "done", .created_seq = 3 });

    var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        \\○ project-1 ● P2 Open work
        \\◐ project-2 ● P2 Active work
        \\--------------------------------------------------------------------------------
        \\Total: 2 items (1 open, 1 active)
        \\
        \\Status: ○ open  ◐ active  ✓ done
        \\Blocked: ⊘ blocked
        \\
    , h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "list: --active retains inactive Epic containers for active child Tickets" {
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

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Container",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "active-child",
        .display = "project-2",
        .title = "Active child",
        .status = "active",
        .container_id = "epic",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "active-epic",
        .display = "project-3",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Active Epic",
        .status = "active",
        .created_seq = 3,
    });

    var h = Harness.init(gpa, &.{"--active"}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        \\○ project-1 [epic] Container
        \\└── ◐ project-2 ● P2 Active child
        \\◐ project-3 [epic] Active Epic
        \\--------------------------------------------------------------------------------
        \\Total: 3 items (1 open, 2 active)
        \\
        \\Status: ○ open  ◐ active  ✓ done
        \\Blocked: ⊘ blocked
        \\
    , h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "list: reports missing store after successful Git discovery" {
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
    try std.testing.expectEqualStrings(messages.list_missing_store ++ "\n", h.stderr());
}

test "list: validates mutually exclusive filters" {
    {
        var h = Harness.init(std.testing.allocator, &.{ "--ready", "--blocked" }, .{});
        defer h.deinit();

        try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(messages.list_conflicting_readiness_filters ++ "\n", h.stderr());
    }
    {
        var h = Harness.init(std.testing.allocator, &.{ "--local", "--remote" }, .{});
        defer h.deinit();

        try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(messages.list_conflicting_origin_filters ++ "\n", h.stderr());
    }
}

test "list: renders styled output with correct ANSI sequences under escape_codes" {
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

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    // 1. P0 bug ticket
    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket-p0-bug",
        .display = "project-1",
        .title = "Critical bug",
        .priority = "P0",
        .ticket_kind = "bug",
        .created_seq = 1,
    });

    // 2. Active ticket
    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket-active",
        .display = "project-2",
        .title = "Active task",
        .status = "active",
        .created_seq = 2,
    });

    // 3. Blocked ticket
    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket-blocked",
        .display = "project-3",
        .title = "Blocked task",
        .created_seq = 3,
    });
    try TmpStore.insertExternalBlocker(conn, "ext-blocker", "ticket-blocked", null);

    // Initialize harness with stdout_mode = .escape_codes
    var h = Harness.init(gpa, &.{}, .{
        .cwd = cwd,
        .stdout_mode = .escape_codes,
    });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));

    const out = h.stdout();

    // Assert correct color codes are present
    // Yellow status glyph for active: "◐" (active) wrapped in \x1b[33m ... \x1b[39m
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[33m\xe2\x97\x90\x1b[39m") != null);

    // Red priority dot and text for P0: "●" and "P0" wrapped in \x1b[31m ... \x1b[39m
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[31m\xe2\x97\x8f\x1b[39m") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[31mP0\x1b[39m") != null);

    // Red bug tag: "[bug]" wrapped in \x1b[31m ... \x1b[39m
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[31m[bug]\x1b[39m") != null);

    // Blocked ticket has whole-row dimming: \x1b[2m at the start of the row, closed with \x1b[22m
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[2m\xe2\x97\x8b project-3 \xe2\x8a\x98") != null);

    // Separator line has dimming sequences: \x1b[2m--------------------------------------------------------------------------------\x1b[22m
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[2m--------------------------------------------------------------------------------\x1b[22m") != null);

    // Status legend glyphs:
    // "○" (open) is styled with status_open (style.none()), so it remains unstyled.
    // "◐" (active) is wrapped in status_active (style.yellow() -> \x1b[33m ... \x1b[39m)
    // "✓" (done) is wrapped in status_done (style.green() -> \x1b[32m ... \x1b[39m)
    try std.testing.expect(std.mem.indexOf(u8, out, "Status: \xe2\x97\x8b open  \x1b[33m\xe2\x97\x90\x1b[39m active  \x1b[32m\xe2\x9c\x93\x1b[39m done") != null);

    // Blocker legend glyph:
    // "⊘" (blocked) is styled with blocked (style.none()), so it remains unstyled.
    try std.testing.expect(std.mem.indexOf(u8, out, "Blocked: \xe2\x8a\x98 blocked") != null);
}
