//! Item Class distinguishes Tickets from Epics in the Repository Store.

/// The top-level item class stored in the Repository Store.
pub const ItemClass = enum {
    ticket,
    epic,

    /// SQLite storage and CLI rendering string.
    pub fn text(self: ItemClass) []const u8 {
        return switch (self) {
            .ticket => "ticket",
            .epic => "epic",
        };
    }
};
