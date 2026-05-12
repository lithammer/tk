//! Item Status for Tickets and Epics.

/// Lifecycle state shared by Tickets and Epics.
pub const ItemStatus = enum {
    open,
    active,
    done,

    /// Default Item Status for newly-created local work.
    pub const default: ItemStatus = .open;

    /// SQLite storage and CLI rendering string.
    pub fn text(self: ItemStatus) []const u8 {
        return switch (self) {
            .open => "open",
            .active => "active",
            .done => "done",
        };
    }
};
