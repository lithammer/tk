//! Repository Store open and write helpers.

const std = @import("std");
const zqlite = @import("zqlite");
const clock_mod = @import("../clock.zig");
const discovery = @import("../git/discovery.zig");
const messages = @import("../messages.zig");
const proc = @import("../proc/runner.zig");
const migrations = @import("migrations.zig");
const mutations_mod = @import("mutations.zig");
const sequences = @import("sequences.zig");
const Priority = @import("../domain/priority.zig").Priority;
const ItemStatus = @import("../domain/status.zig").ItemStatus;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;
const Origin = @import("../domain/origin.zig").Origin;
const ItemClass = @import("../domain/item_class.zig").ItemClass;
const MutationType = @import("../domain/mutation_type.zig").MutationType;

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
    has_unresolved_blocker: bool,

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
/// payload is narrowed to the Display ID the command prints.
pub const NextTicket = struct {
    display_id: []u8,

    /// Free text copied out of SQLite's statement-owned row buffers.
    pub fn deinit(self: NextTicket, gpa: std.mem.Allocator) void {
        gpa.free(self.display_id);
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

/// Per-command message bundle for `openStoreCatching`. Each command declares
/// one constant at module level so the open-failure rendering pipeline is
/// driven entirely by the command's stable phrasing.
pub const OpenMessages = struct {
    /// Subcommand name as it appears in diagnostics, e.g. `"next"` or
    /// `"worktree set"`. Used by `renderOpenFailure` to format `tk <name>: …`.
    command_name: []const u8,
    /// Pre-formatted "Repository Store not initialized" line for this command.
    missing_store: []const u8,
    /// Storage-error triple used when `openExisting` raises an error.
    storage: StorageErrorMessages,
};

/// Open the Repository Store for a command, rendering the standard
/// open-failure or storage-error diagnostic on any failure. Returns the open
/// `Store` on success or `null` after the diagnostic is written; callers do
/// `openStoreCatching(...) orelse return 1;`.
///
/// `error.OutOfMemory` is rendered through `msgs.storage.out_of_memory` rather
/// than propagated. This matches every existing command's storage-error
/// handling: anticipated OOM exits 1 with a stable message; only unanticipated
/// failures (e.g. zig-clap OOM that bypasses command rendering) reach the
/// exit-3 catch-all in `main.zig`.
pub fn openStoreCatching(
    gpa: std.mem.Allocator,
    runner: proc.Runner,
    cwd: std.Io.Dir,
    stderr: *std.Io.Writer,
    msgs: OpenMessages,
) ?Store {
    const outcome = openExisting(gpa, runner, cwd) catch |err| {
        renderStorageError(stderr, err, msgs.storage);
        return null;
    };
    switch (outcome) {
        .ok => |store| return store,
        else => {
            renderOpenFailure(stderr, gpa, msgs.command_name, msgs.missing_store, outcome);
            return null;
        },
    }
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

/// Compact summary of a related item for the `tk show` sub-section rows.
///
/// Used for PARENT (the container Epic), TICKETS (children of an Epic),
/// BLOCKED BY, and BLOCKING. All fields are allocator-owned by the caller.
pub const ItemSummary = struct {
    display_id: []u8,
    title: []u8,
    item_class: ItemClass,
    status: ItemStatus,
    priority: ?Priority,

    /// Free allocator-owned fields.
    pub fn deinit(self: ItemSummary, gpa: std.mem.Allocator) void {
        gpa.free(self.display_id);
        gpa.free(self.title);
    }
};

/// One unresolved External Blocker rendered in `tk show`.
pub const ExternalBlockerSummary = struct {
    reason: []u8,

    /// Free allocator-owned fields.
    pub fn deinit(self: ExternalBlockerSummary, gpa: std.mem.Allocator) void {
        gpa.free(self.reason);
    }
};

/// Full current-state view of one Ticket or Epic for `tk show`.
///
/// Allocator-owned slices; free with `deinit`.
pub const ItemDetail = struct {
    id: []u8,
    display_id: []u8,
    item_class: ItemClass,
    ticket_kind: ?TicketKind,
    priority: ?Priority,
    title: []u8,
    body: []u8,
    status: ItemStatus,
    origin: Origin,
    backend_kind: ?[]u8,
    backend_key: ?[]u8,
    created_at: []u8,
    updated_at: []u8,
    parent: ?ItemSummary,
    children: []ItemSummary,
    blocked_by: []ItemSummary,
    blocking: []ItemSummary,
    external_blockers: []ExternalBlockerSummary,

    /// Free all allocator-owned fields.
    pub fn deinit(self: ItemDetail, gpa: std.mem.Allocator) void {
        gpa.free(self.id);
        gpa.free(self.display_id);
        gpa.free(self.title);
        gpa.free(self.body);
        gpa.free(self.created_at);
        gpa.free(self.updated_at);
        if (self.backend_kind) |bk| gpa.free(bk);
        if (self.backend_key) |bk| gpa.free(bk);
        if (self.parent) |p| p.deinit(gpa);
        for (self.children) |c| c.deinit(gpa);
        gpa.free(self.children);
        for (self.blocked_by) |b| b.deinit(gpa);
        gpa.free(self.blocked_by);
        for (self.blocking) |b| b.deinit(gpa);
        gpa.free(self.blocking);
        for (self.external_blockers) |eb| eb.deinit(gpa);
        gpa.free(self.external_blockers);
    }
};

pub const ShowError = ResolveError;

/// Read one item's full current state from the Repository Store.
///
/// Resolves `display_arg` via the `item_ids` table (Display ID or Alias).
/// Returns `null` when the Display ID or Alias does not resolve. The caller
/// is responsible for freeing the returned `ItemDetail` via `deinit`.
pub fn showItem(store: Store, gpa: std.mem.Allocator, display_arg: []const u8) ShowError!?ItemDetail {
    const ref = (try resolveItemRef(store, gpa, display_arg)) orelse return null;
    defer ref.deinit(gpa);

    // Read the main item row.
    const item_row = (try store.conn.row(
        \\select id, display_value, item_class, ticket_kind, priority, title, body,
        \\       status, origin, backend_kind, backend_key, created_at, updated_at,
        \\       container_id
        \\  from items
        \\ where id = ?1
    , .{ref.id})) orelse return null;
    defer item_row.deinit();

    const id = try gpa.dupe(u8, item_row.text(0));
    errdefer gpa.free(id);
    const display_id = try gpa.dupe(u8, item_row.text(1));
    errdefer gpa.free(display_id);
    const item_class = enumFromText(ItemClass, item_row.text(2));
    const ticket_kind: ?TicketKind = if (item_row.nullableText(3)) |t| enumFromText(TicketKind, t) else null;
    const priority: ?Priority = if (item_row.nullableText(4)) |t| enumFromText(Priority, t) else null;
    const title = try gpa.dupe(u8, item_row.text(5));
    errdefer gpa.free(title);
    const body = try gpa.dupe(u8, item_row.text(6));
    errdefer gpa.free(body);
    const status = enumFromText(ItemStatus, item_row.text(7));
    const origin = enumFromText(Origin, item_row.text(8));
    const backend_kind: ?[]u8 = if (item_row.nullableText(9)) |t| try gpa.dupe(u8, t) else null;
    errdefer if (backend_kind) |bk| gpa.free(bk);
    const backend_key: ?[]u8 = if (item_row.nullableText(10)) |t| try gpa.dupe(u8, t) else null;
    errdefer if (backend_key) |bk| gpa.free(bk);
    const created_at = try gpa.dupe(u8, item_row.text(11));
    errdefer gpa.free(created_at);
    const updated_at = try gpa.dupe(u8, item_row.text(12));
    errdefer gpa.free(updated_at);
    const container_id: ?[]const u8 = item_row.nullableText(13);

    // Resolve parent if this is a Ticket with a container.
    var parent: ?ItemSummary = null;
    errdefer if (parent) |p| p.deinit(gpa);
    if (container_id) |cid| {
        if (try store.conn.row(
            \\select display_value, title, item_class, status, priority
            \\  from items
            \\ where id = ?1
        , .{cid})) |r| {
            defer r.deinit();
            const p_display_id = try gpa.dupe(u8, r.text(0));
            errdefer gpa.free(p_display_id);
            const p_title = try gpa.dupe(u8, r.text(1));
            parent = .{
                .display_id = p_display_id,
                .title = p_title,
                .item_class = enumFromText(ItemClass, r.text(2)),
                .status = enumFromText(ItemStatus, r.text(3)),
                .priority = if (r.nullableText(4)) |t| enumFromText(Priority, t) else null,
            };
        }
    }

    // Collect children if this is an Epic.
    var children: std.ArrayList(ItemSummary) = .empty;
    errdefer {
        for (children.items) |c| c.deinit(gpa);
        children.deinit(gpa);
    }
    if (item_class == .epic) {
        var rows = try store.conn.rows(
            \\select display_value, title, item_class, status, priority
            \\  from items
            \\ where container_id = ?1
            \\ order by created_seq asc
        , .{id});
        defer rows.deinit();
        while (rows.next()) |r| {
            const c_display_id = try gpa.dupe(u8, r.text(0));
            errdefer gpa.free(c_display_id);
            const c_title = try gpa.dupe(u8, r.text(1));
            try children.append(gpa, .{
                .display_id = c_display_id,
                .title = c_title,
                .item_class = enumFromText(ItemClass, r.text(2)),
                .status = enumFromText(ItemStatus, r.text(3)),
                .priority = if (r.nullableText(4)) |t| enumFromText(Priority, t) else null,
            });
        }
        if (rows.err) |err| return err;
    }

    // BLOCKED BY: items blocking this item that are not yet done.
    var blocked_by: std.ArrayList(ItemSummary) = .empty;
    errdefer {
        for (blocked_by.items) |b| b.deinit(gpa);
        blocked_by.deinit(gpa);
    }
    {
        var rows = try store.conn.rows(
            \\select i.display_value, i.title, i.item_class, i.status, i.priority
            \\  from dependencies d
            \\  join items i on i.id = d.blocking_id
            \\ where d.blocked_id = ?1
            \\   and i.status <> 'done'
            \\ order by i.created_seq asc
        , .{id});
        defer rows.deinit();
        while (rows.next()) |r| {
            const b_display_id = try gpa.dupe(u8, r.text(0));
            errdefer gpa.free(b_display_id);
            const b_title = try gpa.dupe(u8, r.text(1));
            try blocked_by.append(gpa, .{
                .display_id = b_display_id,
                .title = b_title,
                .item_class = enumFromText(ItemClass, r.text(2)),
                .status = enumFromText(ItemStatus, r.text(3)),
                .priority = if (r.nullableText(4)) |t| enumFromText(Priority, t) else null,
            });
        }
        if (rows.err) |err| return err;
    }

    // BLOCKING: items blocked by this item that are not yet done.
    var blocking: std.ArrayList(ItemSummary) = .empty;
    errdefer {
        for (blocking.items) |b| b.deinit(gpa);
        blocking.deinit(gpa);
    }
    {
        var rows = try store.conn.rows(
            \\select i.display_value, i.title, i.item_class, i.status, i.priority
            \\  from dependencies d
            \\  join items i on i.id = d.blocked_id
            \\ where d.blocking_id = ?1
            \\   and i.status <> 'done'
            \\ order by i.created_seq asc
        , .{id});
        defer rows.deinit();
        while (rows.next()) |r| {
            const bl_display_id = try gpa.dupe(u8, r.text(0));
            errdefer gpa.free(bl_display_id);
            const bl_title = try gpa.dupe(u8, r.text(1));
            try blocking.append(gpa, .{
                .display_id = bl_display_id,
                .title = bl_title,
                .item_class = enumFromText(ItemClass, r.text(2)),
                .status = enumFromText(ItemStatus, r.text(3)),
                .priority = if (r.nullableText(4)) |t| enumFromText(Priority, t) else null,
            });
        }
        if (rows.err) |err| return err;
    }

    // EXTERNAL BLOCKERS: unresolved only.
    var external_blockers: std.ArrayList(ExternalBlockerSummary) = .empty;
    errdefer {
        for (external_blockers.items) |eb| eb.deinit(gpa);
        external_blockers.deinit(gpa);
    }
    {
        var rows = try store.conn.rows(
            \\select reason
            \\  from external_blockers
            \\ where item_id = ?1
            \\   and resolved_at is null
            \\ order by created_at asc
        , .{id});
        defer rows.deinit();
        while (rows.next()) |r| {
            const reason = try gpa.dupe(u8, r.text(0));
            try external_blockers.append(gpa, .{ .reason = reason });
        }
        if (rows.err) |err| return err;
    }

    return .{
        .id = id,
        .display_id = display_id,
        .item_class = item_class,
        .ticket_kind = ticket_kind,
        .priority = priority,
        .title = title,
        .body = body,
        .status = status,
        .origin = origin,
        .backend_kind = backend_kind,
        .backend_key = backend_key,
        .created_at = created_at,
        .updated_at = updated_at,
        .parent = parent,
        .children = try children.toOwnedSlice(gpa),
        .blocked_by = try blocked_by.toOwnedSlice(gpa),
        .blocking = try blocking.toOwnedSlice(gpa),
        .external_blockers = try external_blockers.toOwnedSlice(gpa),
    };
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
    \\       status, origin, container_id, created_seq,
    \\       (has_unresolved_dependency or has_unresolved_external_blocker) as has_unresolved_blocker
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
    \\select display_value
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

    switch (options.scope) {
        .none => {},
        .display_arg => |display_arg| {
            const resolved = (try resolveItemRef(store, gpa, display_arg)) orelse return .scope_not_found;
            resolved_scope = resolved;
            scope_id = resolved.id;
            scope_mode = resolved.item_class.text();
        },
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

    const display_seq = try sequences.next(store.conn, "display_seq");
    const created_seq = try sequences.next(store.conn, "item_created_seq");
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

fn queryTextAlloc(conn: zqlite.Conn, gpa: std.mem.Allocator, sql: []const u8) (migrations.QueryError || error{OutOfMemory})![]u8 {
    if (try conn.row(sql, .{})) |r| {
        defer r.deinit();
        return gpa.dupe(u8, r.text(0));
    }
    return error.Notfound;
}

/// How the `--parent` field should be handled in an update request.
///
/// `unchanged` — container_id is left as-is; no parent Mutation is emitted.
/// `clear` — container_id is set to NULL (remove from current Epic).
/// `set` — container_id is set to the given internal stable `items.id`.
pub const ParentOp = union(enum) {
    unchanged,
    clear,
    set: []const u8,
};

/// Input for `updateItem`.
pub const UpdateRequest = struct {
    /// Internal stable ID of the item to update (from `resolveItemRef`).
    id: []const u8,
    /// Item class of the resolved item (drives Mutation type selection).
    item_class: ItemClass,
    /// New title, or `null` to leave unchanged.
    title: ?[]const u8 = null,
    /// New body, or `null` to leave unchanged.
    body: ?[]const u8 = null,
    /// New Priority for Tickets, or `null` to leave unchanged. Must be
    /// `null` for Epics — the caller enforces this precondition.
    priority: ?Priority = null,
    /// Parent operation. Must be `.unchanged` for Epics.
    parent: ParentOp = .unchanged,
};

/// Fields from the current item row that `updateItem` reads inside the tx.
const CurrentItem = struct {
    origin: Origin,
    title: []u8,
    body: []u8,
    priority: ?[]u8,
    container_id: ?[]u8,

    fn deinit(self: CurrentItem, gpa: std.mem.Allocator) void {
        gpa.free(self.title);
        gpa.free(self.body);
        if (self.priority) |p| gpa.free(p);
        if (self.container_id) |c| gpa.free(c);
    }
};

/// The minimal updated item snapshot returned on success.
pub const UpdatedItem = struct {
    display_id: []u8,
    title: []u8,
    item_class: ItemClass,

    /// Free allocator-owned fields.
    pub fn deinit(self: UpdatedItem, gpa: std.mem.Allocator) void {
        gpa.free(self.display_id);
        gpa.free(self.title);
    }
};

/// Result of `updateItem`.
///
/// `.ok` owns an `UpdatedItem` that the caller must free. `.not_found` means
/// the internal id did not resolve to a live row.
pub const UpdateOutcome = union(enum) {
    ok: UpdatedItem,
    not_found,
};

/// Error set for `updateItem`.
pub const UpdateError = migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Input for a reusable Item Status lifecycle write.
pub const SetStatusRequest = struct {
    /// Internal stable ID of the Ticket or Epic to update.
    id: []const u8,
    /// Target Item Status to persist.
    status: ItemStatus,
};

/// Minimal item snapshot returned after a lifecycle status write.
pub const StatusChangedItem = struct {
    display_id: []u8,
    title: []u8,
    item_class: ItemClass,
    status: ItemStatus,

    /// Free allocator-owned fields copied from the Repository Store.
    pub fn deinit(self: StatusChangedItem, gpa: std.mem.Allocator) void {
        gpa.free(self.display_id);
        gpa.free(self.title);
    }
};

/// Result of `setItemStatus`.
pub const SetStatusOutcome = union(enum) {
    ok: StatusChangedItem,
    not_found,
    /// Refused: the item is `done` and the request asked for any other
    /// Item Status. ADR 0006 makes `done` terminal in v1. Carries the
    /// persisted `ItemClass` so callers can render `Ticket` vs `Epic`
    /// in the diagnostic without an extra round-trip.
    locked_done: ItemClass,
};

/// Error set for `setItemStatus`.
pub const SetStatusError = migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Input for creating an item-backed Dependency edge.
pub const AddDependencyRequest = struct {
    /// Internal stable ID of the Blocked Item whose readiness changes.
    blocked_id: []const u8,
    /// Internal stable ID of the Blocking Item that must be done first.
    blocking_id: []const u8,
};

/// Error set for `addDependency`.
pub const AddDependencyError = migrations.QueryError || zqlite.Error || mutations_mod.AppendError || error{OutOfMemory};

/// Result of `addDependency`.
pub const AddDependencyOutcome = union(enum) {
    ok,
    /// The Blocked Item is already `done`; v1 only creates live blocking
    /// relationships.
    blocked_done,
    /// The Blocking Item is already `done`; v1 only creates live blocking
    /// relationships.
    blocking_done,
    /// The requested Dependency would make the graph cyclic.
    cycle,
    /// A Backend Blocked Item cannot be applied to its Backend Adapter while
    /// the Blocking Item is still local-only.
    backend_blocked_local_blocking,
    /// Backend Dependency Mutations can only target references addressable by
    /// the same Backend Adapter.
    backend_kind_mismatch,
};

/// Input for removing an item-backed Dependency edge.
pub const RemoveDependencyRequest = AddDependencyRequest;

/// Error set for `removeDependency`.
pub const RemoveDependencyError = migrations.QueryError || zqlite.Error || mutations_mod.AppendError || error{OutOfMemory};

/// Apply a field-level update to a Ticket or Epic.
///
/// Opens a `begin immediate` transaction, reads the current row, diffs each
/// requested field against the stored value, writes only the changed columns,
/// and — for Backend-origin items — appends Mutations to the outbox in the
/// same transaction. Priority changes are never emitted as Mutations.
///
/// Field idempotence: if a requested value equals the current stored value it
/// is excluded from the UPDATE and no Mutation is appended for that field.
/// `updated_at` is bumped only when at least one field actually changes.
pub fn updateItem(
    store: Store,
    gpa: std.mem.Allocator,
    clock: clock_mod.Clock,
    req: UpdateRequest,
) UpdateError!UpdateOutcome {
    var iso_buf: [24]u8 = undefined;
    const now = clock.nowIso(&iso_buf);

    try store.conn.execNoArgs("begin immediate");
    errdefer store.conn.rollback();

    // Read the current item row inside the transaction. The SQLite prepared-
    // statement cursor must outlive the `gpa.dupe` chain so OOM during the
    // copy-out does not leak the cursor before `errdefer rollback` fires.
    const current_row = (try store.conn.row(
        \\select origin, title, body, priority, container_id, display_value
        \\  from items
        \\ where id = ?1
    , .{req.id})) orelse {
        store.conn.rollback();
        return .not_found;
    };
    defer current_row.deinit();
    const current: CurrentItem = blk: {
        const origin = enumFromText(Origin, current_row.text(0));
        const title = try gpa.dupe(u8, current_row.text(1));
        errdefer gpa.free(title);
        const body = try gpa.dupe(u8, current_row.text(2));
        errdefer gpa.free(body);
        const priority: ?[]u8 = if (current_row.nullableText(3)) |p| try gpa.dupe(u8, p) else null;
        errdefer if (priority) |p| gpa.free(p);
        const container_id: ?[]u8 = if (current_row.nullableText(4)) |c| try gpa.dupe(u8, c) else null;
        break :blk .{
            .origin = origin,
            .title = title,
            .body = body,
            .priority = priority,
            .container_id = container_id,
        };
    };
    defer current.deinit(gpa);

    // Compute per-field deltas.
    const new_title: ?[]const u8 = if (req.title) |t|
        if (!std.mem.eql(u8, t, current.title)) t else null
    else
        null;
    const new_body: ?[]const u8 = if (req.body) |b|
        if (!std.mem.eql(u8, b, current.body)) b else null
    else
        null;
    const new_priority: ?Priority = if (req.priority) |p|
        if (current.priority == null or !std.mem.eql(u8, p.text(), current.priority.?)) p else null
    else
        null;

    // Parent delta: old_epic_id set when removing; new_epic_id set when adding.
    var old_epic_id: ?[]const u8 = null;
    var new_epic_id: ?[]const u8 = null;
    var parent_changed = false;
    switch (req.parent) {
        .unchanged => {},
        .clear => {
            if (current.container_id != null) {
                old_epic_id = current.container_id;
                parent_changed = true;
            }
        },
        .set => |epic_id| {
            const already_set = if (current.container_id) |cid| std.mem.eql(u8, cid, epic_id) else false;
            if (!already_set) {
                old_epic_id = current.container_id;
                new_epic_id = epic_id;
                parent_changed = true;
            }
        },
    }

    const title_or_body_changed = (new_title != null or new_body != null);
    const any_change = title_or_body_changed or new_priority != null or parent_changed;

    if (!any_change) {
        // Nothing changed — read back the display_id and title for the caller.
        const snapshot_row = (try store.conn.row(
            "select display_value, title from items where id = ?1",
            .{req.id},
        )) orelse {
            store.conn.rollback();
            return .not_found;
        };
        defer snapshot_row.deinit();
        const display_id = try gpa.dupe(u8, snapshot_row.text(0));
        errdefer gpa.free(display_id);
        const snap_title = try gpa.dupe(u8, snapshot_row.text(1));
        errdefer gpa.free(snap_title);
        try store.conn.commit();
        return .{ .ok = .{ .display_id = display_id, .title = snap_title, .item_class = req.item_class } };
    }

    // Build the UPDATE statement dynamically for only the changed columns.
    // Always include updated_at when something changed.
    const eff_title = new_title orelse current.title;
    const eff_body = new_body orelse current.body;

    if (parent_changed) {
        const new_container: ?[]const u8 = new_epic_id;
        const new_container_class: ?[]const u8 = if (new_container != null) "epic" else null;
        if (new_priority) |p| {
            try store.conn.exec(
                \\update items
                \\   set title = ?2, body = ?3, priority = ?4,
                \\       container_id = ?5, container_class = ?6, updated_at = ?7
                \\ where id = ?1
            , .{ req.id, eff_title, eff_body, p.text(), new_container, new_container_class, now });
        } else {
            try store.conn.exec(
                \\update items
                \\   set title = ?2, body = ?3,
                \\       container_id = ?4, container_class = ?5, updated_at = ?6
                \\ where id = ?1
            , .{ req.id, eff_title, eff_body, new_container, new_container_class, now });
        }
    } else if (new_priority) |p| {
        try store.conn.exec(
            \\update items
            \\   set title = ?2, body = ?3, priority = ?4, updated_at = ?5
            \\ where id = ?1
        , .{ req.id, eff_title, eff_body, p.text(), now });
    } else {
        try store.conn.exec(
            \\update items
            \\   set title = ?2, body = ?3, updated_at = ?4
            \\ where id = ?1
        , .{ req.id, eff_title, eff_body, now });
    }

    // Append Mutations for Backend-origin items only.
    if (current.origin == .backend) {
        // Parent change: remove then add (mutation order per spec).
        if (old_epic_id) |eid| {
            try mutations_mod.appendMutation(
                store.conn,
                gpa,
                .remove_ticket_from_epic,
                req.id,
                req.item_class,
                .{ .epic_ref = .{ .epic_id = eid } },
                now,
            );
        }
        if (new_epic_id) |eid| {
            try mutations_mod.appendMutation(
                store.conn,
                gpa,
                .add_ticket_to_epic,
                req.id,
                req.item_class,
                .{ .epic_ref = .{ .epic_id = eid } },
                now,
            );
        }
        // Title/body change: full snapshot mutation.
        if (title_or_body_changed) {
            const mut_type: MutationType = if (req.item_class == .ticket) .update_ticket else .update_epic;
            try mutations_mod.appendMutation(
                store.conn,
                gpa,
                mut_type,
                req.id,
                req.item_class,
                .{ .update_title_body = .{ .title = eff_title, .body = eff_body } },
                now,
            );
        }
    }

    // Read back the final display_id and title for the caller.
    const snap = (try store.conn.row(
        "select display_value, title from items where id = ?1",
        .{req.id},
    )) orelse {
        store.conn.rollback();
        return .not_found;
    };
    defer snap.deinit();
    const display_id = try gpa.dupe(u8, snap.text(0));
    errdefer gpa.free(display_id);
    const snap_title = try gpa.dupe(u8, snap.text(1));
    errdefer gpa.free(snap_title);

    try store.conn.commit();

    return .{ .ok = .{ .display_id = display_id, .title = snap_title, .item_class = req.item_class } };
}

/// Set a Ticket or Epic Item Status in current state.
///
/// Backend-origin changes append a pending `set_item_status` Mutation in the
/// same Repository Store transaction. Local-origin changes only update current
/// state. Field-idempotent calls succeed without changing `updated_at` or
/// emitting a Mutation.
///
/// ADR 0006 makes `done` terminal in v1. A pre-read short-circuit refuses
/// any request that would move a `done` row to a non-`done` Item Status
/// and returns `.locked_done` so callers can render a typed diagnostic.
/// The `items_no_escape_from_done` trigger from migration 002 is the only
/// trigger that can fire on the UPDATE inside this function, and the
/// short-circuit prevents even that one from firing; the trigger remains
/// as defense-in-depth for write paths that do not pre-read.
pub fn setItemStatus(
    store: Store,
    gpa: std.mem.Allocator,
    clock: clock_mod.Clock,
    req: SetStatusRequest,
) SetStatusError!SetStatusOutcome {
    var iso_buf: [24]u8 = undefined;
    const now = clock.nowIso(&iso_buf);

    try store.conn.execNoArgs("begin immediate");
    errdefer store.conn.rollback();

    const current_row = (try store.conn.row(
        \\select origin, status, item_class, display_value, title
        \\  from items
        \\ where id = ?1
    , .{req.id})) orelse {
        store.conn.rollback();
        return .not_found;
    };
    defer current_row.deinit();

    const origin = enumFromText(Origin, current_row.text(0));
    const current_status = enumFromText(ItemStatus, current_row.text(1));
    const item_class = enumFromText(ItemClass, current_row.text(2));

    // ADR 0006: `done` is terminal in v1. Refuse non-`done` targets before
    // any allocator dupes so the no-op write transaction commits cleanly
    // and the caller receives a typed `.locked_done` outcome rather than
    // the schema trigger's `error.ConstraintTrigger`.
    if (current_status == .done and req.status != .done) {
        try store.conn.commit();
        return .{ .locked_done = item_class };
    }

    const display_id = try gpa.dupe(u8, current_row.text(3));
    errdefer gpa.free(display_id);
    const title = try gpa.dupe(u8, current_row.text(4));
    errdefer gpa.free(title);

    if (current_status == req.status) {
        try store.conn.commit();
        return .{ .ok = .{
            .display_id = display_id,
            .title = title,
            .item_class = item_class,
            .status = req.status,
        } };
    }

    try store.conn.exec(
        \\update items
        \\   set status = ?2, updated_at = ?3
        \\ where id = ?1
    , .{ req.id, req.status.text(), now });

    if (origin == .backend) {
        try mutations_mod.appendMutation(
            store.conn,
            gpa,
            .set_item_status,
            req.id,
            item_class,
            .{ .item_status = .{ .status = req.status.text() } },
            now,
        );
    }

    try store.conn.commit();

    return .{ .ok = .{
        .display_id = display_id,
        .title = title,
        .item_class = item_class,
        .status = req.status,
    } };
}

/// Create a Dependency edge from a Blocking Item to a Blocked Item.
///
/// The command layer resolves Display IDs and Aliases into internal stable IDs
/// before calling this helper. Dependency edges are current-state relationship
/// data; same-backend Dependency changes also append backend intent through
/// the Mutation Log.
pub fn addDependency(
    store: Store,
    gpa: std.mem.Allocator,
    clock: clock_mod.Clock,
    req: AddDependencyRequest,
) AddDependencyError!AddDependencyOutcome {
    var iso_buf: [24]u8 = undefined;
    const now = clock.nowIso(&iso_buf);

    try store.conn.execNoArgs("begin immediate");
    errdefer store.conn.rollback();

    const endpoint_row = (try store.conn.row(
        \\select blocked.status, blocked.origin, blocked.item_class, blocked.backend_kind,
        \\       blocking.status, blocking.origin, blocking.backend_kind
        \\  from items blocked
        \\  join items blocking on blocking.id = ?2
        \\ where blocked.id = ?1
    , .{ req.blocked_id, req.blocking_id })) orelse return error.ConstraintForeignKey;
    defer endpoint_row.deinit();

    const blocked_status = enumFromText(ItemStatus, endpoint_row.text(0));
    const blocked_origin = enumFromText(Origin, endpoint_row.text(1));
    const blocked_class = enumFromText(ItemClass, endpoint_row.text(2));
    const blocked_backend_kind = endpoint_row.nullableText(3);
    const blocking_status = enumFromText(ItemStatus, endpoint_row.text(4));
    const blocking_origin = enumFromText(Origin, endpoint_row.text(5));
    const blocking_backend_kind = endpoint_row.nullableText(6);

    if (blocked_status == .done) {
        try store.conn.commit();
        return .blocked_done;
    }
    if (blocking_status == .done) {
        try store.conn.commit();
        return .blocking_done;
    }
    if (blocked_origin == .backend and blocking_origin == .local) {
        try store.conn.commit();
        return .backend_blocked_local_blocking;
    }
    if (blocked_origin == .backend and blocking_origin == .backend and !std.mem.eql(u8, blocked_backend_kind.?, blocking_backend_kind.?)) {
        try store.conn.commit();
        return .backend_kind_mismatch;
    }

    if (try store.conn.row(
        \\with recursive reachable(id) as (
        \\    select ?2
        \\    union
        \\    select d.blocking_id
        \\      from dependencies d, reachable
        \\     where d.blocked_id = reachable.id
        \\)
        \\select 1 from reachable where id = ?1
    , .{ req.blocked_id, req.blocking_id })) |cycle_row| {
        defer cycle_row.deinit();
        try store.conn.commit();
        return .cycle;
    }

    const had_edge = if (try store.conn.row(
        \\select 1 from dependencies
        \\ where blocking_id = ?1
        \\   and blocked_id = ?2
    , .{ req.blocking_id, req.blocked_id })) |edge_row| blk: {
        edge_row.deinit();
        break :blk true;
    } else false;

    try store.conn.exec(
        \\insert or ignore into dependencies(blocking_id, blocked_id, created_at)
        \\values (?1, ?2, ?3)
    , .{ req.blocking_id, req.blocked_id, now });

    if (!had_edge and blocked_origin == .backend and blocking_origin == .backend and std.mem.eql(u8, blocked_backend_kind.?, blocking_backend_kind.?)) {
        try mutations_mod.appendMutation(
            store.conn,
            gpa,
            .add_dependency,
            req.blocked_id,
            blocked_class,
            .{ .dependency_ref = .{ .blocking_id = req.blocking_id } },
            now,
        );
    }

    try store.conn.commit();
    return .ok;
}

/// Remove a Dependency edge from a Blocking Item to a Blocked Item.
///
/// Missing edges are successful no-ops so `tk unblock` behaves as a
/// desired-state cleanup command.
pub fn removeDependency(
    store: Store,
    gpa: std.mem.Allocator,
    clock: clock_mod.Clock,
    req: RemoveDependencyRequest,
) RemoveDependencyError!void {
    var iso_buf: [24]u8 = undefined;
    const now = clock.nowIso(&iso_buf);

    try store.conn.execNoArgs("begin immediate");
    errdefer store.conn.rollback();

    const endpoint_row = (try store.conn.row(
        \\select blocked.origin, blocked.item_class, blocked.backend_kind,
        \\       blocking.origin, blocking.backend_kind
        \\  from items blocked
        \\  join items blocking on blocking.id = ?2
        \\ where blocked.id = ?1
    , .{ req.blocked_id, req.blocking_id })) orelse return error.ConstraintForeignKey;
    defer endpoint_row.deinit();

    const blocked_origin = enumFromText(Origin, endpoint_row.text(0));
    const blocked_class = enumFromText(ItemClass, endpoint_row.text(1));
    const blocked_backend_kind = endpoint_row.nullableText(2);
    const blocking_origin = enumFromText(Origin, endpoint_row.text(3));
    const blocking_backend_kind = endpoint_row.nullableText(4);

    const had_edge = if (try store.conn.row(
        \\select 1 from dependencies
        \\ where blocking_id = ?1
        \\   and blocked_id = ?2
    , .{ req.blocking_id, req.blocked_id })) |edge_row| blk: {
        edge_row.deinit();
        break :blk true;
    } else false;

    try store.conn.exec(
        \\delete from dependencies
        \\ where blocking_id = ?1
        \\   and blocked_id = ?2
    , .{ req.blocking_id, req.blocked_id });

    if (had_edge and blocked_origin == .backend and blocking_origin == .backend and std.mem.eql(u8, blocked_backend_kind.?, blocking_backend_kind.?)) {
        try mutations_mod.appendMutation(
            store.conn,
            gpa,
            .remove_dependency,
            req.blocked_id,
            blocked_class,
            .{ .dependency_ref = .{ .blocking_id = req.blocking_id } },
            now,
        );
    }

    try store.conn.commit();
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
        .has_unresolved_blocker = row.int(10) != 0,
    };
}

/// A Display ID or Alias resolved to a stable internal ID and Item class.
///
/// Returned by `resolveItemRef` and `resolveAsEpic`. The caller owns `id`
/// and must free it with `deinit` in the switch arm that receives the value.
pub const ResolvedItemRef = struct {
    id: []u8,
    item_class: ItemClass,

    /// Free the internal ID string allocated by the Repository Store resolver.
    pub fn deinit(self: ResolvedItemRef, gpa: std.mem.Allocator) void {
        gpa.free(self.id);
    }
};

/// Errors that `resolveItemRef` and `resolveAsEpic` can return.
///
/// `NextError` is a superset of `ResolveError`, so callers that propagate
/// into a `NextError` context need no conversion.
pub const ResolveError = migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Look up a Display ID or Alias in the Repository Store and return the
/// matching stable internal ID and Item class.
///
/// The `item_ids` table uses a case-insensitive collation, so `TK-1` and
/// `tk-1` resolve identically. Returns `null` when no matching row exists.
pub fn resolveItemRef(store: Store, gpa: std.mem.Allocator, display_arg: []const u8) ResolveError!?ResolvedItemRef {
    if (try store.conn.row(resolve_item_ref_sql, .{display_arg})) |r| {
        defer r.deinit();
        return .{
            .id = try gpa.dupe(u8, r.text(0)),
            .item_class = enumFromText(ItemClass, r.text(1)),
        };
    }
    return null;
}

/// Outcome of resolving a Display ID or Alias that must refer to an Epic.
///
/// `not_an_epic` carries the resolved reference for future diagnostics that
/// want to name the resolved item's actual `item_class`. With v1's two
/// Item Classes, callers may safely hardcode "Ticket" in the user-facing
/// message instead of reading `ref.item_class`.
/// Each arm owns its payload; callers free `epic.id` and `not_an_epic.id`
/// in the matching switch arm.
pub const ResolveEpicOutcome = union(enum) {
    epic: ResolvedItemRef,
    not_found,
    not_an_epic: ResolvedItemRef,
};

/// Resolve a Display ID or Alias to an Epic reference.
///
/// Use this for `--parent <epic-id>` validation so the deferred composite
/// foreign key on `items(container_id, container_class)` does not surface as
/// a raw FK error when the user supplies a Ticket's Display ID.
pub fn resolveAsEpic(store: Store, gpa: std.mem.Allocator, display_arg: []const u8) ResolveError!ResolveEpicOutcome {
    const resolved = (try resolveItemRef(store, gpa, display_arg)) orelse return .not_found;
    if (resolved.item_class == .epic) return .{ .epic = resolved };
    return .{ .not_an_epic = resolved };
}

fn nextTicketFromSql(gpa: std.mem.Allocator, row: zqlite.Row) NextError!NextTicket {
    const display_id = try gpa.dupe(u8, row.text(0));
    return .{
        .display_id = display_id,
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

test "nextReadyTicket: selects backend Tickets regardless of Mutation state" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "local",
        .display = "tk-1",
        .title = "Local ready",
        .priority = "P1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "backend",
        .display = "GH#9",
        .title = "Backend ready",
        .priority = "P0",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "9",
        .created_seq = 2,
    });
    try conn.exec(
        \\insert into mutations(
        \\  sequence, mutation_type, item_id, item_class, payload_json, state,
        \\  failure_json, created_at, state_changed_at
        \\)
        \\values (
        \\  1, 'update_ticket', 'backend', 'ticket', '{}', 'failed', '{}',
        \\  '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z'
        \\)
    , .{});

    const outcome = try nextReadyTicket(store, gpa, .{});
    switch (outcome) {
        .ticket => |ticket| {
            defer ticket.deinit(gpa);
            try std.testing.expectEqualStrings("GH#9", ticket.display_id);
        },
        else => return error.UnexpectedOutcome,
    }
}

test "nextReadyTicket: breaks Priority ties by created_seq across Origins" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "backend",
        .display = "GH#9",
        .title = "Imported first",
        .priority = "P1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "9",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "local",
        .display = "tk-1",
        .title = "Local second",
        .priority = "P1",
        .created_seq = 2,
    });

    const outcome = try nextReadyTicket(store, gpa, .{});
    switch (outcome) {
        .ticket => |ticket| {
            defer ticket.deinit(gpa);
            try std.testing.expectEqualStrings("GH#9", ticket.display_id);
        },
        else => return error.UnexpectedOutcome,
    }
}

