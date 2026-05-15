//! Typed V1 Mutation kind enum shared by the Mutation Log and Backend Adapters.

const std = @import("std");

/// All V1 Mutation kinds that the Mutation Log outbox may carry.
///
/// Tag names match the `mutation_type` SQL `check` constraint spellings exactly
/// so they can be round-tripped with `text`/`fromText` without a separate map.
pub const MutationType = enum {
    update_ticket,
    update_epic,
    set_item_status,
    add_ticket_to_epic,
    remove_ticket_from_epic,
    add_dependency,
    remove_dependency,
    add_external_blocker,
    resolve_external_blocker,
    promote_ticket,
    promote_epic,

    /// Return the SQL-compatible text spelling (matches the enum tag name).
    pub fn text(self: MutationType) []const u8 {
        return @tagName(self);
    }

    /// Parse from a SQL text column. Returns `null` for unknown values.
    pub fn fromText(s: []const u8) ?MutationType {
        return std.meta.stringToEnum(MutationType, s);
    }
};

test "MutationType: every value round-trips through text/fromText" {
    inline for (std.enums.values(MutationType)) |t| {
        try std.testing.expectEqual(t, MutationType.fromText(t.text()).?);
    }
    try std.testing.expectEqual(@as(?MutationType, null), MutationType.fromText("not_a_real_type"));
}
