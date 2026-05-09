//! Repository Store schema migrations.
//!
//! Slice 2 ships exactly one migration: the v1 skeleton. Migration 1
//! creates every table later slices populate (items, dependencies,
//! external_blockers, mutations, remotes, sync_cursors, sequences,
//! store_config, item_ids, schema_migrations) plus the indexes tied to
//! known v1 query paths.
//!
//! Migrations are applied inside a single transaction per migration. The
//! caller (currently `tk init`) handles connection setup (`PRAGMA
//! journal_mode = wal`, `PRAGMA foreign_keys = on`, `PRAGMA busy_timeout =
//! 5000`) before invoking `applyAll`.

const std = @import("std");
const sqlite = @import("sqlite.zig");

pub const Db = sqlite.Db;

/// Application ID written to `PRAGMA application_id` so an existing SQLite
/// file can be identified as a Ticket Repository Store. Spelled `TKDB` in
/// big-endian ASCII (`0x54 0x4B 0x44 0x42`).
pub const application_id: i32 = 0x544B4442;

pub const Migration = struct {
    version: u32,
    sql: [:0]const u8,
};

pub const migration_1: Migration = .{
    .version = 1,
    .sql =
    \\create table schema_migrations (
    \\    version integer primary key,
    \\    applied_at text not null
    \\) strict;
    \\
    \\create table sequences (
    \\    name text primary key check(name in ('item_created_seq','display_seq','mutation_seq')),
    \\    value integer not null check(value >= 0)
    \\) strict, without rowid;
    \\insert into sequences(name, value) values ('item_created_seq', 0);
    \\insert into sequences(name, value) values ('display_seq', 0);
    \\insert into sequences(name, value) values ('mutation_seq', 0);
    \\
    \\create table store_config (
    \\    key text primary key check(key in ('display_prefix')),
    \\    value text not null
    \\) strict, without rowid;
    \\
    \\create table items (
    \\    id text primary key,
    \\    display_value text not null collate nocase,
    \\    item_class text not null check(item_class in ('ticket','epic')),
    \\    ticket_kind text check(ticket_kind in ('task','bug')),
    \\    priority text check(priority in ('P0','P1','P2','P3','P4')),
    \\    title text not null check(length(title) > 0),
    \\    body text not null default '',
    \\    container_id text,
    \\    container_class text,
    \\    origin text not null check(origin in ('local','backend')),
    \\    backend_kind text,
    \\    backend_key text,
    \\    status text not null check(status in ('open','active','done')),
    \\    created_seq integer not null unique,
    \\    created_at text not null,
    \\    updated_at text not null,
    \\    display_source text not null generated always as ('display') stored,
    \\    check (
    \\        (item_class = 'ticket' and ticket_kind is not null and priority is not null)
    \\        or
    \\        (item_class = 'epic' and ticket_kind is null and priority is null)
    \\    ),
    \\    check (
    \\        (item_class = 'epic' and container_id is null and container_class is null)
    \\        or
    \\        (item_class = 'ticket')
    \\    ),
    \\    check (
    \\        (container_id is null and container_class is null)
    \\        or
    \\        (container_id is not null and container_class = 'epic')
    \\    ),
    \\    check (
    \\        (origin = 'local' and backend_kind is null and backend_key is null)
    \\        or
    \\        (origin = 'backend' and backend_kind is not null and backend_key is not null)
    \\    ),
    \\    foreign key (container_id, container_class) references items(id, item_class) deferrable initially deferred,
    \\    foreign key (display_value, id, display_source) references item_ids(value, item_id, source) deferrable initially deferred
    \\) strict;
    \\create unique index items_backend_unique on items(backend_kind, backend_key) where backend_kind is not null;
    \\create index items_container_idx on items(container_id) where container_id is not null;
    \\create index items_next_idx on items(priority, created_seq) where status = 'open' and item_class = 'ticket';
    \\create unique index items_id_class_unique on items(id, item_class);
    \\
    \\create table item_ids (
    \\    value text primary key collate nocase,
    \\    source text not null check(source in ('display','alias')),
    \\    item_id text not null references items(id) on delete restrict deferrable initially deferred,
    \\    created_at text not null,
    \\    check (length(value) > 0 and value glob '[A-Za-z0-9._/:#-]*')
    \\) strict, without rowid;
    \\create unique index item_ids_value_id_source on item_ids(value, item_id, source);
    \\create unique index item_ids_one_display_per_item on item_ids(item_id) where source = 'display';
    \\
    \\create table dependencies (
    \\    blocking_id text not null references items(id) on delete restrict,
    \\    blocked_id text not null references items(id) on delete restrict,
    \\    created_at text not null,
    \\    primary key (blocking_id, blocked_id),
    \\    check (blocking_id <> blocked_id)
    \\) strict, without rowid;
    \\create index dependencies_blocked_idx on dependencies(blocked_id);
    \\create index dependencies_blocking_idx on dependencies(blocking_id);
    \\
    \\create table external_blockers (
    \\    id text primary key,
    \\    item_id text not null references items(id) on delete restrict,
    \\    reason text not null check(length(reason) > 0),
    \\    created_at text not null,
    \\    resolved_at text
    \\) strict, without rowid;
    \\create index external_blockers_unresolved_idx on external_blockers(item_id) where resolved_at is null;
    \\
    \\create table mutations (
    \\    sequence integer primary key,
    \\    mutation_type text not null check(mutation_type in (
    \\        'create_ticket','create_epic',
    \\        'update_ticket','update_epic',
    \\        'add_ticket_to_epic','remove_ticket_from_epic',
    \\        'add_dependency','remove_dependency',
    \\        'add_external_blocker','resolve_external_blocker',
    \\        'transition_status','set_priority',
    \\        'promote_ticket','promote_epic'
    \\    )),
    \\    item_id text not null,
    \\    item_class text not null check(item_class in ('ticket','epic')),
    \\    payload_json text not null check(json_valid(payload_json)),
    \\    state text not null check(state in ('pending','failed','skipped','applied')),
    \\    failure_json text check(failure_json is null or json_valid(failure_json)),
    \\    created_at text not null,
    \\    state_changed_at text not null,
    \\    foreign key (item_id, item_class) references items(id, item_class),
    \\    check (
    \\        (state in ('pending','applied') and failure_json is null)
    \\        or
    \\        (state = 'failed' and failure_json is not null)
    \\        or
    \\        (state = 'skipped')
    \\    )
    \\) strict;
    \\create index mutations_state_idx on mutations(state, sequence);
    \\
    \\create table remotes (
    \\    name text primary key check(name = 'primary'),
    \\    backend_kind text not null check(backend_kind in ('github','jira')),
    \\    config_json text not null check(json_valid(config_json)),
    \\    created_at text not null,
    \\    updated_at text not null
    \\) strict, without rowid;
    \\
    \\create table sync_cursors (
    \\    remote_name text primary key references remotes(name) on delete restrict,
    \\    backend_kind text not null,
    \\    last_applied_sequence integer not null default 0,
    \\    updated_at text not null
    \\) strict, without rowid;
    \\
    \\create trigger dependencies_no_cycle before insert on dependencies
    \\for each row when exists (
    \\    with recursive reachable(id) as (
    \\        select new.blocking_id
    \\        union
    \\        select dependencies.blocking_id
    \\          from dependencies, reachable
    \\         where dependencies.blocked_id = reachable.id
    \\    )
    \\    select 1 from reachable where id = new.blocked_id
    \\) begin
    \\    select raise(abort, 'dependency cycle');
    \\end;
    ,
};

