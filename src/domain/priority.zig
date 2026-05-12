//! Priority is a local-only Ticket ranking.

/// Local-only Ticket ranking. Lower numbers sort before higher numbers.
pub const Priority = enum {
    P0,
    P1,
    P2,
    P3,
    P4,

    /// Default Priority for newly-created local Tickets.
    pub const default: Priority = .P2;

    /// SQLite storage and CLI rendering string.
    pub fn text(self: Priority) []const u8 {
        return switch (self) {
            .P0 => "P0",
            .P1 => "P1",
            .P2 => "P2",
            .P3 => "P3",
            .P4 => "P4",
        };
    }
};
