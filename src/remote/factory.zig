//! Backend Adapter factory.
//!
//! `openConfigured` reads the singleton `remotes` row through
//! `store.sync.getRemote` and dispatches by `backend_kind`. In tk-17 only
//! the `fake` kind has an implementation (used by engine tests directly);
//! `github` and `jira` return `error.NotImplemented` so real adapter
//! implementations can land in their own slices.

const std = @import("std");
const Allocator = std.mem.Allocator;
const zqlite = @import("zqlite");

const Adapter = @import("adapter.zig").Adapter;
const store_sync = @import("../store/sync.zig");

/// Error set returned by `openConfigured`.
pub const OpenError = store_sync.GetRemoteError || error{
    /// A Remote row exists but the matching backend kind has no Adapter
    /// implementation yet (real `github` / `jira` adapters land in later
    /// slices).
    NotImplemented,
};

/// Look up the configured Remote and return an Adapter for it. Returns
/// `null` when no Remote is configured.
///
/// Caller owns nothing — the returned Adapter is a value type with vtable +
/// context pointers stored in the implementing module's caller-owned struct.
/// Until real adapters land this always returns `error.NotImplemented` for
/// known kinds.
pub fn openConfigured(
    conn: zqlite.Conn,
    gpa: Allocator,
) OpenError!?Adapter {
    const row = (try store_sync.getRemote(conn, gpa)) orelse return null;
    defer row.deinit(gpa);

    // No adapter kinds have real implementations in tk-17; the dispatch
    // by `row.backend_kind` lands in a later slice alongside the github /
    // jira modules.
    return error.NotImplemented;
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

const migrations = @import("../store/migrations.zig");
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

test "openConfigured: returns null when no Remote configured" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try std.testing.expectEqual(@as(?Adapter, null), try openConfigured(conn, gpa));
}

test "openConfigured: github returns NotImplemented in tk-17" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });

    try std.testing.expectError(error.NotImplemented, openConfigured(conn, gpa));
}

test "openConfigured: jira returns NotImplemented in tk-17" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "jira",
        .config_json = "{\"site\":\"x\",\"project\":\"P\"}",
    });

    try std.testing.expectError(error.NotImplemented, openConfigured(conn, gpa));
}