pub const all_migrations: []const Migration = &.{migration_1};

pub const ApplyError = sqlite.Error || error{
    StoreFromFutureVersion,
};

/// Apply any migrations not yet recorded in `schema_migrations`. Each
/// migration runs inside its own transaction. The application_id and
/// user_version pragmas are kept in sync with the highest applied version.
pub fn applyAll(db: *Db, now_iso: []const u8) ApplyError!void {
    const has_migrations_table = try schemaMigrationsExists(db);
    const recorded_version: i64 = if (has_migrations_table)
        try db.queryOneInt("select coalesce(max(version), 0) from schema_migrations") orelse 0
    else
        0;
    const max_known: i64 = @intCast(all_migrations[all_migrations.len - 1].version);

    if (recorded_version > max_known) return error.StoreFromFutureVersion;

    for (all_migrations) |mig| {
        if (mig.version <= recorded_version) continue;

        try db.exec("begin");
        errdefer db.exec("rollback") catch {};

        try db.exec(mig.sql);

        // Buffer is oversized for these three statements; bufPrintZ
        // returning NoSpaceLeft would mean an unreachable program bug, not
        // an OOM, so an unreachable lets the assertion catch it in debug.
        var buf: [512]u8 = undefined;
        const tail_sql = std.fmt.bufPrintZ(
            &buf,
            \\insert into schema_migrations(version, applied_at) values ({d}, '{s}');
            \\pragma application_id = {d};
            \\pragma user_version = {d};
            ,
            .{ mig.version, now_iso, application_id, mig.version },
        ) catch unreachable;
        try db.exec(tail_sql);

        try db.exec("commit");
    }
}