test "nextReadyTicket: ignores Ticket Kind for ordering" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "task",
        .display = "tk-1",
        .title = "Earlier task",
        .ticket_kind = "task",
        .priority = "P1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "bug",
        .display = "tk-2",
        .title = "Later bug",
        .ticket_kind = "bug",
        .priority = "P1",
        .created_seq = 2,
    });

    const outcome = try nextReadyTicket(store, gpa, .{});
    switch (outcome) {
        .ticket => |ticket| {
            defer ticket.deinit(gpa);
            try std.testing.expectEqualStrings("tk-1", ticket.display_id);
        },
        else => return error.UnexpectedOutcome,
    }
}

test "nextReadyTicket: selects a ready child Ticket under a done Epic" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "done-epic",
        .display = "tk-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Closed container",
        .status = "done",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "ready-child",
        .display = "tk-2",
        .title = "Ready child",
        .priority = "P1",
        .container_id = "done-epic",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "later-ready",
        .display = "tk-3",
        .title = "Later ready",
        .priority = "P2",
        .created_seq = 3,
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

test "nextReadyTicket: scoped Epic blockers do not block ready children" {
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
        .title = "Blocked Epic",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "child",
        .display = "tk-2",
        .title = "Ready child",
        .priority = "P2",
        .container_id = "epic",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocker",
        .display = "tk-3",
        .title = "Epic blocker",
        .priority = "P4",
        .created_seq = 3,
    });
    try TmpStore.insertDependency(conn, "blocker", "epic");
    try TmpStore.insertExternalBlocker(conn, "external-blocker", "epic", null);

    const outcome = try nextReadyTicket(store, gpa, .{ .scope = .{ .display_arg = "tk-1" } });
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

    const missing_scope = try nextReadyTicket(store, gpa, .{ .scope = .{ .display_arg = "missing-9" } });
    switch (missing_scope) {
        .scope_not_found => {},
        else => return error.UnexpectedOutcome,
    }
}

