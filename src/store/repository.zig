//! Repository Store open and write helpers.

const std = @import("std");
const zqlite = @import("zqlite");
const clock_mod = @import("../clock.zig");
const discovery = @import("../git/discovery.zig");
const proc = @import("../proc/runner.zig");
const migrations = @import("migrations.zig");
const Priority = @import("../domain/priority.zig").Priority;
const ItemStatus = @import("../domain/status.zig").ItemStatus;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;
const Origin = @import("../domain/origin.zig").Origin;
const ItemClass = @import("../domain/item_class.zig").ItemClass;

/// Open Repository Store connection. Call `close` when done.
pub const Store = struct {
    conn: zqlite.Conn,

    /// Close the underlying SQLite connection.
    pub fn close(self: Store) void {
        self.conn.close();
    }
};

/// Result of creating a local Ticket.
pub const CreatedTicket = struct {
    id: []u8,
    display_id: []u8,
    kind: TicketKind,
    priority: Priority,
    status: ItemStatus,
    origin: Origin,
    title: []const u8,
    body: []const u8,

    /// Free generated values owned by the store helper.
    pub fn deinit(self: CreatedTicket, gpa: std.mem.Allocator) void {
        gpa.free(self.id);
        gpa.free(self.display_id);
    }
};

/// Input for local Ticket creation.
pub const CreateLocalTicketInput = struct {
    kind: TicketKind,
    priority: Priority,
    title: []const u8,
    body: []const u8,
};

/// One current-state row for the `tk list` List Tree.
///
/// The Repository Store owns filtering and ordering. The command renderer owns
/// the final tree glyphs and compact plain-text row shape.
pub const ListRow = struct {
    id: []u8,
    display_id: []u8,
    item_class: ItemClass,
    ticket_kind: ?TicketKind,
    priority: ?Priority,
    title: []u8,
    status: ItemStatus,
    origin: Origin,
    container_id: ?[]u8,
    created_seq: i64,

    /// Free text copied out of SQLite's statement-owned row buffers.
    pub fn deinit(self: ListRow, gpa: std.mem.Allocator) void {
        gpa.free(self.id);
        gpa.free(self.display_id);
        gpa.free(self.title);
        if (self.container_id) |container_id| gpa.free(container_id);
    }
};

/// Free a slice returned by `listRows`.
pub fn freeListRows(gpa: std.mem.Allocator, rows: []ListRow) void {
    for (rows) |row| row.deinit(gpa);
    gpa.free(rows);
}

/// Item-selection mode for `tk list`.
pub const ListView = enum {
    default,
    all,
    ready,
    blocked,
    active,

    fn sqlText(self: ListView) []const u8 {
        return switch (self) {
            .default => "default",
            .all => "all",
            .ready => "ready",
            .blocked => "blocked",
            .active => "active",
        };
    }
};

/// Stored-Origin filter for `tk list`.
pub const ListOriginFilter = enum {
    any,
    local,
    remote,

    fn sqlText(self: ListOriginFilter) []const u8 {
        return switch (self) {
            .any => "any",
            .local => "local",
            .remote => "backend",
        };
    }
};

/// Read options for the List Tree query.
pub const ListOptions = struct {
    view: ListView = .default,
    origin: ListOriginFilter = .any,
};

/// Outcome of opening an existing Repository Store.
///
/// `discovery_failed` wraps a `discovery.Outcome` whose inner tag is one of
/// the failure variants (never `.ok`); render it with `discovery.renderFailure`
/// so the four shared Git-failure shapes stay in one place across commands.
/// The three store-specific arms carry no payload. `.ok` owns an open
/// connection that must be closed through `Store.close`.
pub const OpenOutcome = union(enum) {
    ok: Store,
    discovery_failed: discovery.Outcome,
    store_missing,
    not_ticket_store,
    store_from_future_version,
};

pub const OpenError = discovery.Error || migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Open the existing Repository Store for the current Git repository.
pub fn openExisting(gpa: std.mem.Allocator, runner: proc.Runner, cwd: std.Io.Dir) OpenError!OpenOutcome {
    const outcome = try discovery.discoverPaths(gpa, runner, cwd);
    var paths = switch (outcome) {
        .ok => |ok| ok,
        else => return .{ .discovery_failed = outcome },
    };
    defer paths.deinit(gpa);

    const db_path = try std.fs.path.joinZ(gpa, &.{ paths.git_common_dir, "tk", "ticket.db" });
    defer gpa.free(db_path);

    const conn = zqlite.open(db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode) catch |err| switch (err) {
        error.CantOpen => return .store_missing,
        else => return err,
    };
    errdefer conn.close();

    try conn.busyTimeout(5000);
    try conn.execNoArgs("pragma foreign_keys = on");

    const app_id = (try migrations.queryOptionalInt(conn, "pragma application_id")) orelse 0;
    if (app_id != migrations.application_id) {
        conn.close();
        return .not_ticket_store;
    }

    const version = try migrations.currentVersion(conn);
    if (version > migrations.all_migrations[migrations.all_migrations.len - 1].version) {
        conn.close();
        return .store_from_future_version;
    }

    return .{ .ok = .{ .conn = conn } };
}

