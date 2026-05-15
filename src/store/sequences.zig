//! Monotonic sequence-counter helpers shared by the Repository Store.
//!
//! The `sequences` table holds three named counters (`item_created_seq`,
//! `display_seq`, `mutation_seq`) seeded at 0 by migration 1. Allocation is
//! update-only inside the caller's open write transaction; a missing row is
//! Repository Store corruption rather than a recoverable condition.
//!
//! This module lives below `repository.zig` and `mutations.zig` so both can
//! allocate sequence values without depending on each other.

const zqlite = @import("zqlite");
const migrations = @import("migrations.zig");

/// Increment the named counter inside the caller's transaction and return the
/// new value. Must run inside an active `begin immediate`.
pub fn next(conn: zqlite.Conn, name: []const u8) migrations.QueryError!i64 {
    if (try conn.row("update sequences set value = value + 1 where name = ?1 returning value", .{name})) |r| {
        defer r.deinit();
        return r.int(0);
    }
    return error.Notfound;
}
