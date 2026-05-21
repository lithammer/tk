//! `tk show` — render one Ticket or Epic with current state.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const ItemClass = @import("../domain/item_class.zig").ItemClass;
const ItemStatus = @import("../domain/status.zig").ItemStatus;
const Origin = @import("../domain/origin.zig").Origin;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;
const init_command = @import("init.zig");
const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

/// Dispatcher metadata for `tk show`.
pub const meta: cli.CommandMeta = .{
    .name = "show",
    .description = "Render one Ticket or Epic with current state",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\<str>
    \\
);

/// Parse `tk show` args, read the Repository Store, and render one item.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    var diag: clap.Diagnostic = .{};
    var res = clap.parseEx(clap.Help, &params, clap.parsers.default, args_iter, .{
        .diagnostic = &diag,
        .allocator = deps.gpa,
    }) catch |err| {
        diag.report(deps.stderr, err) catch {};
        return 2;
    };
    defer res.deinit();

    if (res.args.help != 0) {
        writeHelp(deps) catch {};
        return 0;
    }

    const id = res.positionals[0] orelse {
        deps.stderr.writeAll(messages.show_id_required ++ "\n") catch {};
        return 2;
    };

    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, open_msgs) orelse return 1;
    defer store.close();

    const detail = (repository.showItem(store, deps.gpa, id) catch |err| {
        renderStorageError(deps, err);
        return 1;
    }) orelse {
        deps.stderr.print(messages.show_id_not_found_prefix ++ "{s}" ++ messages.show_id_not_found_suffix ++ "\n", .{id}) catch {};
        return 1;
    };
    defer detail.deinit(deps.gpa);

    render(deps.stdout, detail) catch {};
    return 0;
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk show - render one Ticket or Epic with current state
        \\
        \\Usage:
        \\  tk show <id> [options]
        \\
        \\Options:
        \\  -h, --help  Display this help and exit.
        \\
    );
}

/// Render a full Beads-style item view to `stdout`.
///
/// Section order: DESCRIPTION, PARENT/TICKETS, BLOCKED BY, BLOCKING,
/// EXTERNAL BLOCKERS. Empty sections are omitted. Sections are separated by
/// one blank line. The output ends with a single trailing newline.
fn render(stdout: *std.Io.Writer, detail: repository.ItemDetail) !void {
    // Header: <status-glyph> <display-id> · <title>   [<facet> · STATUS]
    var facet_buf: [8]u8 = undefined;
    const facet: []const u8 = switch (detail.item_class) {
        .epic => "EPIC",
        .ticket => try std.fmt.bufPrint(&facet_buf, "● {s}", .{(detail.priority orelse unreachable).text()}),
    };
    const status_upper: []const u8 = switch (detail.status) {
        .open => "OPEN",
        .active => "ACTIVE",
        .done => "DONE",
    };
    try stdout.print("{s} {s} · {s}   [{s} · {s}]\n", .{
        detail.status.glyph(),
        detail.display_id,
        detail.title,
        facet,
        status_upper,
    });

    // Metadata line.
    switch (detail.origin) {
        .local => try stdout.writeAll("Origin: local"),
        .backend => {
            const bk = detail.backend_kind orelse "";
            const bkey = detail.backend_key orelse "";
            if (std.mem.eql(u8, bk, "github")) {
                try stdout.print("Origin: github (#{s})", .{bkey});
            } else {
                try stdout.print("Origin: {s} ({s})", .{ bk, bkey });
            }
        },
    }
    if (detail.item_class == .ticket) {
        const kind = detail.ticket_kind orelse unreachable;
        try stdout.print(" · Kind: {s}", .{kind.text()});
    }
    try stdout.writeAll("\n");

    // Date line: Created: YYYY-MM-DD · Updated: YYYY-MM-DD
    const created_date = detail.created_at[0..@min(10, detail.created_at.len)];
    const updated_date = detail.updated_at[0..@min(10, detail.updated_at.len)];
    try stdout.print("Created: {s} · Updated: {s}\n", .{ created_date, updated_date });

    // Track whether we have printed a section yet (for blank-line separators).
    var has_section = false;

    // DESCRIPTION section.
    if (detail.body.len > 0) {
        try stdout.writeAll("\n");
        try stdout.writeAll(messages.show_section_description ++ "\n");
        try stdout.writeAll(detail.body);
        if (detail.body[detail.body.len - 1] != '\n') {
            try stdout.writeAll("\n");
        }
        has_section = true;
    }

    // PARENT section (for Tickets with a container Epic).
    if (detail.parent) |p| {
        if (has_section) try stdout.writeAll("\n");
        try stdout.writeAll(messages.show_section_parent ++ "\n");
        try renderSubRow(stdout, "↑", p);
        has_section = true;
    }

    // TICKETS section (for Epics with children).
    if (detail.children.len > 0) {
        if (has_section) try stdout.writeAll("\n");
        try stdout.writeAll(messages.show_section_tickets ++ "\n");
        for (detail.children) |child| {
            try renderSubRow(stdout, "↓", child);
        }
        has_section = true;
    }

    // BLOCKED BY section.
    if (detail.blocked_by.len > 0) {
        if (has_section) try stdout.writeAll("\n");
        try stdout.writeAll(messages.show_section_blocked_by ++ "\n");
        for (detail.blocked_by) |item| {
            try renderSubRow(stdout, "→", item);
        }
        has_section = true;
    }

    // BLOCKING section.
    if (detail.blocking.len > 0) {
        if (has_section) try stdout.writeAll("\n");
        try stdout.writeAll(messages.show_section_blocking ++ "\n");
        for (detail.blocking) |item| {
            try renderSubRow(stdout, "→", item);
        }
        has_section = true;
    }

    // EXTERNAL BLOCKERS section.
    if (detail.external_blockers.len > 0) {
        if (has_section) try stdout.writeAll("\n");
        try stdout.writeAll(messages.show_section_external_blockers ++ "\n");
        for (detail.external_blockers) |eb| {
            try stdout.print("  • {s}\n", .{eb.reason});
        }
    }
}