pub const CreateError = migrations.QueryError || zqlite.Error || error{OutOfMemory};
pub const ListError = migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Read current Repository Store rows for a List Tree.
pub fn listRows(store: Store, gpa: std.mem.Allocator, options: ListOptions) ListError![]ListRow {
    var result: std.ArrayList(ListRow) = .empty;
    errdefer {
        for (result.items) |row| row.deinit(gpa);
        result.deinit(gpa);
    }

    var rows = try store.conn.rows(
        \\with annotated as (
        \\    select i.*,
        \\           exists (
        \\               select 1
        \\                 from dependencies d
        \\                 join items blocking on blocking.id = d.blocking_id
        \\                where d.blocked_id = i.id
        \\                  and blocking.status <> 'done'
        \\           ) as has_unresolved_dependency,
        \\           exists (
        \\               select 1
        \\                 from external_blockers eb
        \\                where eb.item_id = i.id
        \\                  and eb.resolved_at is null
        \\           ) as has_unresolved_external_blocker
        \\      from items i
        \\),
        \\matching as (
        \\    select *,
        \\           case ?1
        \\             when 'default' then status in ('open', 'active')
        \\             when 'all' then 1
        \\             when 'ready' then item_class = 'ticket'
        \\                               and status = 'open'
        \\                               and not has_unresolved_dependency
        \\                               and not has_unresolved_external_blocker
        \\             when 'blocked' then item_class = 'ticket'
        \\                                 and status in ('open', 'active')
        \\                                 and (
        \\                                     has_unresolved_dependency
        \\                                     or has_unresolved_external_blocker
        \\                                 )
        \\             when 'active' then status = 'active'
        \\           end as self_matches
        \\      from annotated
        \\)
        \\select id, display_value, item_class, ticket_kind, priority, title,
        \\       status, origin, container_id, created_seq
        \\  from matching parent
        \\ where (?2 = 'any' or parent.origin = ?2)
        \\   and (
        \\       parent.self_matches
        \\       or (
        \\           ?1 in ('ready', 'blocked', 'active')
        \\           and
        \\           parent.item_class = 'epic'
        \\           and exists (
        \\               select 1
        \\                 from matching child
        \\                where child.container_id = parent.id
        \\                  and child.self_matches
        \\                  and (?2 = 'any' or child.origin = ?2)
        \\           )
        \\       )
        \\   )
        \\ order by created_seq asc
    , .{ options.view.sqlText(), options.origin.sqlText() });
    defer rows.deinit();

    while (rows.next()) |row| {
        try result.append(gpa, try listRowFromSql(gpa, row));
    }
    if (rows.err) |err| return err;

    return result.toOwnedSlice(gpa);
}

/// Create a Local Ticket and its current Display ID resolver row.
pub fn createLocalTicket(
    store: Store,
    gpa: std.mem.Allocator,
    clock: clock_mod.Clock,
    random: std.Random,
    input: CreateLocalTicketInput,
) CreateError!CreatedTicket {
    const id = try generateInternalId(gpa, random);
    errdefer gpa.free(id);

    var iso_buf: [24]u8 = undefined;
    const now = clock.nowIso(&iso_buf);

    try store.conn.execNoArgs("begin immediate");
    errdefer store.conn.rollback();

    const display_seq = try nextSequence(store.conn, "display_seq");
    const created_seq = try nextSequence(store.conn, "item_created_seq");
    const prefix = try queryTextAlloc(store.conn, gpa, "select value from store_config where key = 'display_prefix'");
    defer gpa.free(prefix);
    const display_id = try std.fmt.allocPrint(gpa, "{s}-{d}", .{ prefix, display_seq });
    errdefer gpa.free(display_id);

    try store.conn.exec(
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
    , .{
        id,
        display_id,
        ItemClass.ticket.text(),
        input.kind.text(),
        input.priority.text(),
        input.title,
        input.body,
        Origin.local.text(),
        ItemStatus.default.text(),
        created_seq,
        now,
    });
    try store.conn.exec(
        "insert into item_ids(value, source, item_id, created_at) values (?1, 'display', ?2, ?3)",
        .{ display_id, id, now },
    );

    try store.conn.commit();

    return .{
        .id = id,
        .display_id = display_id,
        .kind = input.kind,
        .priority = input.priority,
        .status = ItemStatus.default,
        .origin = Origin.local,
        .title = input.title,
        .body = input.body,
    };
}

fn nextSequence(conn: zqlite.Conn, name: []const u8) migrations.QueryError!i64 {
    if (try conn.row("update sequences set value = value + 1 where name = ?1 returning value", .{name})) |r| {
        defer r.deinit();
        return r.int(0);
    }
    return error.Notfound;
}

