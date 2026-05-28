//! Ticket Kind classifies Tickets as tasks or bugs.
//!
//! Ported from `src/domain/ticket_kind.zig`. The two-variant set is mirrored in
//! the V1 `items.ticket_kind` CHECK constraint; the `text()` spelling is the
//! storage contract.

use std::fmt;

/// The category of a Ticket. `TicketKind::Task` is the default for `tk add`
/// until `--bug` is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TicketKind {
    #[default]
    Task,
    Bug,
}

impl TicketKind {
    /// SQLite storage and CLI rendering string.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Bug => "bug",
        }
    }
}

impl fmt::Display for TicketKind {
    /// Single-sources the unstyled representation on [`TicketKind::text`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_writes_text() {
        assert_eq!(format!("{}", TicketKind::Bug), "bug");
    }

    #[test]
    fn default_is_task() {
        assert_eq!(TicketKind::default(), TicketKind::Task);
    }
}