test "resolveItemRef: returns null for an unknown Display ID" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    const result = try resolveItemRef(store, gpa, "missing");
    try std.testing.expectEqual(null, result);
}

test "resolveItemRef: resolves Display IDs and Aliases case-insensitively" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{ .id = "ticket-id", .display = "tk-1", .title = "A Ticket", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic-id",
        .display = "tk-2",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "An Epic",
        .created_seq = 2,
    });
    try TmpStore.insertAlias(conn, "legacy-9", "ticket-id");

    {
        const resolved = (try resolveItemRef(store, gpa, "tk-1")) orelse return error.ExpectedResolved;
        defer resolved.deinit(gpa);
        try std.testing.expectEqualStrings("ticket-id", resolved.id);
        try std.testing.expectEqual(ItemClass.ticket, resolved.item_class);
    }
    {
        const resolved = (try resolveItemRef(store, gpa, "TK-1")) orelse return error.ExpectedResolved;
        defer resolved.deinit(gpa);
        try std.testing.expectEqualStrings("ticket-id", resolved.id);
        try std.testing.expectEqual(ItemClass.ticket, resolved.item_class);
    }
    {
        const resolved = (try resolveItemRef(store, gpa, "tk-2")) orelse return error.ExpectedResolved;
        defer resolved.deinit(gpa);
        try std.testing.expectEqualStrings("epic-id", resolved.id);
        try std.testing.expectEqual(ItemClass.epic, resolved.item_class);
    }
    {
        const resolved = (try resolveItemRef(store, gpa, "legacy-9")) orelse return error.ExpectedResolved;
        defer resolved.deinit(gpa);
        try std.testing.expectEqualStrings("ticket-id", resolved.id);
        try std.testing.expectEqual(ItemClass.ticket, resolved.item_class);
    }
}

