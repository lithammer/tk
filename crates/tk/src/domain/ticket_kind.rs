//! Ticket Kind classifies Tickets as tasks or bugs.
//!
//! The two-variant set is mirrored in
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
    /// SQLite storage spelling and the value the `items.ticket_kind` CHECK
    /// constraint accepts; also the lowercase form `tk list` renders.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Bug => "bug",
        }
    }

    /// Capitalized noun for the `tk show` facet bar, mirroring
    /// [`ItemClass::label`](crate::domain::item_class::ItemClass::label) so the
    /// Ticket Kind and Item Class read in the same register (`Task`/`Bug`
    /// alongside `Epic`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Task => "Task",
            Self::Bug => "Bug",
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

    #[test]
    fn label_is_capitalized_and_distinct_from_storage_text() {
        // The facet bar reads `Task`/`Bug`; the SQL CHECK column stays
        // lowercase. Guards against collapsing the two back together.
        assert_eq!(TicketKind::Task.label(), "Task");
        assert_eq!(TicketKind::Bug.label(), "Bug");
        assert_eq!(TicketKind::Bug.text(), "bug");
    }
}
