//! Mutation Log outbox helpers.
//!
//! `appendMutation` inserts one row into the `mutations` table. It must be
//! called inside an existing `begin immediate` transaction; it never starts
//! or commits a transaction itself. The sequence is allocated from
//! `sequences.mutation_seq` via the same `update … returning` pattern used
//! by the other sequence counters in the Repository Store.

const std = @import("std");
const zqlite = @import("zqlite");
const migrations = @import("migrations.zig");
const sequences = @import("sequences.zig");
const MutationType = @import("../domain/mutation_type.zig").MutationType;
const ItemClass = @import("../domain/item_class.zig").ItemClass;

/// Re-export of the typed payload union for callers already importing this
/// module. The canonical definition lives in `src/domain/mutation_payload.zig`
/// — pure data shared by `store/`, `remote/`, and the future `sync/` engine.
pub const MutationPayload = @import("../domain/mutation_payload.zig").MutationPayload;

/// Error set for `appendMutation`.
pub const AppendError = migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Append one Mutation row to the `mutations` table.
///
/// Must be called inside an active `begin immediate` transaction; commits and
/// rollbacks are the caller's responsibility. Allocates a `mutation_seq`
/// sequence value, serializes `payload` to JSON, and inserts the row with
/// state `pending`.
pub fn appendMutation(
    conn: zqlite.Conn,
    gpa: std.mem.Allocator,
    mutation_type: MutationType,
    item_id: []const u8,
    item_class: ItemClass,
    payload: MutationPayload,
    now: []const u8,
) AppendError!void {
    const seq = try sequences.next(conn, "mutation_seq");

    const payload_json = switch (payload) {
        inline else => |v| try std.json.Stringify.valueAlloc(gpa, v, .{}),
    };
    defer gpa.free(payload_json);

    try conn.exec(
        \\insert into mutations(
        \\  sequence, mutation_type, item_id, item_class, payload_json,
        \\  state, failure_json, created_at, state_changed_at
        \\) values (?1, ?2, ?3, ?4, ?5, 'pending', null, ?6, ?6)
    , .{
        seq,
        mutation_type.text(),
        item_id,
        item_class.text(),
        payload_json,
        now,
    });
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

test "appendMutation: update_ticket inserts pending row with correct payload" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Original",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    try conn.execNoArgs("begin immediate");
    try appendMutation(conn, gpa, .update_ticket, "t1", .ticket, .{
        .update_title_body = .{ .title = "New title", .body = "New body" },
    }, "2026-05-09T00:00:00.000Z");
    try conn.commit();

    const row = (try conn.row(
        \\select sequence, mutation_type, item_id, item_class, payload_json, state, failure_json
        \\  from mutations
        \\ where sequence = 1
    , .{})) orelse return error.ExpectedRow;
    defer row.deinit();

    try std.testing.expectEqual(@as(i64, 1), row.int(0));
    try std.testing.expectEqualStrings("update_ticket", row.text(1));
    try std.testing.expectEqualStrings("t1", row.text(2));
    try std.testing.expectEqualStrings("ticket", row.text(3));
    try std.testing.expectEqualStrings(
        "{\"title\":\"New title\",\"body\":\"New body\"}",
        row.text(4),
    );
    try std.testing.expectEqualStrings("pending", row.text(5));
    try std.testing.expectEqual(@as(?[]const u8, null), row.nullableText(6));
}

