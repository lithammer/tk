//! `tk show` — render one Ticket or Epic with current state.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const ItemClass = @import("../domain/item_class.zig").ItemClass;
const ItemStatus = @import("../domain/status.zig").ItemStatus;
const Priority = @import("../domain/priority.zig").Priority;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;
const init_command = @import("init.zig");
const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
const styler_mod = @import("../render/styler.zig");
const palette = @import("../render/palette.zig");
const Style = @import("../render/style.zig").Style;

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

    render(deps.stdout, detail, deps.styler.forStdout()) catch {};
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

/// Render a full item view to `stdout`.
///
/// Layout: a label line and an indented facet bar, then sections
/// separated by one blank line.
///
///   <status-glyph> <display-id> · <title>
///     <P_> · <kind> · <created> → <updated>    (Tickets)
///     EPIC · <created> → <updated>             (Epics)
///
/// The shape drops tokens that were redundant: the status word
/// duplicates the glyph; the `●` dot duplicates the priority text;
/// the Display ID format conveys Local-vs-Backend so a separate
/// Origin row is just restating `GH#9` / `PROJ-12`. The Origin
/// elision relies on tk's v1 single-Remote invariant — when one
/// Repository Store may hold rows from more than one Remote at a
/// time, the layout will need a Backend kind token (see ADR holding
/// area on the Mutation-failure / multi-Remote work).
///
/// Section order: DESCRIPTION, PARENT/TICKETS, BLOCKED BY, BLOCKING,
/// EXTERNAL BLOCKERS. Empty sections are omitted. Sections are separated
/// by one blank line. The output ends with a single trailing newline.
///
/// Styling is mode-gated by `styler`: when the resolved stdout mode is
/// `.no_color` the wrap/open/close calls emit empty bytes, so non-TTY
/// output stays byte-identical to plain. The styled palette follows
/// `tk list` parity (status glyph colours, priority text, `bug` red,
/// `EPIC` magenta, `(EPIC)` sub-row prefix) plus bold on section
/// headers and the main label title. See palette.zig and ADR 0014.
fn render(stdout: *std.Io.Writer, detail: repository.ItemDetail, styler: styler_mod.SubStyler) !void {
    // Label line: <status-glyph> <display-id> · <title>
    try styler.wrap(statusStyle(detail.status), detail.status.glyph()).format(stdout);
    try stdout.writeAll(" ");
    try styler.wrap(idStyle(detail.item_class), detail.display_id).format(stdout);
    try stdout.writeAll(" · ");
    try styler.wrap(palette.header, detail.title).format(stdout);
    try stdout.writeAll("\n");

    // Facet bar: indented two spaces. Ticket form carries priority and
    // kind; Epic form carries the magenta `EPIC` token in their place.
    // Both forms end with `<created> → <updated>`.
    try stdout.writeAll("  ");
    switch (detail.item_class) {
        .epic => try styler.wrap(palette.kind_epic, "EPIC").format(stdout),
        .ticket => {
            const priority = detail.priority orelse unreachable;
            try styler.wrap(priorityStyle(priority), priority.text()).format(stdout);
            try stdout.writeAll(" · ");
            const kind = detail.ticket_kind orelse unreachable;
            switch (kind) {
                .bug => try styler.wrap(palette.kind_bug, kind.text()).format(stdout),
                .task => try stdout.writeAll(kind.text()),
            }
        },
    }
    const created_date = detail.created_at[0..@min(10, detail.created_at.len)];
    const updated_date = detail.updated_at[0..@min(10, detail.updated_at.len)];
    try stdout.print(" · {s} → {s}\n", .{ created_date, updated_date });

    // Track whether we have printed a section yet (for blank-line separators).
    var has_section = false;

    // DESCRIPTION section.
    if (detail.body.len > 0) {
        try stdout.writeAll("\n");
        try writeSectionHeader(stdout, styler, messages.show_section_description);
        try stdout.writeAll(detail.body);
        if (detail.body[detail.body.len - 1] != '\n') {
            try stdout.writeAll("\n");
        }
        has_section = true;
    }

    // PARENT section (for Tickets with a container Epic).
    if (detail.parent) |p| {
        if (has_section) try stdout.writeAll("\n");
        try writeSectionHeader(stdout, styler, messages.show_section_parent);
        try renderSubRow(stdout, "↑", p, styler);
        has_section = true;
    }

    // TICKETS section (for Epics with children).
    if (detail.children.len > 0) {
        if (has_section) try stdout.writeAll("\n");
        try writeSectionHeader(stdout, styler, messages.show_section_tickets);
        for (detail.children) |child| {
            try renderSubRow(stdout, "↓", child, styler);
        }
        has_section = true;
    }

    // BLOCKED BY section.
    if (detail.blocked_by.len > 0) {
        if (has_section) try stdout.writeAll("\n");
        try writeSectionHeader(stdout, styler, messages.show_section_blocked_by);
        for (detail.blocked_by) |item| {
            try renderSubRow(stdout, "→", item, styler);
        }
        has_section = true;
    }

    // BLOCKING section.
    if (detail.blocking.len > 0) {
        if (has_section) try stdout.writeAll("\n");
        try writeSectionHeader(stdout, styler, messages.show_section_blocking);
        for (detail.blocking) |item| {
            try renderSubRow(stdout, "→", item, styler);
        }
        has_section = true;
    }

    // EXTERNAL BLOCKERS section.
    if (detail.external_blockers.len > 0) {
        if (has_section) try stdout.writeAll("\n");
        try writeSectionHeader(stdout, styler, messages.show_section_external_blockers);
        for (detail.external_blockers) |eb| {
            try stdout.print("  • {s}\n", .{eb.reason});
        }
    }
}

