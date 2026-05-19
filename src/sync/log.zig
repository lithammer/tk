//! Read helpers for `tk sync log`.
//!
//! `listMutations` returns the rows for the list view; `showMutation` returns
//! one row with the full decoded payload + raw failure_json for the detail
//! view. Both join `items` so the target Display ID rides on every result —
//! the user reads the Display ID, not the internal `items.id`.

const std = @import("std");
const Allocator = std.mem.Allocator;
const zqlite = @import("zqlite");

const migrations = @import("../store/migrations.zig");

/// Filter applied to `listMutations`. The default view (`.default`) hides
/// `applied` Mutations to mirror `tk list`'s hide-done convention — browsing
/// applied Mutations is deferred.
pub const ListFilter = enum {
    /// Pending + failed + skipped (the default).
    default,
    /// Only `state = 'pending'`.
    pending,
    /// Only `state = 'failed'`.
    failed,
    /// Only `state = 'skipped'`.
    skipped,
};

/// One row of the `tk sync log` list view.
pub const ListRow = struct {
    sequence: i64,
    state: []u8,
    mutation_type: []u8,
    target_display_id: []u8,
    created_at: []u8,
    /// Set only when `state = 'failed'`; the decoded `failure.detail` from
    /// the persisted `{"detail": "..."}` wrapper.
    failure_detail: ?[]u8,

    pub fn deinit(self: ListRow, gpa: Allocator) void {
        gpa.free(self.state);
        gpa.free(self.mutation_type);
        gpa.free(self.target_display_id);
        gpa.free(self.created_at);
        if (self.failure_detail) |d| gpa.free(d);
    }
};

/// Error set returned by `listMutations` and `showMutation`.
pub const LogError = migrations.QueryError || zqlite.Error || std.json.ParseError(std.json.Scanner) || error{
    OutOfMemory,
    MutationNotFound,
};

/// Return Mutation Log rows matching `filter` in ascending sequence order.
pub fn listMutations(
    conn: zqlite.Conn,
    gpa: Allocator,
    filter: ListFilter,
) LogError![]ListRow {
    var list: std.ArrayList(ListRow) = .empty;
    errdefer {
        for (list.items) |row| row.deinit(gpa);
        list.deinit(gpa);
    }

    const sql = switch (filter) {
        .default =>
        \\select m.sequence, m.state, m.mutation_type, i.display_value, m.created_at, m.failure_json
        \\  from mutations m
        \\  join items i on i.id = m.item_id and i.item_class = m.item_class
        \\ where m.state in ('pending', 'failed', 'skipped')
        \\ order by m.sequence asc
        ,
        .pending =>
        \\select m.sequence, m.state, m.mutation_type, i.display_value, m.created_at, m.failure_json
        \\  from mutations m
        \\  join items i on i.id = m.item_id and i.item_class = m.item_class
        \\ where m.state = 'pending'
        \\ order by m.sequence asc
        ,
        .failed =>
        \\select m.sequence, m.state, m.mutation_type, i.display_value, m.created_at, m.failure_json
        \\  from mutations m
        \\  join items i on i.id = m.item_id and i.item_class = m.item_class
        \\ where m.state = 'failed'
        \\ order by m.sequence asc
        ,
        .skipped =>
        \\select m.sequence, m.state, m.mutation_type, i.display_value, m.created_at, m.failure_json
        \\  from mutations m
        \\  join items i on i.id = m.item_id and i.item_class = m.item_class
        \\ where m.state = 'skipped'
        \\ order by m.sequence asc
        ,
    };

    var rows = try conn.rows(sql, .{});
    defer rows.deinit();

    while (rows.next()) |r| {
        const sequence = r.int(0);
        const state = try gpa.dupe(u8, r.text(1));
        errdefer gpa.free(state);
        const mutation_type = try gpa.dupe(u8, r.text(2));
        errdefer gpa.free(mutation_type);
        const target = try gpa.dupe(u8, r.text(3));
        errdefer gpa.free(target);
        const created_at = try gpa.dupe(u8, r.text(4));
        errdefer gpa.free(created_at);

        var failure_detail: ?[]u8 = null;
        if (r.nullableText(5)) |raw| {
            failure_detail = try decodeFailureDetail(gpa, raw);
        }
        errdefer if (failure_detail) |d| gpa.free(d);

        try list.append(gpa, .{
            .sequence = sequence,
            .state = state,
            .mutation_type = mutation_type,
            .target_display_id = target,
            .created_at = created_at,
            .failure_detail = failure_detail,
        });
    }
    if (rows.err) |err| return err;

    return list.toOwnedSlice(gpa);
}

