//! Repository Store open and write helpers.

const std = @import("std");
const zqlite = @import("zqlite");
const clock_mod = @import("../clock.zig");
const discovery = @import("../git/discovery.zig");
const messages = @import("../messages.zig");
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
        return @tagName(self);
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

/// One ready Ticket selected by `tk next`.
///
/// The Repository Store owns readiness, Workspace Scope interpretation after
/// the caller supplies a scope argument, Priority ordering, and creation-order
/// tie breaks. The command renderer owns the compact stdout row, so this
/// payload is narrowed to the three fields the row prints.
pub const NextTicket = struct {
    display_id: []u8,
    priority: Priority,
    title: []u8,

    /// Free text copied out of SQLite's statement-owned row buffers.
    pub fn deinit(self: NextTicket, gpa: std.mem.Allocator) void {
        gpa.free(self.display_id);
        gpa.free(self.title);
    }
};

/// Workspace Scope input for ready-Ticket selection.
///
/// `tk worktree` is responsible for discovering configured or inferred
/// Workspace Scope. Repository Store reads resolve the resulting Display ID or
/// Alias against `item_ids` so Promotion does not break old scope references.
pub const NextScope = union(enum) {
    none,
    display_arg: []const u8,
};

/// Read options for selecting the next ready Ticket.
pub const NextOptions = struct {
    scope: NextScope = .none,
    ignore_scope: bool = false,
};