/// Write a section label (e.g. "DESCRIPTION") followed by a newline,
/// wrapped in the `palette.header` style so it renders bold under
/// `.escape_codes` and plain under `.no_color`.
fn writeSectionHeader(stdout: *std.Io.Writer, styler: styler_mod.SubStyler, label: []const u8) !void {
    try styler.wrap(palette.header, label).format(stdout);
    try stdout.writeAll("\n");
}

/// Render one sub-row line with the given direction glyph.
///
/// Shape: `  <glyph> <status-glyph> <display-id>: [(EPIC) ]<title>[ ● <priority>]`
///
/// Per-token styling mirrors `tk list`'s row palette: status glyph
/// coloured per status, `(EPIC)` prefix in `kind_epic`, priority dot +
/// text in the matching priority Style. Display ID passes through
/// `id_epic` / `id_ticket` which are currently `style.none()`.
fn renderSubRow(stdout: *std.Io.Writer, glyph: []const u8, item: repository.ItemSummary, styler: styler_mod.SubStyler) !void {
    try stdout.writeAll("  ");
    try stdout.writeAll(glyph);
    try stdout.writeAll(" ");
    try styler.wrap(statusStyle(item.status), item.status.glyph()).format(stdout);
    try stdout.writeAll(" ");
    try styler.wrap(idStyle(item.item_class), item.display_id).format(stdout);
    try stdout.writeAll(": ");
    if (item.item_class == .epic) {
        try styler.wrap(palette.kind_epic, "(EPIC)").format(stdout);
        try stdout.writeAll(" ");
    }
    try stdout.writeAll(item.title);
    if (item.priority) |p| {
        const p_st = priorityStyle(p);
        try stdout.writeAll(" ");
        try styler.wrap(p_st, "●").format(stdout);
        try stdout.writeAll(" ");
        try styler.wrap(p_st, p.text()).format(stdout);
    }
    try stdout.writeAll("\n");
}

/// Map an `ItemStatus` to the palette `Style` used for its glyph on
/// the label line and in every sub-row.
fn statusStyle(status: ItemStatus) Style {
    return switch (status) {
        .open => palette.status_open,
        .active => palette.status_active,
        .done => palette.status_done,
    };
}

/// Map a `Priority` to its palette `Style`. Mirrors `list.zig`.
fn priorityStyle(priority: Priority) Style {
    return switch (priority) {
        .P0 => palette.priority_p0,
        .P1 => palette.priority_p1,
        .P2 => palette.priority_p2,
        .P3 => palette.priority_p3,
        .P4 => palette.priority_p4,
    };
}