/// One row of the `tk sync log <sequence>` detail view.
pub const DetailRow = struct {
    sequence: i64,
    state: []u8,
    mutation_type: []u8,
    target_display_id: []u8,
    item_class: []u8,
    payload_json: []u8,
    failure_detail: ?[]u8,
    created_at: []u8,
    state_changed_at: []u8,

    pub fn deinit(self: DetailRow, gpa: Allocator) void {
        gpa.free(self.state);
        gpa.free(self.mutation_type);
        gpa.free(self.target_display_id);
        gpa.free(self.item_class);
        gpa.free(self.payload_json);
        if (self.failure_detail) |d| gpa.free(d);
        gpa.free(self.created_at);
        gpa.free(self.state_changed_at);
    }
};

/// Look up one Mutation Log entry by sequence and return its full detail.
pub fn showMutation(
    conn: zqlite.Conn,
    gpa: Allocator,
    sequence: i64,
) LogError!DetailRow {
    const row = (try conn.row(
        \\select m.sequence, m.state, m.mutation_type, i.display_value,
        \\       m.item_class, m.payload_json, m.failure_json,
        \\       m.created_at, m.state_changed_at
        \\  from mutations m
        \\  join items i on i.id = m.item_id and i.item_class = m.item_class
        \\ where m.sequence = ?1
    , .{sequence})) orelse return error.MutationNotFound;
    defer row.deinit();

    const state = try gpa.dupe(u8, row.text(1));
    errdefer gpa.free(state);
    const mutation_type = try gpa.dupe(u8, row.text(2));
    errdefer gpa.free(mutation_type);
    const target = try gpa.dupe(u8, row.text(3));
    errdefer gpa.free(target);
    const item_class = try gpa.dupe(u8, row.text(4));
    errdefer gpa.free(item_class);
    const payload_json = try gpa.dupe(u8, row.text(5));
    errdefer gpa.free(payload_json);

    var failure_detail: ?[]u8 = null;
    if (row.nullableText(6)) |raw| {
        failure_detail = try decodeFailureDetail(gpa, raw);
    }
    errdefer if (failure_detail) |d| gpa.free(d);

    const created_at = try gpa.dupe(u8, row.text(7));
    errdefer gpa.free(created_at);
    const state_changed_at = try gpa.dupe(u8, row.text(8));

    return .{
        .sequence = row.int(0),
        .state = state,
        .mutation_type = mutation_type,
        .target_display_id = target,
        .item_class = item_class,
        .payload_json = payload_json,
        .failure_detail = failure_detail,
        .created_at = created_at,
        .state_changed_at = state_changed_at,
    };
}

/// Decode the `failure_json` text column (always `{"detail": "..."}` in v1,
/// per applyMutationOutcome) into the bare detail string. Returns a
/// freshly-allocated copy owned by `gpa`.
fn decodeFailureDetail(gpa: Allocator, raw: []const u8) LogError![]u8 {
    const Wrapper = struct { detail: []const u8 };
    const parsed = try std.json.parseFromSlice(Wrapper, gpa, raw, .{});
    defer parsed.deinit();
    return gpa.dupe(u8, parsed.value.detail);
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

fn openMemDb() !zqlite.Conn {
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    errdefer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    return conn;
}

fn seedFixture(conn: zqlite.Conn) !void {
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"P\",\"body\":\"\"}",
        .state = "pending",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 2,
        .mutation_type = "set_item_status",
        .item_id = "t1",
        .payload_json = "{\"status\":\"done\"}",
        .state = "failed",
        .failure_json = "{\"detail\":\"HTTP 422\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 3,
        .mutation_type = "add_dependency",
        .item_id = "t1",
        .payload_json = "{\"blocking_id\":\"t-other\"}",
        .state = "skipped",
        .failure_json = "{\"detail\":\"validation\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 4,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "applied",
    });
}