test "resolveAsEpic: routes Tickets, Epics, and unknown ids" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{ .id = "ticket-id", .display = "tk-1", .title = "A Ticket", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic-id",
        .display = "tk-2",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "An Epic",
        .created_seq = 2,
    });

    {
        const outcome = try resolveAsEpic(store, gpa, "tk-2");
        switch (outcome) {
            .epic => |ref| {
                defer ref.deinit(gpa);
                try std.testing.expectEqualStrings("epic-id", ref.id);
                try std.testing.expectEqual(ItemClass.epic, ref.item_class);
            },
            else => return error.ExpectedEpic,
        }
    }
    {
        const outcome = try resolveAsEpic(store, gpa, "tk-1");
        switch (outcome) {
            .not_an_epic => |ref| {
                defer ref.deinit(gpa);
                try std.testing.expectEqualStrings("ticket-id", ref.id);
                try std.testing.expectEqual(ItemClass.ticket, ref.item_class);
            },
            else => return error.ExpectedNotAnEpic,
        }
    }
    {
        const outcome = try resolveAsEpic(store, gpa, "missing");
        switch (outcome) {
            .not_found => {},
            else => return error.ExpectedNotFound,
        }
    }
}

fn expectDisplayIds(rows: []const ListRow, expected: []const []const u8) !void {
    try std.testing.expectEqual(expected.len, rows.len);
    for (expected, 0..) |display_id, i| {
        try std.testing.expectEqualStrings(display_id, rows[i].display_id);
    }
}