test "appendMutation: update_epic inserts pending row" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "tk-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Original Epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "10",
        .created_seq = 1,
    });

    try conn.execNoArgs("begin immediate");
    try appendMutation(conn, gpa, .update_epic, "e1", .epic, .{
        .update_title_body = .{ .title = "Updated Epic", .body = "" },
    }, "2026-05-09T00:00:00.000Z");
    try conn.commit();

    const row = (try conn.row(
        "select mutation_type, item_class, payload_json from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer row.deinit();

    try std.testing.expectEqualStrings("update_epic", row.text(0));
    try std.testing.expectEqualStrings("epic", row.text(1));
    try std.testing.expect(std.mem.indexOf(u8, row.text(2), "Updated Epic") != null);
}

test "appendMutation: add_ticket_to_epic inserts pending row with epic_id" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    try conn.execNoArgs("begin immediate");
    try appendMutation(conn, gpa, .add_ticket_to_epic, "t1", .ticket, .{
        .epic_ref = .{ .epic_id = "epic-internal-id" },
    }, "2026-05-09T00:00:00.000Z");
    try conn.commit();

    const row = (try conn.row(
        "select mutation_type, payload_json from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer row.deinit();

    try std.testing.expectEqualStrings("add_ticket_to_epic", row.text(0));
    try std.testing.expectEqualStrings("{\"epic_id\":\"epic-internal-id\"}", row.text(1));
}

test "appendMutation: remove_ticket_from_epic inserts pending row with epic_id" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    try conn.execNoArgs("begin immediate");
    try appendMutation(conn, gpa, .remove_ticket_from_epic, "t1", .ticket, .{
        .epic_ref = .{ .epic_id = "old-epic-id" },
    }, "2026-05-09T00:00:00.000Z");
    try conn.commit();

    const row = (try conn.row(
        "select mutation_type, payload_json from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer row.deinit();

    try std.testing.expectEqualStrings("remove_ticket_from_epic", row.text(0));
    try std.testing.expectEqualStrings("{\"epic_id\":\"old-epic-id\"}", row.text(1));
}

test "appendMutation: set_item_status inserts status-only payload" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    try conn.execNoArgs("begin immediate");
    try appendMutation(conn, gpa, .set_item_status, "t1", .ticket, .{
        .item_status = .{ .status = "done" },
    }, "2026-05-09T00:00:00.000Z");
    try conn.commit();

    const row = (try conn.row(
        "select mutation_type, item_class, payload_json from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer row.deinit();

    try std.testing.expectEqualStrings("set_item_status", row.text(0));
    try std.testing.expectEqualStrings("ticket", row.text(1));
    try std.testing.expectEqualStrings("{\"status\":\"done\"}", row.text(2));
}

test "appendMutation: sequence is monotonically increasing across calls" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    try conn.execNoArgs("begin immediate");
    try appendMutation(conn, gpa, .update_ticket, "t1", .ticket, .{
        .update_title_body = .{ .title = "A", .body = "" },
    }, "2026-05-09T00:00:00.000Z");
    try appendMutation(conn, gpa, .update_ticket, "t1", .ticket, .{
        .update_title_body = .{ .title = "B", .body = "" },
    }, "2026-05-09T00:00:00.000Z");
    try appendMutation(conn, gpa, .update_ticket, "t1", .ticket, .{
        .update_title_body = .{ .title = "C", .body = "" },
    }, "2026-05-09T00:00:00.000Z");
    try conn.commit();

    const count = (try conn.row("select count(*) from mutations", .{})) orelse return error.ExpectedRow;
    defer count.deinit();
    try std.testing.expectEqual(@as(i64, 3), count.int(0));

    var rows = try conn.rows("select sequence from mutations order by sequence asc", .{});
    defer rows.deinit();
    var expected_seq: i64 = 1;
    while (rows.next()) |r| {
        try std.testing.expectEqual(expected_seq, r.int(0));
        expected_seq += 1;
    }
    if (rows.err) |err| return err;
}

test "appendMutation: advances sequences.mutation_seq value" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    const initial_seq = try migrations.queryOptionalInt(conn, "select value from sequences where name = 'mutation_seq'");
    try std.testing.expectEqual(@as(?i64, 0), initial_seq);

    try conn.execNoArgs("begin immediate");
    try appendMutation(conn, gpa, .update_ticket, "t1", .ticket, .{
        .update_title_body = .{ .title = "X", .body = "" },
    }, "2026-05-09T00:00:00.000Z");
    try appendMutation(conn, gpa, .update_ticket, "t1", .ticket, .{
        .update_title_body = .{ .title = "Y", .body = "" },
    }, "2026-05-09T00:00:00.000Z");
    try conn.commit();

    const final_seq = try migrations.queryOptionalInt(conn, "select value from sequences where name = 'mutation_seq'");
    try std.testing.expectEqual(@as(?i64, 2), final_seq);
}
