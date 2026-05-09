//! Repository Store schema migrations.
//!
//! Migration 1 creates the v1 schema skeleton: items, item_ids, dependencies,
//! external_blockers, mutations, sequences, store_config, remotes,
//! sync_cursors, and schema_migrations, plus the indexes tied to known v1
//! query paths.
//!
//! Each migration runs inside its own transaction. The caller is responsible
//! for connection-level setup (foreign_keys, busy_timeout, journal_mode)
//! before invoking `applyAll`.

const std = @import("std");
const zqlite = @import("zqlite");

pub const Conn = zqlite.Conn;

/// Application ID written to `pragma application_id` so an existing SQLite
/// file can be identified as a Ticket Repository Store. Spelled `TKDB` in
/// big-endian ASCII (`0x54 0x4B 0x44 0x42`).
pub const application_id: i32 = 0x544B4442;

pub const Migration = struct {
    version: u32,
    sql: [*:0]const u8,
};

pub const migration_1: Migration = .{
    .version = 1,
    .sql = @embedFile("migrations/001_repository_store.sql"),
};

pub const all_migrations: []const Migration = &.{migration_1};

/// Errors that the SQL helpers in this module can return. zqlite's
/// `prepare` (used internally by `exec` and `row`) adds `MultipleStatements`
/// in debug builds when a non-first SQL statement is passed to a single-shot
/// API; bubble it up rather than mask it.
pub const QueryError = zqlite.Error || error{MultipleStatements};

pub const ApplyError = QueryError || error{StoreFromFutureVersion};

/// Last SQLite error message captured by `applyAll`. zqlite's `rollback`
/// runs a successful `rollback` statement, which clears the per-connection
/// `sqlite3_errmsg`; capture it here before that happens so callers reading
/// `migrations.lastError()` see the original migration failure rather than
/// the post-rollback empty string.
var last_error_buf: [256]u8 = undefined;
var last_error_len: usize = 0;

pub fn lastError() []const u8 {
    return last_error_buf[0..last_error_len];
}

pub fn applyAll(conn: Conn, now_iso: []const u8) ApplyError!void {
    last_error_len = 0;

    const recorded_version: i64 = if (try schemaMigrationsExists(conn))
        (try queryOptionalInt(conn, "select coalesce(max(version), 0) from schema_migrations")) orelse 0
    else
        0;
    const max_known: i64 = @intCast(all_migrations[all_migrations.len - 1].version);
    if (recorded_version > max_known) return error.StoreFromFutureVersion;

    for (all_migrations) |mig| {
        if (mig.version <= recorded_version) continue;
        applyOne(conn, mig, now_iso) catch |err| {
            captureError(conn);
            conn.rollback();
            return err;
        };
    }
}

fn applyOne(conn: Conn, mig: Migration, now_iso: []const u8) ApplyError!void {
    try conn.transaction();

    try conn.execNoArgs(mig.sql);

    // application_id and user_version pragmas don't accept `?` parameters.
    // The values are a compile-time `i32` constant and a `u32` from the
    // hardcoded migration list, so the buffer is statically oversized and
    // bufPrintZ's NoSpaceLeft is unreachable.
    var buf: [128]u8 = undefined;
    const pragma_sql = std.fmt.bufPrintZ(
        &buf,
        "pragma application_id = {d}; pragma user_version = {d};",
        .{ application_id, mig.version },
    ) catch unreachable;
    try conn.execNoArgs(pragma_sql);

    try conn.exec(
        "insert into schema_migrations(version, applied_at) values (?1, ?2)",
        .{ @as(i64, @intCast(mig.version)), now_iso },
    );

    try conn.commit();
}

fn captureError(conn: Conn) void {
    const msg = std.mem.span(conn.lastError());
    const n = @min(msg.len, last_error_buf.len);
    @memcpy(last_error_buf[0..n], msg[0..n]);
    last_error_len = n;
}

pub fn currentVersion(conn: Conn) QueryError!u32 {
    if (!try schemaMigrationsExists(conn)) return 0;
    const recorded = (try queryOptionalInt(conn, "select coalesce(max(version), 0) from schema_migrations")) orelse 0;
    return @intCast(recorded);
}

fn schemaMigrationsExists(conn: Conn) QueryError!bool {
    const present = try queryOptionalInt(
        conn,
        "select 1 from sqlite_master where type='table' and name='schema_migrations'",
    );
    return present != null;
}

/// Returns the first integer column of the first row, or null if no row.
pub fn queryOptionalInt(conn: Conn, sql: []const u8) QueryError!?i64 {
    if (try conn.row(sql, .{})) |r| {
        defer r.deinit();
        return r.int(0);
    }
    return null;
}

// ---- Tests ---------------------------------------------------------------

fn openMemoryDb() !Conn {
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    errdefer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    return conn;
}