pub fn currentVersion(db: *Db) sqlite.Error!u32 {
    if (!try schemaMigrationsExists(db)) return 0;
    const recorded = try db.queryOneInt("select coalesce(max(version), 0) from schema_migrations") orelse 0;
    return @intCast(recorded);
}

fn schemaMigrationsExists(db: *Db) sqlite.Error!bool {
    const present = try db.queryOneInt(
        "select 1 from sqlite_master where type='table' and name='schema_migrations'",
    );
    return present != null;
}

test "applyAll on empty db creates v1 schema and records migration" {
    var db = try Db.open(":memory:", .{});
    defer db.close();

    try applyAll(&db, "2026-05-09T00:00:00.000Z");

    const v = try currentVersion(&db);
    try std.testing.expectEqual(@as(u32, 1), v);

    const app_id = (try db.queryOneInt("pragma application_id")).?;
    try std.testing.expectEqual(@as(i64, application_id), app_id);

    const user_v = (try db.queryOneInt("pragma user_version")).?;
    try std.testing.expectEqual(@as(i64, 1), user_v);

    const tables = (try db.queryOneInt(
        "select count(*) from sqlite_master where type='table' and name in ('items','item_ids','dependencies','external_blockers','mutations','remotes','sync_cursors','sequences','store_config','schema_migrations')",
    )).?;
    try std.testing.expectEqual(@as(i64, 10), tables);

    const seq_count = (try db.queryOneInt("select count(*) from sequences")).?;
    try std.testing.expectEqual(@as(i64, 3), seq_count);
}

test "applyAll is idempotent on a current store" {
    var db = try Db.open(":memory:", .{});
    defer db.close();

    try applyAll(&db, "2026-05-09T00:00:00.000Z");
    try applyAll(&db, "2026-05-09T00:00:01.000Z");

    const count = (try db.queryOneInt("select count(*) from schema_migrations")).?;
    try std.testing.expectEqual(@as(i64, 1), count);
}

test "applyAll rejects future version" {
    var db = try Db.open(":memory:", .{});
    defer db.close();

    try applyAll(&db, "2026-05-09T00:00:00.000Z");
    try db.exec("insert into schema_migrations(version, applied_at) values (999, '2099-01-01T00:00:00.000Z')");

    try std.testing.expectError(error.StoreFromFutureVersion, applyAll(&db, "2026-05-09T00:00:00.000Z"));
}

