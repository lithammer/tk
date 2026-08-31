//! Work State: the local axis ADR-0043 splits out of `items.status`.
//!
//! A **Local Field** (CONTEXT.md) covering Tickets and Epics alike — never
//! applied to a Backend, never recorded as a Mutation. Its counterpart is
//! the Backend-shared [`crate::domain::lifecycle`]; ADR-0043 records how
//! Item Status derives from the pair.

/// Whether someone is working a Ticket or Epic right now.
///
/// `WorkState::Idle` is the default: a newly created or newly imported Item
/// is not yet being worked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WorkState {
    #[default]
    Idle,
    Active,
}

impl WorkState {
    /// Storage spelling. ADR-0043 pins `items.work_state` to these two values.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Active => "active",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_idle() {
        // A newly created or newly imported Item is not being worked. If this
        // drifted to `Active`, ADR-0043's derivation would report every new
        // Item as Item Status `active`.
        assert_eq!(WorkState::default(), WorkState::Idle);
    }

    #[test]
    fn text_matches_the_storage_spellings() {
        // These spellings and migration 011's `items.work_state` CHECK must
        // change together in one commit, or every row fails to decode.
        assert_eq!(WorkState::Idle.text(), "idle");
        assert_eq!(WorkState::Active.text(), "active");
    }
}