test "showItem: returns null for an unknown Display ID" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    const result = try showItem(store, gpa, "missing");
    try std.testing.expectEqual(null, result);
}

test "showItem: returns full Ticket detail with parent, dependencies, and external blockers" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    // Epic parent.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "epic",
        .display = "tk-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Ship list command",
        .created_seq = 1,
    });
    // The Ticket under test.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket",
        .display = "tk-2",
        .title = "Render ready list",
        .priority = "P1",
        .container_id = "epic",
        .created_seq = 2,
    });
    // A Blocking Item (unresolved).
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocker",
        .display = "tk-3",
        .title = "Wait for backend",
        .priority = "P2",
        .created_seq = 3,
    });
    try TmpStore.insertDependency(conn, "blocker", "ticket");
    // A Blocked Item (downstream, not done).
    try TmpStore.insertFixtureItem(conn, .{
        .id = "downstream",
        .display = "tk-9",
        .title = "Downstream cleanup",
        .priority = "P2",
        .created_seq = 9,
    });
    try TmpStore.insertDependency(conn, "ticket", "downstream");
    // An unresolved External Blocker with an explicit reason so the test
    // assertion does not couple to the TmpStore fixture default.
    try conn.exec(
        \\insert into external_blockers(id, item_id, reason, created_at, resolved_at)
        \\values (?1, ?2, ?3, '2026-05-09T00:00:00.000Z', null)
    , .{ "eb-open", "ticket", "needs legal review" });
    // A resolved External Blocker (must be excluded).
    try TmpStore.insertExternalBlocker(conn, "eb-done", "ticket", "2026-05-10T00:00:00.000Z");

    const detail = (try showItem(store, gpa, "tk-2")) orelse return error.ExpectedDetail;
    defer detail.deinit(gpa);

    try std.testing.expectEqualStrings("ticket", detail.id);
    try std.testing.expectEqualStrings("tk-2", detail.display_id);
    try std.testing.expectEqual(ItemClass.ticket, detail.item_class);
    try std.testing.expectEqual(ItemStatus.open, detail.status);

    // Parent.
    const p = detail.parent orelse return error.ExpectedParent;
    try std.testing.expectEqualStrings("tk-1", p.display_id);
    try std.testing.expectEqualStrings("Ship list command", p.title);

    // BLOCKED BY: one unresolved blocker.
    try std.testing.expectEqual(@as(usize, 1), detail.blocked_by.len);
    try std.testing.expectEqualStrings("tk-3", detail.blocked_by[0].display_id);

    // BLOCKING: one downstream.
    try std.testing.expectEqual(@as(usize, 1), detail.blocking.len);
    try std.testing.expectEqualStrings("tk-9", detail.blocking[0].display_id);

    // EXTERNAL BLOCKERS: only the unresolved one.
    try std.testing.expectEqual(@as(usize, 1), detail.external_blockers.len);
    try std.testing.expectEqualStrings("needs legal review", detail.external_blockers[0].reason);
}