const test_helpers = struct {
    fn freshDb() !Db {
        var db = try Db.open(":memory:", .{});
        errdefer db.close();
        try applyAll(&db, "2026-05-09T00:00:00.000Z");
        return db;
    }

    /// Inserts a Display ID resolver row and an item, in one deferred-FK
    /// transaction. Items must have a matching `source = 'display'` row in
    /// `item_ids` to satisfy the deferred composite foreign key.
    fn insertTicket(db: *Db, args: struct {
        id: []const u8,
        display: []const u8,
        title: []const u8 = "title",
        priority: []const u8 = "P2",
        kind: []const u8 = "task",
        status: []const u8 = "open",
        created_seq: i64 = 1,
    }) !void {
        var buf: [2048]u8 = undefined;
        const sql = try std.fmt.bufPrintZ(&buf,
            \\begin;
            \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
            \\values ('{s}', '{s}', 'ticket', '{s}', '{s}', '{s}', '', 'local', '{s}', {d}, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z');
            \\insert into item_ids(value, source, item_id, created_at)
            \\values ('{s}', 'display', '{s}', '2026-05-09T00:00:00.000Z');
            \\commit;
        ,
            .{ args.id, args.display, args.kind, args.priority, args.title, args.status, args.created_seq, args.display, args.id },
        );
        try db.exec(sql);
    }
};

test "items: epic with ticket_kind violates check" {
    var db = try test_helpers.freshDb();
    defer db.close();
    const sql: [:0]const u8 =
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('e1', 'tk-1', 'epic', 'task', null, 'epic', '', 'local', 'open', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}

test "items: ticket without priority violates check" {
    var db = try test_helpers.freshDb();
    defer db.close();
    const sql: [:0]const u8 =
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('t1', 'tk-1', 'ticket', 'task', null, 'title', '', 'local', 'open', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}

test "items: invalid status rejected" {
    var db = try test_helpers.freshDb();
    defer db.close();
    const sql: [:0]const u8 =
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('t1', 'tk-1', 'ticket', 'task', 'P2', 'title', '', 'local', 'blocked', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}

test "items: empty title rejected" {
    var db = try test_helpers.freshDb();
    defer db.close();
    const sql: [:0]const u8 =
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('t1', 'tk-1', 'ticket', 'task', 'P2', '', '', 'local', 'open', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}

test "items: contained ticket pointing at non-Epic rejected" {
    var db = try test_helpers.freshDb();
    defer db.close();
    try test_helpers.insertTicket(&db, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    const sql: [:0]const u8 =
        \\begin;
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, container_id, container_class, created_seq, created_at, updated_at)
        \\values ('t2', 'tk-2', 'ticket', 'task', 'P2', 'child', '', 'local', 'open', 't1', 'ticket', 2, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z');
        \\insert into item_ids(value, source, item_id, created_at) values ('tk-2', 'display', 't2', '2026-05-09T00:00:00.000Z');
        \\commit;
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}

test "dependencies: self-edge rejected" {
    var db = try test_helpers.freshDb();
    defer db.close();
    try test_helpers.insertTicket(&db, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    const sql: [:0]const u8 =
        \\insert into dependencies(blocking_id, blocked_id, created_at)
        \\values ('t1', 't1', '2026-05-09T00:00:00.000Z')
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}

test "dependencies: cycle rejected" {
    var db = try test_helpers.freshDb();
    defer db.close();
    try test_helpers.insertTicket(&db, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    try test_helpers.insertTicket(&db, .{ .id = "t2", .display = "tk-2", .created_seq = 2 });
    try db.exec(
        \\insert into dependencies(blocking_id, blocked_id, created_at)
        \\values ('t1', 't2', '2026-05-09T00:00:00.000Z')
    );
    const sql: [:0]const u8 =
        \\insert into dependencies(blocking_id, blocked_id, created_at)
        \\values ('t2', 't1', '2026-05-09T00:00:00.000Z')
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}

test "item_ids: duplicate value rejected" {
    var db = try test_helpers.freshDb();
    defer db.close();
    try test_helpers.insertTicket(&db, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    try test_helpers.insertTicket(&db, .{ .id = "t2", .display = "tk-2", .created_seq = 2 });
    const sql: [:0]const u8 =
        \\insert into item_ids(value, source, item_id, created_at)
        \\values ('tk-1', 'alias', 't2', '2026-05-09T00:00:00.000Z')
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}

test "external_blockers: empty reason rejected" {
    var db = try test_helpers.freshDb();
    defer db.close();
    try test_helpers.insertTicket(&db, .{ .id = "t1", .display = "tk-1", .created_seq = 1 });
    const sql: [:0]const u8 =
        \\insert into external_blockers(id, item_id, reason, created_at)
        \\values ('eb1', 't1', '', '2026-05-09T00:00:00.000Z')
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}

test "items: missing display resolver row rejected on commit" {
    var db = try test_helpers.freshDb();
    defer db.close();
    // Without inserting an item_ids row, the deferred composite FK
    // (display_value, id, 'display') -> item_ids(value, item_id, source)
    // must fail at commit time.
    const sql: [:0]const u8 =
        \\begin;
        \\insert into items(id, display_value, item_class, ticket_kind, priority, title, body, origin, status, created_seq, created_at, updated_at)
        \\values ('t1', 'tk-1', 'ticket', 'task', 'P2', 'title', '', 'local', 'open', 1, '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z');
        \\commit;
    ;
    try std.testing.expectError(error.ExecFailed, db.exec(sql));
}