test "applyAll on empty db creates v1 schema and records migration" {
    const conn = try openMemoryDb();
    defer conn.close();

    try applyAll(conn, "2026-05-09T00:00:00.000Z");

    try std.testing.expectEqual(@as(u32, 1), try currentVersion(conn));

    try std.testing.expectEqual(
        @as(?i64, application_id),
        try queryOptionalInt(conn, "pragma application_id"),
    );

    try std.testing.expectEqual(
        @as(?i64, 1),
        try queryOptionalInt(conn, "pragma user_version"),
    );

    // Behavioral check: the v1 schema can hold a Ticket round-trip. The
    // helper inserts an item plus its display resolver row and commits the
    // deferred composite FK; querying back the Display ID confirms the
    // schema is wired correctly without locking an exact table count.
    try test_helpers.insertTicket(conn, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    if (try conn.row("select value from item_ids where item_id = 't1' and source = 'display'", .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings("tk-1", r.text(0));
    } else return error.ExpectedRow;

    // Behavioral check: the documented sequence-allocation pattern returns a
    // monotonic value. This pins the contract used by the create_ticket /
    // create_epic Mutation Apply path rather than the literal row count of
    // pre-seeded sequences.
    if (try conn.row(
        "update sequences set value = value + 1 where name = 'item_created_seq' returning value",
        .{},
    )) |r| {
        defer r.deinit();
        try std.testing.expectEqual(@as(i64, 1), r.int(0));
    } else return error.ExpectedRow;
}

test "applyAll is idempotent on a current store" {
    const conn = try openMemoryDb();
    defer conn.close();

    try applyAll(conn, "2026-05-09T00:00:00.000Z");
    try applyAll(conn, "2026-05-09T00:00:01.000Z");

    try std.testing.expectEqual(
        @as(?i64, 1),
        try queryOptionalInt(conn, "select count(*) from schema_migrations"),
    );
}

test "applyAll rejects future version" {
    const conn = try openMemoryDb();
    defer conn.close();

    try applyAll(conn, "2026-05-09T00:00:00.000Z");
    try conn.exec(
        "insert into schema_migrations(version, applied_at) values (?1, ?2)",
        .{ @as(i64, 999), "2099-01-01T00:00:00.000Z" },
    );

    try std.testing.expectError(
        error.StoreFromFutureVersion,
        applyAll(conn, "2026-05-09T00:00:00.000Z"),
    );
}

test "applyAll captures sqlite error message before rollback clears it" {
    const conn = try openMemoryDb();
    defer conn.close();

    // Pre-create one of the tables migration_1 creates so the migration's
    // `create table items` fails. The errdefer rollback would otherwise
    // clear sqlite3_errmsg before applyAll returns; lastError() must still
    // expose the original "table items already exists" message.
    try conn.execNoArgs("create table items (x integer)");

    const result = applyAll(conn, "2026-05-09T00:00:00.000Z");
    try std.testing.expectError(error.Error, result);
    const msg = lastError();
    try std.testing.expect(msg.len > 0);
    try std.testing.expect(std.mem.indexOf(u8, msg, "items") != null);
}

test "applyAll records applied_at from the caller-supplied clock" {
    const conn = try openMemoryDb();
    defer conn.close();
    const fixed = "2026-05-09T12:34:56.789Z";

    try applyAll(conn, fixed);

    if (try conn.row("select applied_at from schema_migrations where version = 1", .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings(fixed, r.text(0));
    } else return error.ExpectedRow;
}

const test_helpers = struct {
    fn freshDb() !Conn {
        const conn = try openMemoryDb();
        errdefer conn.close();
        try applyAll(conn, "2026-05-09T00:00:00.000Z");
        return conn;
    }

    /// Inserts a Display ID resolver row and an item, in one deferred-FK
    /// transaction. Items must have a matching `source = 'display'` row in
    /// `item_ids` to satisfy the deferred composite foreign key.
    fn insertTicket(conn: Conn, args: struct {
        id: []const u8,
        display: []const u8,
        title: []const u8 = "title",
        priority: []const u8 = "P2",
        kind: []const u8 = "task",
        status: []const u8 = "open",
        created_seq: i64 = 1,
    }) !void {
        try conn.transaction();
        errdefer conn.rollback();
        try conn.exec(
            \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
            \\values (?1, ?2, 'ticket', ?3, ?4, ?5, '', 'local', ?6, ?7, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
        ,
            .{ args.id, args.display, args.kind, args.priority, args.title, args.status, args.created_seq },
        );
        try conn.exec(
            "insert into item_ids(value, source, item_id, created_at) values (?1, 'display', ?2, '2026-05-09T00:00:00.000Z')",
            .{ args.display, args.id },
        );
        try conn.commit();
    }

    fn expectRejected(conn: Conn, sql: [*:0]const u8) !void {
        const result = conn.execNoArgs(sql);
        if (result) |_| {
            std.debug.print("expected SQL to fail but it succeeded:\n{s}\n", .{sql});
            return error.ExpectedConstraintFailure;
        } else |err| switch (err) {
            error.Constraint,
            error.ConstraintCheck,
            error.ConstraintForeignKey,
            error.ConstraintNotNull,
            error.ConstraintPrimaryKey,
            error.ConstraintTrigger,
            error.ConstraintUnique,
            => {},
            else => return err,
        }
    }
};

test "Repository Store rejects an Epic with a Ticket Kind" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.expectRejected(conn,
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('e1', 'tk-1', 'epic', 'task', null, 'epic', '', 'local', 'open', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store rejects a Ticket without a Priority" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.expectRejected(conn,
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('t1', 'tk-1', 'ticket', 'task', null, 'title', '', 'local', 'open', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store rejects an Item with an unknown Item Status" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.expectRejected(conn,
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('t1', 'tk-1', 'ticket', 'task', 'P2', 'title', '', 'local', 'blocked', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store rejects a Ticket with an empty title" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.expectRejected(conn,
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('t1', 'tk-1', 'ticket', 'task', 'P2', '', '', 'local', 'open', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store rejects a Ticket contained by a non-Epic" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.insertTicket(conn, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    try test_helpers.expectRejected(conn,
        \\begin;
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, container_id, container_class, created_seq, created_at, updated_at)
        \\values ('t2', 'tk-2', 'ticket', 'task', 'P2', 'child', '', 'local', 'open', 't1', 'ticket', 2, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z');
        \\insert into item_ids(value, source, item_id, created_at) values ('tk-2', 'display', 't2', '2026-05-09T00:00:00.000Z');
        \\commit;
    );
}

test "Repository Store rejects a Dependency where Blocking Item equals Blocked Item" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.insertTicket(conn, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    try test_helpers.expectRejected(conn,
        \\insert into dependencies(blocking_id, blocked_id, created_at)
        \\values ('t1', 't1', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store rejects a Dependency cycle" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.insertTicket(conn, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    try test_helpers.insertTicket(conn, .{ .id = "t2", .display = "tk-2", .created_seq = 2 });
    try conn.execNoArgs(
        \\insert into dependencies(blocking_id, blocked_id, created_at)
        \\values ('t1', 't2', '2026-05-09T00:00:00.000Z')
    );
    try test_helpers.expectRejected(conn,
        \\insert into dependencies(blocking_id, blocked_id, created_at)
        \\values ('t2', 't1', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store rejects a duplicate Display ID or Alias" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.insertTicket(conn, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    try test_helpers.insertTicket(conn, .{ .id = "t2", .display = "tk-2", .created_seq = 2 });
    try test_helpers.expectRejected(conn,
        \\insert into item_ids(value, source, item_id, created_at)
        \\values ('tk-1', 'alias', 't2', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store rejects an External Blocker with an empty reason" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.insertTicket(conn, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    try test_helpers.expectRejected(conn,
        \\insert into external_blockers(id, item_id, reason, created_at)
        \\values ('eb1', 't1', '', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store rejects a Display ID or Alias with disallowed characters" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.insertTicket(conn, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    // Trailing characters outside [A-Za-z0-9._/:#-] must be rejected. SQLite
    // GLOB's leading-anchor behavior makes `glob '[class]*'` validate only
    // the first byte; the schema must use a negative-class match instead.
    try test_helpers.expectRejected(conn,
        \\insert into item_ids(value, source, item_id, created_at)
        \\values ('tk-1!', 'alias', 't1', '2026-05-09T00:00:00.000Z')
    );
    try test_helpers.expectRejected(conn,
        \\insert into item_ids(value, source, item_id, created_at)
        \\values ('a b', 'alias', 't1', '2026-05-09T00:00:00.000Z')
    );
    try test_helpers.expectRejected(conn,
        \\insert into item_ids(value, source, item_id, created_at)
        \\values ('a;drop', 'alias', 't1', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store enforces the documented V1 Mutation Type vocabulary" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    try test_helpers.insertTicket(conn, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });

    // set_item_status is the canonical name per CONTEXT.md V1 Mutation Type.
    try conn.exec(
        \\insert into mutations(sequence, mutation_type, item_id, item_class, payload_json, state, created_at, state_changed_at)
        \\values (?1, 'set_item_status', 't1', 'ticket', '{}', 'pending', ?2, ?2)
    , .{ @as(i64, 1), "2026-05-09T00:00:00.000Z" });

    // transition_status was an interim name that never appeared in the spec;
    // accepting it here would let a future regression smuggle the wrong
    // vocabulary into the mutation log.
    try test_helpers.expectRejected(conn,
        \\insert into mutations(sequence, mutation_type, item_id, item_class, payload_json, state, created_at, state_changed_at)
        \\values (2, 'transition_status', 't1', 'ticket', '{}', 'pending', '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
    );
}

test "Repository Store rejects an Item without a current Display ID" {
    const conn = try test_helpers.freshDb();
    defer conn.close();
    // Without inserting an item_ids row, the deferred composite FK
    // (display_value, id, 'display') -> item_ids(value, item_id, source)
    // must fail at commit time.
    try test_helpers.expectRejected(conn,
        \\begin;
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('t1', 'tk-1', 'ticket', 'task', 'P2', 'title', '', 'local', 'open', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z');
        \\commit;
    );
}
