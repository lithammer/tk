//! Ticket Kind classifies Tickets as tasks or bugs.

/// The category of a Ticket.
pub const TicketKind = enum {
    task,
    bug,

    /// Default Ticket Kind for `tk add` until `--bug` is implemented.
    pub const default: TicketKind = .task;

    /// SQLite storage and CLI rendering string.
    pub fn text(self: TicketKind) []const u8 {
        return switch (self) {
            .task => "task",
            .bug => "bug",
        };
    }
};