/// Map an `ItemClass` to the palette `Style` used for its Display ID.
/// Both entries are `style.none()` today; passing them through keeps
/// `tk show` symmetric with `tk list` for future palette changes.
fn idStyle(item_class: ItemClass) Style {
    return switch (item_class) {
        .epic => palette.id_epic,
        .ticket => palette.id_ticket,
    };
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
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
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
        var h = Harness.init(gpa, &.{"project-1"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ project-1 · First Ticket
            \\  P2 · task · 2026-04-21 → 2026-04-21
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
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
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
        var h = Harness.init(gpa, &.{"project-2"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ project-2 · Render ready list
            \\  P1 · task · 2026-04-21 → 2026-04-28
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
        var h = Harness.init(gpa, &.{"project-1"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ project-1 · Ship list command
            \\  EPIC · 2026-04-21 → 2026-04-28
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

test "show: renders Backend Ticket with the backend Display ID" {
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
        var h = Harness.init(gpa, &.{"GH#9"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ GH#9 · Backend task
            \\  P1 · task · 2026-04-21 → 2026-04-21
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "show: renders Backend Epic with the backend Display ID" {
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
        var h = Harness.init(gpa, &.{"PROJ-12"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ PROJ-12 · Jira Epic
            \\  EPIC · 2026-04-21 → 2026-04-21
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
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
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
        var h = Harness.init(gpa, &.{"project-1"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\○ GH#42 · A Backend Ticket
            \\  P2 · task · 2026-04-21 → 2026-04-21
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
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    {
        var h = Harness.init(gpa, &.{"no-such-id"}, .{ .cwd = cwd });
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

    var h = Harness.init(gpa, &.{"project-1"}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.show_missing_store ++ "\n", h.stderr());
}

test "show: requires a positional id" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.show_id_required ++ "\n", h.stderr());
}

test "show: --help prints usage and exits 0" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"--help"}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    const out = h.stdout();
    try std.testing.expect(std.mem.indexOf(u8, out, "tk show") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "Usage:") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "<id>") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "-h, --help") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "show: renders styled output with correct ANSI sequences under escape_codes" {
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

    // Epic parent (project-1) — exercises the magenta `(EPIC)` prefix
    // in the PARENT sub-row.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Ship list command",
        .created_seq = 1,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });
    // Main item (project-2) — P0 bug, ACTIVE. Exercises bold title,
    // yellow active label-line glyph, red `P0` and red `bug` tokens
    // on the facet bar, and the `created → updated` date span.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "main",
        .display = "project-2",
        .title = "Active bug",
        .priority = "P0",
        .ticket_kind = "bug",
        .status = "active",
        .body = "Body so the DESCRIPTION section renders.",
        .container_id = "epic",
        .created_seq = 2,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });
    // BLOCKED BY (project-3) — ACTIVE P1 Ticket. Exercises yellow
    // active glyph and yellow P1 priority dot + text in a sub-row.
    // `done` status is filtered out of BLOCKED BY / BLOCKING by
    // `repository.showItem`; the done glyph is covered separately by
    // the Epic-main-header test below.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocker",
        .display = "project-3",
        .title = "Active blocker",
        .priority = "P1",
        .status = "active",
        .created_seq = 3,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });
    try TmpStore.insertDependency(conn, "blocker", "main");
    // BLOCKING (project-9) — open P3 Ticket. Priority_p3 is `none()`
    // so this row asserts negative styling on a sub-row by sitting
    // alongside the styled ones (no SGR around the P3 dot/text).
    try TmpStore.insertFixtureItem(conn, .{
        .id = "downstream",
        .display = "project-9",
        .title = "Open downstream",
        .priority = "P3",
        .created_seq = 9,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });
    try TmpStore.insertDependency(conn, "main", "downstream");
    // One unresolved external blocker so the EXTERNAL BLOCKERS header
    // renders (its label is the only styled span in that section).
    try conn.exec(
        \\insert into external_blockers(id, item_id, reason, created_at, resolved_at)
        \\values (?1, ?2, ?3, '2026-04-21T00:00:00.000Z', null)
    , .{ "eb-open", "main", "Need legal review" });

    var h = Harness.init(gpa, &.{"project-2"}, .{
        .cwd = cwd,
        .stdout_mode = .escape_codes,
    });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));

    const out = h.stdout();

    // Bold section headers: SGR 1 .. 22 around each label.
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[1mDESCRIPTION\x1b[22m") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[1mPARENT\x1b[22m") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[1mBLOCKED BY\x1b[22m") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[1mBLOCKING\x1b[22m") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[1mEXTERNAL BLOCKERS\x1b[22m") != null);

    // Label line: bold title.
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[1mActive bug\x1b[22m") != null);

    // Label line: yellow status glyph (◐ = \xe2\x97\x90) before the Display ID.
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[33m\xe2\x97\x90\x1b[39m project-2") != null);

    // Facet bar (indented two spaces): red `P0`, separator, red `bug`.
    // The combined pattern pins position too — order swaps would fail.
    try std.testing.expect(std.mem.indexOf(u8, out, "\n  \x1b[31mP0\x1b[39m \xc2\xb7 \x1b[31mbug\x1b[39m") != null);

    // PARENT sub-row: magenta `(EPIC)` prefix.
    try std.testing.expect(std.mem.indexOf(u8, out, ": \x1b[35m(EPIC)\x1b[39m Ship list command") != null);

    // BLOCKED BY sub-row: yellow active glyph (◐ = \xe2\x97\x90) and yellow P1.
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[33m\xe2\x97\x90\x1b[39m project-3") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, " \x1b[33m\xe2\x97\x8f\x1b[39m \x1b[33mP1\x1b[39m") != null);
}

test "show: Epic facet bar renders magenta EPIC token under escape_codes" {
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

    // Done Epic so the label-line glyph (✓) renders in green. The
    // facet bar for an Epic carries `EPIC` (magenta) in place of the
    // priority+kind tokens a Ticket would emit. This is the only test
    // that asserts the magenta `EPIC` token on the Epic facet bar.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .status = "done",
        .title = "Finished epic",
        .created_seq = 1,
        .created_at = "2026-04-21T00:00:00.000Z",
        .updated_at = "2026-04-21T00:00:00.000Z",
    });

    var h = Harness.init(gpa, &.{"project-1"}, .{
        .cwd = cwd,
        .stdout_mode = .escape_codes,
    });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));

    const out = h.stdout();

    // Label line: green done glyph (✓ = \xe2\x9c\x93) before the Display ID.
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[32m\xe2\x9c\x93\x1b[39m project-1") != null);
    // Label line: bold title.
    try std.testing.expect(std.mem.indexOf(u8, out, "\x1b[1mFinished epic\x1b[22m") != null);
    // Facet bar: magenta `EPIC` token at the start of the indented row.
    try std.testing.expect(std.mem.indexOf(u8, out, "\n  \x1b[35mEPIC\x1b[39m \xc2\xb7 2026-04-21") != null);
}
