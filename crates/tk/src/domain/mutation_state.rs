//! Mutation Log entry state.
//!
//! Every state is mirrored in the `mutations.state` CHECK constraint, so the
//! `text()` spelling is the storage contract, not just a rendering
//! convenience. [`MutationState`] carries the transition table every
//! `mutations.state` write obeys, so it is a domain value rather than a
//! pass-through display string.

use std::fmt;

/// Lifecycle state of one Mutation Log (outbox) entry. New Mutations are
/// appended as [`MutationState::Pending`]; the sync engine and the explicit
/// recovery commands transition them.
///
/// # Transition table
///
/// `crate::store::mutations::transition` is the only writer of
/// `mutations.state`, and it refuses every edge this table omits. The
/// `failure_json` column is bookkeeping of the edge, not of the caller: an
/// edge either clears the previous attempt's evidence, records fresh evidence,
/// or preserves what is already there.
///
/// | From | To | `failure_json` | Taken by |
/// | --- | --- | --- | --- |
/// | `pending`, `failed` | `applying` | cleared | durably marking a Backend creation in flight |
/// | `applying` | `applying` | recorded | a creation whose effect stayed indeterminate |
/// | `applying` | `pending` | cleared | Promotion Retry |
/// | `pending`, `failed`, `applying` | `failed` | recorded | a certified Backend rejection |
/// | `failed` | `skipped` | preserved | Sync Skip |
/// | `pending`, `failed` | `cancelled` | preserved | Promotion Cancellation |
/// | `applying` | `abandoned` | preserved | Promotion Cancellation |
/// | `pending`, `failed`, `applying` | `applied` | cleared | a persisted Backend effect |
///
/// `skipped`, `cancelled`, `abandoned`, and `applied` are terminal: nothing
/// leaves them. `applying` is the only self-edge, and it exists because an
/// indeterminate creation records why without resolving the doubt. Promotion
/// Cancellation withdraws from `applying` too, but into `abandoned` rather than
/// `cancelled`, because tk never observed what that creation did (ADR-0039).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MutationState {
    /// Queued and not yet attempted.
    Pending,
    /// Certified rejection with durable failure evidence.
    Failed,
    /// Backend creation began and no confirmed identity or no-effect verdict exists.
    Applying,
    /// Human-curated terminal omission after sync failed on the Mutation.
    Skipped,
    /// Terminally withdrawn by Promotion Cancellation, never attempted, so
    /// nothing it would have created exists on the Backend.
    Cancelled,
    /// Terminally withdrawn by Promotion Cancellation before any Backend
    /// identity was recorded, so a Backend object may exist that tk cannot
    /// address.
    Abandoned,
    /// Backend effect and any resulting identity were persisted.
    Applied,
}

impl MutationState {
    /// Every state, written out rather than derived for the same reason
    /// [`text`] is: a caller that has to reason over the whole set — checking
    /// a stored spelling, or walking the transition table above — reads this
    /// list instead of maintaining its own.
    ///
    /// [`text`]: MutationState::text
    pub const ALL: [Self; 7] = [
        Self::Pending,
        Self::Failed,
        Self::Applying,
        Self::Skipped,
        Self::Cancelled,
        Self::Abandoned,
        Self::Applied,
    ];

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
            Self::Cancelled => "cancelled",
            Self::Abandoned => "abandoned",
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
        assert_eq!(MutationState::Cancelled.text(), "cancelled");
        assert_eq!(MutationState::Abandoned.text(), "abandoned");
        assert_eq!(MutationState::Applied.text(), "applied");
    }

    #[test]
    fn display_writes_text() {
        assert_eq!(format!("{}", MutationState::Skipped), "skipped");
    }

    #[test]
    fn all_lists_every_variant_exactly_once() {
        // A missing or duplicated entry here means ALL has silently drifted
        // from the enum, defeating its purpose as the caller's whole-set view.
        let mut texts: Vec<&str> = MutationState::ALL.iter().map(|s| s.text()).collect();
        texts.sort_unstable();
        texts.dedup();
        assert_eq!(texts.len(), MutationState::ALL.len());
    }
}
