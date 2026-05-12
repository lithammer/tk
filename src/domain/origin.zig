//! Origin describes whether an item is local or backend-backed.

/// Source of authority for a Ticket or Epic.
pub const Origin = enum {
    local,
    backend,

    /// SQLite storage and CLI rendering string.
    pub fn text(self: Origin) []const u8 {
        return switch (self) {
            .local => "local",
            .backend => "backend",
        };
    }
};
