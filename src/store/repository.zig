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

/// Outcome of opening an existing Repository Store.
///
/// `.git_rejected` may carry an allocator-owned trimmed stderr slice from Git;
/// the caller renders and frees it. `.ok` owns an open connection that must be
/// closed through `Store.close`.
pub const OpenOutcome = union(enum) {
    ok: Store,
    git_missing,
    spawn_failed,
    git_rejected: ?[]u8,
    git_output_unparseable,
    store_missing,
    not_ticket_store,
    store_from_future_version,
};

pub const OpenError = discovery.Error || migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Open the existing Repository Store for the current Git repository.
pub fn openExisting(gpa: std.mem.Allocator, runner: proc.Runner, cwd: std.Io.Dir) OpenError!OpenOutcome {
    const outcome = try discovery.discoverPaths(gpa, runner, cwd);
    switch (outcome) {
        .git_missing => return .git_missing,
        .spawn_failed => return .spawn_failed,
        .git_rejected => |maybe_msg| return .{ .git_rejected = maybe_msg },
        .git_output_unparseable => return .git_output_unparseable,
        .ok => {},
    }
    var paths = outcome.ok;
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