/// Result of selecting the next ready Ticket.
///
/// `.ticket` owns copied row fields; callers must free that payload in the
/// switch arm that receives it.
pub const NextOutcome = union(enum) {
    ticket: NextTicket,
    no_ready_ticket,
    scope_not_found,
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

/// Render a non-`.ok` `OpenOutcome` as a command-prefixed stderr diagnostic.
///
/// The four failure arms share identical phrasing across commands; only the
/// command-prefixed missing-store sentence varies. Callers pass the
/// already-prefixed missing-store message (e.g. `messages.list_missing_store`)
/// so `messages.zig` remains the single source of stable strings.
pub fn renderOpenFailure(
    stderr: *std.Io.Writer,
    gpa: std.mem.Allocator,
    command_name: []const u8,
    missing_store: []const u8,
    outcome: OpenOutcome,
) void {
    switch (outcome) {
        .ok => unreachable,
        .discovery_failed => |inner| discovery.renderFailure(stderr, gpa, command_name, inner),
        .store_missing => stderr.print("{s}\n", .{missing_store}) catch {},
        .not_ticket_store => stderr.print("tk {s}: Repository Store is {s}\n", .{ command_name, messages.init_refuse_foreign }) catch {},
        .store_from_future_version => stderr.print("tk {s}: Repository Store was created by a {s}\n", .{ command_name, messages.init_refuse_future_version }) catch {},
    }
}

/// Command-prefixed message constants used by `renderStorageError`.
///
/// `fallback` is the generic non-transient diagnostic; the caller appends
/// `"\n{s}\n"` for `@errorName(err)`. The three messages stay command-specific
/// so `messages.zig` remains the single source of truth for stable strings.
pub const StorageErrorMessages = struct {
    busy_retry: []const u8,
    out_of_memory: []const u8,
    fallback: []const u8,
};

/// Render a Repository Store read or write failure as a stderr diagnostic.
///
/// Busy/locked errors and `error.OutOfMemory` get dedicated phrasing so we
/// only suggest a retry when one is plausible; everything else falls through
/// to `fallback ++ "\n{s}\n"` carrying the underlying `@errorName`.
pub fn renderStorageError(stderr: *std.Io.Writer, err: anyerror, msgs: StorageErrorMessages) void {
    if (isBusyError(err)) {
        stderr.print("{s}\n", .{msgs.busy_retry}) catch {};
        return;
    }
    if (err == error.OutOfMemory) {
        stderr.print("{s}\n", .{msgs.out_of_memory}) catch {};
        return;
    }
    stderr.print("{s}\n{s}\n", .{ msgs.fallback, @errorName(err) }) catch {};
}

/// Classify a Repository Store error as a transient SQLite busy/locked state
/// that a retry can clear. Shared by commands so the retry contract stays
/// uniform across writes and reads.
pub fn isBusyError(err: anyerror) bool {
    return switch (err) {
        error.Busy,
        error.BusyRecovery,
        error.BusySnapshot,
        error.BusyTimeout,
        error.Locked,
        error.LockedSharedCache,
        error.LockedVTab,
        => true,
        else => false,
    };
}

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
pub const NextError = migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Shared current-state annotation used by List Tree and next-Ticket reads.
///
/// Keeping readiness and blocking derivation in one CTE gives `tk list` and
/// `tk next` the same Repository Store semantics without a command-side copy.
const annotated_current_items_cte =
    \\with annotated as (
    \\    select i.id, i.display_value, i.item_class, i.ticket_kind,
    \\           i.priority, i.title, i.status, i.origin, i.container_id,
    \\           i.created_seq,
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
    \\)
;

/// SQL for the List Tree read. Bound with two text parameters:
///   `?1` — `ListView.sqlText` selecting which items match.
///   `?2` — `ListOriginFilter.sqlText` filtering stored Origin.
///
/// The `case ?1 when '<tag>' then ...` arms must cover every `ListView` tag.
/// A regression test at the bottom of this file enforces that contract.
const list_rows_sql = annotated_current_items_cte ++
    \\,
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
;

/// SQL for `tk next`. Bound with:
///   `?1` — scope mode: `all`, `ticket`, or `epic`.
///   `?2` — internal stable ID for the scoped Ticket or Epic.
const next_ready_ticket_sql = annotated_current_items_cte ++ "\n" ++
    \\select display_value, priority, title
    \\  from annotated
    \\ where item_class = 'ticket'
    \\   and status = 'open'
    \\   and not has_unresolved_dependency
    \\   and not has_unresolved_external_blocker
    \\   and (
    \\       ?1 = 'all'
    \\       or (?1 = 'ticket' and id = ?2)
    \\       or (?1 = 'epic' and container_id = ?2)
    \\   )
    \\ order by priority asc, created_seq asc
    \\ limit 1
;

const resolve_item_ref_sql =
    \\select i.id, i.item_class
    \\  from item_ids ids
    \\  join items i on i.id = ids.item_id
    \\ where ids.value = ?1
;

/// Read current Repository Store rows for a List Tree.
pub fn listRows(store: Store, gpa: std.mem.Allocator, options: ListOptions) ListError![]ListRow {
    var result: std.ArrayList(ListRow) = .empty;
    errdefer {
        for (result.items) |row| row.deinit(gpa);
        result.deinit(gpa);
    }

    var rows = try store.conn.rows(list_rows_sql, .{ options.view.sqlText(), options.origin.sqlText() });
    defer rows.deinit();

    while (rows.next()) |row| {
        try result.append(gpa, try listRowFromSql(gpa, row));
    }
    if (rows.err) |err| return err;

    return result.toOwnedSlice(gpa);
}

/// Select the next ready Ticket from current Repository Store state.
pub fn nextReadyTicket(store: Store, gpa: std.mem.Allocator, options: NextOptions) NextError!NextOutcome {
    var scope_mode: []const u8 = "all";
    var scope_id: []const u8 = "";

    var resolved_scope: ?ResolvedItemRef = null;
    defer if (resolved_scope) |scope| scope.deinit(gpa);

    if (!options.ignore_scope) {
        switch (options.scope) {
            .none => {},
            .display_arg => |display_arg| {
                const resolved = (try resolveItemRef(store, gpa, display_arg)) orelse return .scope_not_found;
                resolved_scope = resolved;
                scope_id = resolved.id;
                scope_mode = resolved.item_class.text();
            },
        }
    }

    if (try store.conn.row(next_ready_ticket_sql, .{ scope_mode, scope_id })) |r| {
        defer r.deinit();
        return .{ .ticket = try nextTicketFromSql(gpa, r) };
    }
    return .no_ready_ticket;
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

    return .{
        .id = id,
        .display_id = display_id,
        .item_class = enumFromText(ItemClass, row.text(2)),
        .ticket_kind = if (row.nullableText(3)) |text| enumFromText(TicketKind, text) else null,
        .priority = if (row.nullableText(4)) |text| enumFromText(Priority, text) else null,
        .title = title,
        .status = enumFromText(ItemStatus, row.text(6)),
        .origin = enumFromText(Origin, row.text(7)),
        .container_id = container_id,
        .created_seq = row.int(9),
    };
}

const ResolvedItemRef = struct {
    id: []u8,
    item_class: ItemClass,

    fn deinit(self: ResolvedItemRef, gpa: std.mem.Allocator) void {
        gpa.free(self.id);
    }
};

fn resolveItemRef(store: Store, gpa: std.mem.Allocator, display_arg: []const u8) NextError!?ResolvedItemRef {
    if (try store.conn.row(resolve_item_ref_sql, .{display_arg})) |r| {
        defer r.deinit();
        return .{
            .id = try gpa.dupe(u8, r.text(0)),
            .item_class = enumFromText(ItemClass, r.text(1)),
        };
    }
    return null;
}

fn nextTicketFromSql(gpa: std.mem.Allocator, row: zqlite.Row) NextError!NextTicket {
    const display_id = try gpa.dupe(u8, row.text(0));
    errdefer gpa.free(display_id);
    const title = try gpa.dupe(u8, row.text(2));
    return .{
        .display_id = display_id,
        .priority = enumFromText(Priority, row.text(1)),
        .title = title,
    };
}

/// Decode a SQLite text column written by the matching enum's `text()` method.
/// Any other value is a Repository Store corruption bug; the schema `check`
/// constraints prevent reaching here in practice.
fn enumFromText(comptime T: type, text: []const u8) T {
    return std.meta.stringToEnum(T, text) orelse unreachable;
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

test "nextReadyTicket: selects ready Tickets by Priority then creation order" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{ .id = "old-p2", .display = "tk-1", .title = "Older P2", .priority = "P2", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "first-p1", .display = "tk-2", .title = "First P1", .priority = "P1", .created_seq = 2 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "second-p1", .display = "tk-3", .title = "Second P1", .priority = "P1", .created_seq = 3 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "active-p0", .display = "tk-4", .title = "Active P0", .priority = "P0", .status = "active", .created_seq = 4 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked-p0", .display = "tk-5", .title = "Blocked P0", .priority = "P0", .created_seq = 5 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocker", .display = "tk-6", .title = "Blocker", .priority = "P4", .created_seq = 6 });
    try TmpStore.insertDependency(conn, "blocker", "blocked-p0");
    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic",
        .display = "tk-7",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Epic is never next",
        .created_seq = 7,
    });

    const outcome = try nextReadyTicket(store, gpa, .{});
    switch (outcome) {
        .ticket => |ticket| {
            defer ticket.deinit(gpa);
            try std.testing.expectEqualStrings("tk-2", ticket.display_id);
        },
        else => return error.UnexpectedOutcome,
    }
}