test "showItem: includes children for Epics ordered by created_seq" {
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
        .title = "Ship list command",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "child-c",
        .display = "tk-4",
        .title = "Third child",
        .container_id = "epic",
        .created_seq = 4,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "child-a",
        .display = "tk-2",
        .title = "First child",
        .container_id = "epic",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "child-b",
        .display = "tk-3",
        .title = "Second child",
        .container_id = "epic",
        .created_seq = 3,
    });

    const detail = (try showItem(store, gpa, "tk-1")) orelse return error.ExpectedDetail;
    defer detail.deinit(gpa);

    try std.testing.expectEqual(ItemClass.epic, detail.item_class);
    try std.testing.expectEqual(@as(usize, 3), detail.children.len);
    try std.testing.expectEqualStrings("tk-2", detail.children[0].display_id);
    try std.testing.expectEqualStrings("tk-3", detail.children[1].display_id);
    try std.testing.expectEqualStrings("tk-4", detail.children[2].display_id);
}

test "showItem: filters out resolved (done) dependencies" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket",
        .display = "tk-2",
        .title = "The Ticket",
        .created_seq = 2,
    });
    // Done blocker — should be excluded from blocked_by.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "done-blocker",
        .display = "tk-1",
        .title = "Done blocker",
        .status = "done",
        .created_seq = 1,
    });
    try TmpStore.insertDependency(conn, "done-blocker", "ticket");

    const detail = (try showItem(store, gpa, "tk-2")) orelse return error.ExpectedDetail;
    defer detail.deinit(gpa);

    // Done blocker must not appear.
    try std.testing.expectEqual(@as(usize, 0), detail.blocked_by.len);
}

