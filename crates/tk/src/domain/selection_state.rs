//! Selection State is a local, Ticket-only intake/selection policy.
//!
//! Selection State decides whether an open Ticket is *selectable now*,
//! separately from Item Status (lifecycle) and Priority (ranking). The three
//! values are mirrored verbatim in the V1 `items.selection_state` CHECK
//! constraint, so the SQL spelling returned by [`SelectionState::text`] is the
//! storage contract — not just a rendering convenience (ADR-0027).
//!
//! Epics stay outside this field: Selection State is `NULL` for Epics in the
//! Repository Store, modelled here as `Option<SelectionState>` at the read
//! boundary.

use std::fmt;

/// Local-only, Ticket-only selection policy (ADR-0027).
///
/// `SelectionState::Accepted` is the default for normal `tk add` and the value
/// newly imported Backend Tickets take: real work that can become ready for
/// `tk next`. `Triage` is captured-but-unaccepted work that needs a human
/// decision (and carries no Priority); `Parked` is accepted work intentionally
/// held out of automatic selection. Triage and parked Tickets are excluded
/// from `tk next` and do not contribute Effective Priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SelectionState {
    Triage,
    #[default]
    Accepted,
    Parked,
}

impl SelectionState {
    /// SQLite storage and CLI rendering string. Matches the
    /// `items.selection_state` CHECK constraint exactly.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::Accepted => "accepted",
            Self::Parked => "parked",
        }
    }
}

impl fmt::Display for SelectionState {
    /// Single-sources the unstyled representation on [`SelectionState::text`];
    /// styled render sites still wrap `text()` through the Styler.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_writes_text() {
        assert_eq!(format!("{}", SelectionState::Parked), "parked");
    }

    #[test]
    fn default_is_accepted() {
        // Normal `tk add` and Backend Pull both land on accepted; the default
        // must not drift to a non-selectable state.
        assert_eq!(SelectionState::default(), SelectionState::Accepted);
    }

    #[test]
    fn text_matches_check_constraint_spellings() {
        // These three spellings are the `items.selection_state` CHECK contract.
        assert_eq!(SelectionState::Triage.text(), "triage");
        assert_eq!(SelectionState::Accepted.text(), "accepted");
        assert_eq!(SelectionState::Parked.text(), "parked");
    }
}