test "nextReadyTicket: applies scoped Display ID and Alias selection" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic",
        .display = "tk-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Scoped Epic",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "scoped-child",
        .display = "tk-2",
        .title = "Scoped child",
        .priority = "P3",
        .container_id = "epic",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "global-best",
        .display = "tk-3",
        .title = "Global best",
        .priority = "P0",
        .created_seq = 3,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "promoted",
        .display = "GH#42",
        .title = "Promoted Ticket",
        .priority = "P1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "42",
        .created_seq = 4,
    });
    try TmpStore.insertAlias(conn, "tk-42", "promoted");

    const scoped_epic = try nextReadyTicket(store, gpa, .{ .scope = .{ .display_arg = "tk-1" } });
    switch (scoped_epic) {
        .ticket => |ticket| {
            defer ticket.deinit(gpa);
            try std.testing.expectEqualStrings("tk-2", ticket.display_id);
        },
        else => return error.UnexpectedOutcome,
    }

    const scoped_alias = try nextReadyTicket(store, gpa, .{ .scope = .{ .display_arg = "tk-42" } });
    switch (scoped_alias) {
        .ticket => |ticket| {
            defer ticket.deinit(gpa);
            try std.testing.expectEqualStrings("GH#42", ticket.display_id);
        },
        else => return error.UnexpectedOutcome,
    }

    const ignored_scope = try nextReadyTicket(store, gpa, .{
        .scope = .{ .display_arg = "tk-1" },
        .ignore_scope = true,
    });
    switch (ignored_scope) {
        .ticket => |ticket| {
            defer ticket.deinit(gpa);
            try std.testing.expectEqualStrings("tk-3", ticket.display_id);
        },
        else => return error.UnexpectedOutcome,
    }

    const missing_scope = try nextReadyTicket(store, gpa, .{ .scope = .{ .display_arg = "missing-9" } });
    switch (missing_scope) {
        .scope_not_found => {},
        else => return error.UnexpectedOutcome,
    }
}

fn expectDisplayIds(rows: []const ListRow, expected: []const []const u8) !void {
    try std.testing.expectEqual(expected.len, rows.len);
    for (expected, 0..) |display_id, i| {
        try std.testing.expectEqualStrings(display_id, rows[i].display_id);
    }
}

test "listRows SQL has a CASE arm for every ListView tag" {
    // The Zig→SQL coupling is implicit through `@tagName`. A renamed tag would
    // compile cleanly and produce a runtime CASE miss (NULL `self_matches` →
    // an unexpectedly empty list). This guard fails the build instead.
    inline for (std.enums.values(ListView)) |tag| {
        const needle = "when '" ++ @tagName(tag) ++ "' then";
        try std.testing.expect(std.mem.indexOf(u8, list_rows_sql, needle) != null);
    }
}