test "showItem: resolves via Alias" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "ticket-id",
        .display = "GH#42",
        .title = "A Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "42",
        .created_seq = 1,
    });
    try TmpStore.insertAlias(conn, "tk-1", "ticket-id");

    const detail = (try showItem(store, gpa, "tk-1")) orelse return error.ExpectedDetail;
    defer detail.deinit(gpa);

    try std.testing.expectEqualStrings("ticket-id", detail.id);
    try std.testing.expectEqualStrings("GH#42", detail.display_id);
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

// ──────────────────────────────────────────────────────────────────────────────
// setItemStatus tests
// ──────────────────────────────────────────────────────────────────────────────

const FakeClock = @import("../clock.zig").FakeClock;
const default_fake_now_ms = @import("../testing/test_cli.zig").default_fake_now_ms;

test "setItemStatus: returns not_found for unknown id" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    const outcome = try setItemStatus(store, gpa, fc.clock(), .{ .id = "missing", .status = .done });
    try std.testing.expectEqual(SetStatusOutcome.not_found, outcome);
}

test "setItemStatus: local-origin done updates status and emits no Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Local Ticket",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    const outcome = try setItemStatus(store, gpa, fc.clock(), .{ .id = "t1", .status = .done });
    switch (outcome) {
        .ok => |item| {
            defer item.deinit(gpa);
            try std.testing.expectEqualStrings("tk-1", item.display_id);
            try std.testing.expectEqualStrings("Local Ticket", item.title);
            try std.testing.expectEqual(ItemClass.ticket, item.item_class);
            try std.testing.expectEqual(ItemStatus.done, item.status);
        },
        .not_found => return error.UnexpectedNotFound,
        .locked_done => return error.UnexpectedLockedDone,
    }

    const row = (try conn.row("select status, updated_at from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("done", row.text(0));
    try std.testing.expect(!std.mem.eql(u8, "2026-01-01T00:00:00.000Z", row.text(1)));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "setItemStatus: Backend-origin change emits set_item_status Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "GH#10",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Backend Epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "10",
        .created_seq = 1,
    });

    const outcome = try setItemStatus(store, gpa, fc.clock(), .{ .id = "e1", .status = .done });
    switch (outcome) {
        .ok => |item| {
            defer item.deinit(gpa);
            try std.testing.expectEqual(ItemClass.epic, item.item_class);
        },
        .not_found => return error.UnexpectedNotFound,
        .locked_done => return error.UnexpectedLockedDone,
    }

    const row = (try conn.row(
        "select mutation_type, item_id, item_class, payload_json from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("set_item_status", row.text(0));
    try std.testing.expectEqualStrings("e1", row.text(1));
    try std.testing.expectEqualStrings("epic", row.text(2));
    try std.testing.expectEqualStrings("{\"status\":\"done\"}", row.text(3));
}

test "setItemStatus: already-done Backend row is idempotent" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Backend Ticket",
        .status = "done",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    const outcome = try setItemStatus(store, gpa, fc.clock(), .{ .id = "t1", .status = .done });
    switch (outcome) {
        .ok => |item| {
            defer item.deinit(gpa);
            try std.testing.expectEqualStrings("GH#1", item.display_id);
            try std.testing.expectEqualStrings("Backend Ticket", item.title);
            try std.testing.expectEqual(ItemClass.ticket, item.item_class);
            try std.testing.expectEqual(ItemStatus.done, item.status);
        },
        .not_found => return error.UnexpectedNotFound,
        .locked_done => return error.UnexpectedLockedDone,
    }

    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
    const row = (try conn.row("select updated_at from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(0));
}

test "setItemStatus: already-done local row is idempotent" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Local Ticket",
        .status = "done",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    const outcome = try setItemStatus(store, gpa, fc.clock(), .{ .id = "t1", .status = .done });
    switch (outcome) {
        .ok => |item| {
            defer item.deinit(gpa);
            try std.testing.expectEqualStrings("tk-1", item.display_id);
            try std.testing.expectEqualStrings("Local Ticket", item.title);
            try std.testing.expectEqual(ItemClass.ticket, item.item_class);
            try std.testing.expectEqual(ItemStatus.done, item.status);
        },
        .not_found => return error.UnexpectedNotFound,
        .locked_done => return error.UnexpectedLockedDone,
    }

    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
    const row = (try conn.row("select updated_at from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(0));
}

test "setItemStatus: done Ticket cannot move to active (locked_done)" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Local Ticket",
        .status = "done",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    const outcome = try setItemStatus(store, gpa, fc.clock(), .{ .id = "t1", .status = .active });
    switch (outcome) {
        .locked_done => |item_class| try std.testing.expectEqual(ItemClass.ticket, item_class),
        else => return error.ExpectedLockedDone,
    }

    const row = (try conn.row("select status, updated_at from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("done", row.text(0));
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(1));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
    const seq = (try conn.row("select value from sequences where name = 'mutation_seq'", .{})) orelse return error.ExpectedRow;
    defer seq.deinit();
    try std.testing.expectEqual(@as(i64, 0), seq.int(0));
}

test "setItemStatus: done Epic cannot move to open (locked_done)" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "tk-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Local Epic",
        .status = "done",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    const outcome = try setItemStatus(store, gpa, fc.clock(), .{ .id = "e1", .status = .open });
    switch (outcome) {
        .locked_done => |item_class| try std.testing.expectEqual(ItemClass.epic, item_class),
        else => return error.ExpectedLockedDone,
    }

    const row = (try conn.row("select status, updated_at from items where id = 'e1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("done", row.text(0));
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(1));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
    const seq = (try conn.row("select value from sequences where name = 'mutation_seq'", .{})) orelse return error.ExpectedRow;
    defer seq.deinit();
    try std.testing.expectEqual(@as(i64, 0), seq.int(0));
}

test "setItemStatus: done Backend Ticket cannot move to active (locked_done)" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "bt1",
        .display = "GH#7",
        .title = "Backend Ticket",
        .status = "done",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "7",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    const outcome = try setItemStatus(store, gpa, fc.clock(), .{ .id = "bt1", .status = .active });
    switch (outcome) {
        .locked_done => |item_class| try std.testing.expectEqual(ItemClass.ticket, item_class),
        else => return error.ExpectedLockedDone,
    }

    const row = (try conn.row("select status, updated_at from items where id = 'bt1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("done", row.text(0));
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(1));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
    const seq = (try conn.row("select value from sequences where name = 'mutation_seq'", .{})) orelse return error.ExpectedRow;
    defer seq.deinit();
    try std.testing.expectEqual(@as(i64, 0), seq.int(0));
}

test "items_no_escape_from_done trigger rejects direct done->active UPDATE" {
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Local Ticket",
        .status = "done",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    try conn.execNoArgs("begin immediate");
    errdefer conn.rollback();
    try std.testing.expectError(
        error.ConstraintTrigger,
        conn.execNoArgs("update items set status = 'active' where id = 't1'"),
    );
    conn.rollback();

    const row = (try conn.row("select status, updated_at from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("done", row.text(0));
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(1));
}

test "setItemStatus: rolls back current state and Mutation sequence when outbox insert fails" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Backend Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });
    try conn.execNoArgs(
        \\create trigger fail_set_item_status_mutation
        \\before insert on mutations
        \\when new.mutation_type = 'set_item_status'
        \\begin
        \\  select raise(abort, 'forced set_item_status failure');
        \\end
    );

    try std.testing.expectError(error.ConstraintTrigger, setItemStatus(store, gpa, fc.clock(), .{ .id = "t1", .status = .done }));

    const row = (try conn.row("select status, updated_at from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("open", row.text(0));
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(1));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
    const seq = (try conn.row("select value from sequences where name = 'mutation_seq'", .{})) orelse return error.ExpectedRow;
    defer seq.deinit();
    try std.testing.expectEqual(@as(i64, 0), seq.int(0));
}

test "setItemStatus: completing a blocker makes the blocked Ticket ready" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "tk-1",
        .title = "Blocked Ticket",
        .priority = "P2",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocker",
        .display = "tk-2",
        .title = "Blocking Item",
        .priority = "P3",
        .status = "active",
        .created_seq = 2,
    });
    try TmpStore.insertDependency(conn, "blocker", "blocked");

    try std.testing.expectEqual(NextOutcome.no_ready_ticket, try nextReadyTicket(store, gpa, .{}));
    {
        const blocked_rows = try listRows(store, gpa, .{ .view = .blocked });
        defer freeListRows(gpa, blocked_rows);
        try expectDisplayIds(blocked_rows, &.{"tk-1"});
    }

    const outcome = try setItemStatus(store, gpa, fc.clock(), .{ .id = "blocker", .status = .done });
    switch (outcome) {
        .ok => |item| item.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
        .locked_done => return error.UnexpectedLockedDone,
    }

    const next = try nextReadyTicket(store, gpa, .{});
    switch (next) {
        .ticket => |ticket| {
            defer ticket.deinit(gpa);
            try std.testing.expectEqualStrings("tk-1", ticket.display_id);
        },
        else => return error.ExpectedReadyTicket,
    }
    {
        const ready_rows = try listRows(store, gpa, .{ .view = .ready });
        defer freeListRows(gpa, ready_rows);
        try expectDisplayIds(ready_rows, &.{"tk-1"});
    }
    {
        const blocked_rows = try listRows(store, gpa, .{ .view = .blocked });
        defer freeListRows(gpa, blocked_rows);
        try std.testing.expectEqual(@as(usize, 0), blocked_rows.len);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// updateItem tests
// ──────────────────────────────────────────────────────────────────────────────

test "updateItem: returns not_found for unknown id" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "no-such-id",
        .item_class = .ticket,
        .title = "X",
    });
    try std.testing.expectEqual(UpdateOutcome.not_found, outcome);
}