fn queryTextAlloc(conn: zqlite.Conn, gpa: std.mem.Allocator, sql: []const u8) (migrations.QueryError || error{OutOfMemory})![]u8 {
    if (try conn.row(sql, .{})) |r| {
        defer r.deinit();
        return gpa.dupe(u8, r.text(0));
    }
    return error.Notfound;
}

fn generateInternalId(gpa: std.mem.Allocator, random: std.Random) ![]u8 {
    var bytes: [16]u8 = undefined;
    random.bytes(&bytes);
    const hex = std.fmt.bytesToHex(bytes, .lower);
    return gpa.dupe(u8, &hex);
}

fn listRowFromSql(gpa: std.mem.Allocator, row: zqlite.Row) ListError!ListRow {
    const id = try gpa.dupe(u8, row.text(0));
    errdefer gpa.free(id);
    const display_id = try gpa.dupe(u8, row.text(1));
    errdefer gpa.free(display_id);
    const title = try gpa.dupe(u8, row.text(5));
    errdefer gpa.free(title);
    const container_id = if (row.nullableText(8)) |text| try gpa.dupe(u8, text) else null;
    errdefer if (container_id) |owned| gpa.free(owned);

    return .{
        .id = id,
        .display_id = display_id,
        .item_class = parseItemClass(row.text(2)),
        .ticket_kind = if (row.nullableText(3)) |text| parseTicketKind(text) else null,
        .priority = if (row.nullableText(4)) |text| parsePriority(text) else null,
        .title = title,
        .status = parseItemStatus(row.text(6)),
        .origin = parseOrigin(row.text(7)),
        .container_id = container_id,
        .created_seq = row.int(9),
    };
}

fn parseItemClass(text: []const u8) ItemClass {
    if (std.mem.eql(u8, text, "ticket")) return .ticket;
    if (std.mem.eql(u8, text, "epic")) return .epic;
    unreachable;
}

fn parseTicketKind(text: []const u8) TicketKind {
    if (std.mem.eql(u8, text, "task")) return .task;
    if (std.mem.eql(u8, text, "bug")) return .bug;
    unreachable;
}

fn parsePriority(text: []const u8) Priority {
    if (std.mem.eql(u8, text, "P0")) return .P0;
    if (std.mem.eql(u8, text, "P1")) return .P1;
    if (std.mem.eql(u8, text, "P2")) return .P2;
    if (std.mem.eql(u8, text, "P3")) return .P3;
    if (std.mem.eql(u8, text, "P4")) return .P4;
    unreachable;
}

fn parseItemStatus(text: []const u8) ItemStatus {
    if (std.mem.eql(u8, text, "open")) return .open;
    if (std.mem.eql(u8, text, "active")) return .active;
    if (std.mem.eql(u8, text, "done")) return .done;
    unreachable;
}

fn parseOrigin(text: []const u8) Origin {
    if (std.mem.eql(u8, text, "local")) return .local;
    if (std.mem.eql(u8, text, "backend")) return .backend;
    unreachable;
}

test "listRows: ready and blocked derive from unresolved blockers" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{ .id = "ready", .display = "tk-1", .title = "Ready", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked-dep", .display = "tk-2", .title = "Blocked by Dependency", .created_seq = 2 });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "open-blocker",
        .display = "tk-3",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Open blocker",
        .created_seq = 3,
    });
    try TmpStore.insertDependency(conn, "open-blocker", "blocked-dep");
    try TmpStore.insertFixtureItem(conn, .{ .id = "unblocked-dep", .display = "tk-4", .title = "Unblocked by done Dependency", .created_seq = 4 });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "done-blocker",
        .display = "tk-5",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Done blocker",
        .status = "done",
        .created_seq = 5,
    });
    try TmpStore.insertDependency(conn, "done-blocker", "unblocked-dep");
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked-external", .display = "tk-6", .title = "Blocked externally", .created_seq = 6 });
    try TmpStore.insertExternalBlocker(conn, "external-open", "blocked-external", null);
    try TmpStore.insertFixtureItem(conn, .{ .id = "resolved-external", .display = "tk-7", .title = "Resolved external blocker", .created_seq = 7 });
    try TmpStore.insertExternalBlocker(conn, "external-done", "resolved-external", "2026-05-10T00:00:00.000Z");

    const ready = try listRows(store, gpa, .{ .view = .ready });
    defer freeListRows(gpa, ready);
    try expectDisplayIds(ready, &.{ "tk-1", "tk-4", "tk-7" });

    const blocked = try listRows(store, gpa, .{ .view = .blocked });
    defer freeListRows(gpa, blocked);
    try expectDisplayIds(blocked, &.{ "tk-2", "tk-6" });
}

fn expectDisplayIds(rows: []const ListRow, expected: []const []const u8) !void {
    try std.testing.expectEqual(expected.len, rows.len);
    for (expected, 0..) |display_id, i| {
        try std.testing.expectEqualStrings(display_id, rows[i].display_id);
    }
}
