//! Mutation Log entry state.
//!
//! The five states are mirrored in the `mutations.state` CHECK constraint
//! (`'pending'`, `'failed'`, `'applying'`, `'skipped'`, `'applied'`); the `text()` spelling is
//! the storage contract, not just a rendering convenience. The state drives the
//! outbox transitions — edits move `pending`/`failed` to `applied`/`failed`,
//! creation first moves to `applying`, and Mark-skipped moves `failed` to
//! `skipped` — so it is a domain value, not a pass-through display string.

use std::fmt;

/// Lifecycle state of one Mutation Log (outbox) entry. New Mutations are
/// appended as [`MutationState::Pending`]; the sync engine transitions them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MutationState {
    /// Queued and not yet attempted.
    Pending,
    /// Certified rejection with durable failure evidence.
    Failed,
    /// Backend creation began and no confirmed identity or no-effect verdict exists.
    Applying,
    /// Human-curated terminal omission.
    Skipped,
    /// Backend effect and any resulting identity were persisted.
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
            Self::Applying => "applying",
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
        assert_eq!(MutationState::Applying.text(), "applying");
        assert_eq!(MutationState::Skipped.text(), "skipped");
        assert_eq!(MutationState::Applied.text(), "applied");
    }

    #[test]
    fn display_writes_text() {
        assert_eq!(format!("{}", MutationState::Skipped), "skipped");
    }
}