test "updateItem: updates title and body of a local Ticket without emitting a Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Old title",
        .body = "Old body",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .title = "New title",
        .body = "New body",
    });
    switch (outcome) {
        .ok => |u| {
            defer u.deinit(gpa);
            try std.testing.expectEqualStrings("tk-1", u.display_id);
            try std.testing.expectEqualStrings("New title", u.title);
        },
        .not_found => return error.UnexpectedNotFound,
    }

    const row = (try conn.row("select title, body from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("New title", row.text(0));
    try std.testing.expectEqualStrings("New body", row.text(1));

    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "updateItem: updates title and body of a Backend Ticket and emits update_ticket Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Old title",
        .body = "Old body",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .title = "New title",
        .body = "New body",
    });
    switch (outcome) {
        .ok => |u| {
            defer u.deinit(gpa);
            try std.testing.expectEqualStrings("GH#1", u.display_id);
        },
        .not_found => return error.UnexpectedNotFound,
    }

    try std.testing.expectEqual(@as(i64, 1), try TmpStore.mutationCount(conn));

    const row = (try conn.row(
        "select mutation_type, item_id, item_class, payload_json from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("update_ticket", row.text(0));
    try std.testing.expectEqualStrings("t1", row.text(1));
    try std.testing.expectEqualStrings("ticket", row.text(2));
    try std.testing.expectEqualStrings("{\"title\":\"New title\",\"body\":\"New body\"}", row.text(3));
}

test "updateItem: idempotent update does not change updated_at and emits no Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Same title",
        .body = "Same body",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .title = "Same title",
        .body = "Same body",
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));

    const row = (try conn.row("select updated_at from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(0));
}

test "updateItem: priority change on local Ticket does not emit a Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Ticket",
        .priority = "P2",
        .created_seq = 1,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .priority = .P0,
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    const row = (try conn.row("select priority from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("P0", row.text(0));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "updateItem: priority change on Backend Ticket does not emit a Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Backend ticket",
        .priority = "P3",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .priority = .P1,
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    const row = (try conn.row("select priority from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("P1", row.text(0));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "updateItem: set parent on local Ticket updates container_id without emitting a Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Local Ticket",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "tk-2",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Local Epic",
        .created_seq = 2,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .parent = .{ .set = "e1" },
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    const row = (try conn.row("select container_id, container_class from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("e1", row.text(0));
    try std.testing.expectEqualStrings("epic", row.text(1));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "updateItem: set parent on Backend Ticket emits add_ticket_to_epic Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "GH#10",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "10",
        .created_seq = 2,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .parent = .{ .set = "e1" },
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    const row = (try conn.row("select mutation_type, payload_json from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("add_ticket_to_epic", row.text(0));
    try std.testing.expectEqualStrings("{\"epic_id\":\"e1\"}", row.text(1));
}

test "updateItem: move between Epics on Backend Ticket emits remove then add Mutations" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "old-epic",
        .display = "GH#10",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Old Epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "10",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "new-epic",
        .display = "GH#20",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "New Epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "20",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Ticket",
        .container_id = "old-epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 3,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .parent = .{ .set = "new-epic" },
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    try std.testing.expectEqual(@as(i64, 2), try TmpStore.mutationCount(conn));

    const r1 = (try conn.row("select mutation_type, payload_json from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer r1.deinit();
    try std.testing.expectEqualStrings("remove_ticket_from_epic", r1.text(0));
    try std.testing.expectEqualStrings("{\"epic_id\":\"old-epic\"}", r1.text(1));

    const r2 = (try conn.row("select mutation_type, payload_json from mutations where sequence = 2", .{})) orelse return error.ExpectedRow;
    defer r2.deinit();
    try std.testing.expectEqualStrings("add_ticket_to_epic", r2.text(0));
    try std.testing.expectEqualStrings("{\"epic_id\":\"new-epic\"}", r2.text(1));
}

test "updateItem: combined parent-move and title change emits Mutations in correct order" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "old-epic",
        .display = "GH#10",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Old Epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "10",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "new-epic",
        .display = "GH#20",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "New Epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "20",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Old title",
        .body = "",
        .container_id = "old-epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 3,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .title = "New title",
        .parent = .{ .set = "new-epic" },
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    // Expect 3 mutations: remove, add, update_ticket — in that order.
    try std.testing.expectEqual(@as(i64, 3), try TmpStore.mutationCount(conn));

    const r1 = (try conn.row("select mutation_type from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer r1.deinit();
    try std.testing.expectEqualStrings("remove_ticket_from_epic", r1.text(0));

    const r2 = (try conn.row("select mutation_type from mutations where sequence = 2", .{})) orelse return error.ExpectedRow;
    defer r2.deinit();
    try std.testing.expectEqualStrings("add_ticket_to_epic", r2.text(0));

    const r3 = (try conn.row("select mutation_type from mutations where sequence = 3", .{})) orelse return error.ExpectedRow;
    defer r3.deinit();
    try std.testing.expectEqualStrings("update_ticket", r3.text(0));
}

test "updateItem: clear parent on Backend Ticket emits remove_ticket_from_epic Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "GH#10",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "10",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Ticket",
        .container_id = "e1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 2,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .parent = .clear,
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    const row = (try conn.row("select container_id from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqual(@as(?[]const u8, null), row.nullableText(0));

    try std.testing.expectEqual(@as(i64, 1), try TmpStore.mutationCount(conn));

    const mr = (try conn.row("select mutation_type from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer mr.deinit();
    try std.testing.expectEqualStrings("remove_ticket_from_epic", mr.text(0));
}

test "updateItem: clear parent is idempotent when Ticket has no parent" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "GH#1",
        .title = "Orphan ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "t1",
        .item_class = .ticket,
        .parent = .clear,
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "updateItem: Backend Epic title change emits update_epic Mutation" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: Store = .{ .conn = conn };
    var fc = FakeClock.init(default_fake_now_ms);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "GH#10",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Old Epic title",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "10",
        .created_seq = 1,
    });

    const outcome = try updateItem(store, gpa, fc.clock(), .{
        .id = "e1",
        .item_class = .epic,
        .title = "New Epic title",
    });
    switch (outcome) {
        .ok => |u| u.deinit(gpa),
        .not_found => return error.UnexpectedNotFound,
    }

    try std.testing.expectEqual(@as(i64, 1), try TmpStore.mutationCount(conn));

    const mr = (try conn.row("select mutation_type from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer mr.deinit();
    try std.testing.expectEqualStrings("update_epic", mr.text(0));
}
