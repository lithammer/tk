//! Mutation Log entry state.
//!
//! The four states are mirrored in the V1 `mutations.state` CHECK constraint
//! (`'pending'`, `'failed'`, `'skipped'`, `'applied'`); the `text()` spelling is
//! the storage contract, not just a rendering convenience. The state drives the
//! outbox transitions — Apply moves `pending`/`failed` → `applied`/`failed`,
//! Mark-skipped moves `failed` → `skipped` — so it is a domain value, not a
//! pass-through display string.

use std::fmt;

/// Lifecycle state of one Mutation Log (outbox) entry. New Mutations are
/// appended as [`MutationState::Pending`]; the sync engine transitions them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MutationState {
    Pending,
    Failed,
    Skipped,
    Applied,
}

impl MutationState {
    /// SQLite storage and CLI rendering string. Matches the `mutations.state`
    /// CHECK constraint exactly. Written out explicitly rather than derived from
    /// the variant names so renaming a variant cannot silently break the SQL
    /// contract.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Applied => "applied",
        }
    }
}

impl fmt::Display for MutationState {
    /// Single-sources the rendered spelling on [`MutationState::text`] so
    /// `tk sync log` output and the SQL spelling never diverge.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_matches_the_check_constrained_spellings() {
        // Pins the storage spellings against the `mutations.state` CHECK
        // constraint; drift here is a silent store-contract break.
        assert_eq!(MutationState::Pending.text(), "pending");
        assert_eq!(MutationState::Failed.text(), "failed");
        assert_eq!(MutationState::Skipped.text(), "skipped");
        assert_eq!(MutationState::Applied.text(), "applied");
    }

    #[test]
    fn display_writes_text() {
        assert_eq!(format!("{}", MutationState::Skipped), "skipped");
    }
}