fn freeListRows(gpa: Allocator, rows: []ListRow) void {
    for (rows) |r| r.deinit(gpa);
    gpa.free(rows);
}

test "listMutations default: pending + failed + skipped (excludes applied)" {
    const gpa = std.testing.allocator;

    const conn = try openMemDb();
    defer conn.close();
    try seedFixture(conn);

    const rows = try listMutations(conn, gpa, .default);
    defer freeListRows(gpa, rows);

    try std.testing.expectEqual(@as(usize, 3), rows.len);
    try std.testing.expectEqual(@as(i64, 1), rows[0].sequence);
    try std.testing.expectEqualStrings("pending", rows[0].state);
    try std.testing.expectEqual(@as(i64, 2), rows[1].sequence);
    try std.testing.expectEqualStrings("failed", rows[1].state);
    try std.testing.expectEqual(@as(i64, 3), rows[2].sequence);
    try std.testing.expectEqualStrings("skipped", rows[2].state);
}

test "listMutations pending filter returns only pending rows" {
    const gpa = std.testing.allocator;

    const conn = try openMemDb();
    defer conn.close();
    try seedFixture(conn);

    const rows = try listMutations(conn, gpa, .pending);
    defer freeListRows(gpa, rows);

    try std.testing.expectEqual(@as(usize, 1), rows.len);
    try std.testing.expectEqualStrings("pending", rows[0].state);
    try std.testing.expectEqualStrings("update_ticket", rows[0].mutation_type);
    try std.testing.expectEqualStrings("gh-1", rows[0].target_display_id);
    try std.testing.expectEqual(@as(?[]u8, null), rows[0].failure_detail);
}

test "listMutations failed filter decodes failure_detail" {
    const gpa = std.testing.allocator;

    const conn = try openMemDb();
    defer conn.close();
    try seedFixture(conn);

    const rows = try listMutations(conn, gpa, .failed);
    defer freeListRows(gpa, rows);

    try std.testing.expectEqual(@as(usize, 1), rows.len);
    try std.testing.expectEqualStrings("failed", rows[0].state);
    const detail = rows[0].failure_detail orelse return error.ExpectedFailureDetail;
    try std.testing.expectEqualStrings("HTTP 422", detail);
}

test "listMutations skipped filter returns skipped rows" {
    const gpa = std.testing.allocator;

    const conn = try openMemDb();
    defer conn.close();
    try seedFixture(conn);

    const rows = try listMutations(conn, gpa, .skipped);
    defer freeListRows(gpa, rows);

    try std.testing.expectEqual(@as(usize, 1), rows.len);
    try std.testing.expectEqualStrings("skipped", rows[0].state);
}

test "listMutations returns empty slice when no Mutations match" {
    const gpa = std.testing.allocator;

    const conn = try openMemDb();
    defer conn.close();

    const rows = try listMutations(conn, gpa, .default);
    defer freeListRows(gpa, rows);
    try std.testing.expectEqual(@as(usize, 0), rows.len);
}

test "showMutation returns the row with decoded failure detail" {
    const gpa = std.testing.allocator;

    const conn = try openMemDb();
    defer conn.close();
    try seedFixture(conn);

    const detail = try showMutation(conn, gpa, 2);
    defer detail.deinit(gpa);

    try std.testing.expectEqual(@as(i64, 2), detail.sequence);
    try std.testing.expectEqualStrings("failed", detail.state);
    try std.testing.expectEqualStrings("set_item_status", detail.mutation_type);
    try std.testing.expectEqualStrings("gh-1", detail.target_display_id);
    try std.testing.expectEqualStrings("ticket", detail.item_class);
    try std.testing.expectEqualStrings("{\"status\":\"done\"}", detail.payload_json);
    try std.testing.expectEqualStrings("HTTP 422", detail.failure_detail orelse return error.ExpectedFailureDetail);
}

test "showMutation returns MutationNotFound for missing sequence" {
    const gpa = std.testing.allocator;

    const conn = try openMemDb();
    defer conn.close();
    try seedFixture(conn);

    try std.testing.expectError(error.MutationNotFound, showMutation(conn, gpa, 999));
}