/// Render one sub-row line with the given direction glyph.
///
/// Shape: `  <glyph> <status-glyph> <display-id>: [(EPIC) ]<title>[ ● <priority>]`
fn renderSubRow(stdout: *std.Io.Writer, glyph: []const u8, item: repository.ItemSummary) !void {
    const epic_prefix: []const u8 = if (item.item_class == .epic) "(EPIC) " else "";
    try stdout.print("  {s} {s} {s}: {s}{s}", .{
        glyph,
        item.status.glyph(),
        item.display_id,
        epic_prefix,
        item.title,
    });
    if (item.priority) |p| {
        try stdout.print(" ● {s}", .{p.text()});
    }
    try stdout.writeAll("\n");
}

const storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.show_store_busy_retry,
    .out_of_memory = messages.show_out_of_memory,
    .fallback = messages.show_read_failed,
};

const open_msgs: repository.OpenMessages = .{
    .command_name = "show",
    .missing_store = messages.show_missing_store,
    .storage = storage_msgs,
};

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, storage_msgs);
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

test "show: renders a minimal Ticket with no relationships" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    // Init the store.
    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    // Insert one Ticket via TmpStore.
    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "tk-1",
        .display = "project-1",
        .title = "First Ticket",
        .priority = "P2",
        .created_seq = 1,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });

    {
        var h = Harness.initWith(gpa, &.{"project-1"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ project-1 · First Ticket   [● P2 · OPEN]
            \\Origin: local · Kind: task
            \\Created: 2026-04-21 · Updated: 2026-04-21
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "show: renders a Ticket with parent, dependencies, and external blocker" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    // Epic parent (project-1).
    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Ship list command",
        .created_seq = 1,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-28T00:00:00.000Z",
    });
    // The Ticket (project-2).
    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket",
        .display = "project-2",
        .title = "Render ready list",
        .priority = "P1",
        .body = "Render the slice with ready-only filter and a folder of fixture test data.",
        .container_id = "epic",
        .created_seq = 2,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-28T00:00:00.000Z",
    });
    // Blocking item (project-3): unresolved so it appears in BLOCKED BY.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocker",
        .display = "project-3",
        .title = "Wait for backend",
        .priority = "P2",
        .created_seq = 3,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });
    try TmpStore.insertDependency(conn, "blocker", "ticket");
    // Downstream (project-9).
    try TmpStore.insertFixtureItem(conn, .{
        .id = "downstream",
        .display = "project-9",
        .title = "Downstream cleanup",
        .priority = "P2",
        .created_seq = 9,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });
    try TmpStore.insertDependency(conn, "ticket", "downstream");
    // External blocker (unresolved).
    try conn.exec(
        \\insert into external_blockers(id, item_id, reason, created_at, resolved_at)
        \\values (?1, ?2, ?3, '2026-04-21T00:00:00.000Z', null)
    , .{ "eb-open", "ticket", "Need legal review of GDPR exports" });
    // Resolved external blocker (excluded).
    try TmpStore.insertExternalBlocker(conn, "eb-done", "ticket", "2026-04-28T00:00:00.000Z");

    {
        var h = Harness.initWith(gpa, &.{"project-2"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ project-2 · Render ready list   [● P1 · OPEN]
            \\Origin: local · Kind: task
            \\Created: 2026-04-21 · Updated: 2026-04-28
            \\
            \\DESCRIPTION
            \\Render the slice with ready-only filter and a folder of fixture test data.
            \\
            \\PARENT
            \\  ↑ ○ project-1: (EPIC) Ship list command
            \\
            \\BLOCKED BY
            \\  → ○ project-3: Wait for backend ● P2
            \\
            \\BLOCKING
            \\  → ○ project-9: Downstream cleanup ● P2
            \\
            \\EXTERNAL BLOCKERS
            \\  • Need legal review of GDPR exports
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "show: renders an Epic with children listed" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
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
        .title = "Ship list command",
        .body = "The slice that ships ready-only filtering.",
        .created_seq = 1,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-28T00:00:00.000Z",
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "child-1",
        .display = "project-2",
        .title = "Render ready list",
        .priority = "P1",
        .container_id = "epic",
        .created_seq = 2,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "child-2",
        .display = "project-5",
        .title = "Check tree glyphs",
        .priority = "P3",
        .status = "active",
        .container_id = "epic",
        .created_seq = 5,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });

    {
        var h = Harness.initWith(gpa, &.{"project-1"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ project-1 · Ship list command   [EPIC · OPEN]
            \\Origin: local
            \\Created: 2026-04-21 · Updated: 2026-04-28
            \\
            \\DESCRIPTION
            \\The slice that ships ready-only filtering.
            \\
            \\TICKETS
            \\  ↓ ○ project-2: Render ready list ● P1
            \\  ↓ ◐ project-5: Check tree glyphs ● P3
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "show: renders Backend Ticket origin as github (#9)" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "gh-ticket",
        .display = "GH#9",
        .title = "Backend task",
        .priority = "P1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "9",
        .created_seq = 1,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });

    {
        var h = Harness.initWith(gpa, &.{"GH#9"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ GH#9 · Backend task   [● P1 · OPEN]
            \\Origin: github (#9) · Kind: task
            \\Created: 2026-04-21 · Updated: 2026-04-21
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "show: renders Backend Epic origin as jira (PROJ-12)" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "jira-epic",
        .display = "PROJ-12",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Jira Epic",
        .origin = "backend",
        .backend_kind = "jira",
        .backend_key = "PROJ-12",
        .created_seq = 1,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });

    {
        var h = Harness.initWith(gpa, &.{"PROJ-12"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ PROJ-12 · Jira Epic   [EPIC · OPEN]
            \\Origin: jira (PROJ-12)
            \\Created: 2026-04-21 · Updated: 2026-04-21
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "show: resolves via Alias" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const zqlite = @import("zqlite");
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket-id",
        .display = "GH#42",
        .title = "A Backend Ticket",
        .priority = "P2",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "42",
        .created_seq = 1,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });
    try TmpStore.insertAlias(conn, "project-1", "ticket-id");

    {
        var h = Harness.initWith(gpa, &.{"project-1"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ GH#42 · A Backend Ticket   [● P2 · OPEN]
            \\Origin: github (#42) · Kind: task
            \\Created: 2026-04-21 · Updated: 2026-04-21
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "show: reports an unknown id as exit 1 with a diagnostic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    {
        var h = Harness.initWith(gpa, &.{"no-such-id"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(
            messages.show_id_not_found_prefix ++ "no-such-id" ++ messages.show_id_not_found_suffix ++ "\n",
            h.stderr(),
        );
    }
}

test "show: reports missing store after successful Git discovery" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    var h = Harness.initWith(gpa, &.{"project-1"}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.show_missing_store ++ "\n", h.stderr());
}

test "show: requires a positional id" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.show_id_required ++ "\n", h.stderr());
}

test "show: --help prints usage and exits 0" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"--help"});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    const out = h.stdout();
    try std.testing.expect(std.mem.indexOf(u8, out, "tk show") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "Usage:") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "<id>") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "-h, --help") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}
