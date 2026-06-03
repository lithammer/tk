//! Per-Mutation Apply outcome — the typed result a Backend Adapter returns
//! from `apply_mutation` and the sync engine consumes when it persists
//! `mutations.state` and `mutations.failure_json`.
//!
//! Pure data: no SQLite, filesystem, Git, or subprocess dependencies.
//!
//! This is one of the two typed shapes ADR-0018 deferred until "real adapter
//! pressure" — that pressure is the sync engine (ADR-0003 Mutation outbox
//! replay), which needs to distinguish three levels of result:
//!
//! - environment failure (adapter unavailable: binary missing, spawn failed)
//!   — surfaces through the adapter method's `Result<_, ApplyError>` error
//!   arm, bubbles out of the engine, leaves the Mutation row `pending`;
//! - per-Mutation rejection (the backend ran but refused the write) —
//!   surfaces as [`ApplyOutcome::Rejected`], recorded to `failure_json`, stops
//!   the apply loop;
//! - acceptance — surfaces as [`ApplyOutcome::Accepted`], transitions the row
//!   to `applied` and advances the Sync Cursor.
//!
//! The acceptance/rejection split (this type) vs. environment failure (the
//! error arm) is the ADR-0009 sync failure taxonomy. This is NOT a
//! `Result`-in-disguise: a rejection is durable evidence the engine records,
//! not an error it bubbles — hence the named `Accepted`/`Rejected` variants
//! rather than `Ok`/`Err`.

/// The backend's verdict on applying one Mutation Log entry.
///
/// Environment failures are NOT modelled here — they arrive through the
/// adapter method's `Result` error arm (`ApplyError`). This enum only
/// distinguishes backend acceptance from backend rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Mutation accepted by the backend.
    Accepted(Receipt),
    /// Mutation rejected by the backend (non-zero exit, validation refusal).
    Rejected(Failure),
}

/// Adapter-supplied evidence that a Mutation succeeded.
///
/// Intentionally empty today — Promotion grows it with the
/// backend-assigned identifiers (issue number, Jira key) a successful
/// `promote_*` Mutation returns. Kept as a struct rather than a unit variant
/// so that growth is an additive field change, not a variant-shape churn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Receipt {}

/// Adapter-supplied evidence that a Mutation was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Human-readable failure detail captured from the adapter (typically the
    /// backend CLI's stderr). The engine persists this verbatim into the
    /// `{"detail":"…"}` wrapper stored in `mutations.failure_json`.
    pub detail: String,
}

impl ApplyOutcome {
    /// Convenience constructor for the empty-receipt acceptance case.
    #[must_use]
    pub fn accepted() -> Self {
        Self::Accepted(Receipt::default())
    }

    /// Convenience constructor for a rejection carrying `detail`.
    #[must_use]
    pub fn rejected(detail: impl Into<String>) -> Self {
        Self::Rejected(Failure {
            detail: detail.into(),
        })
    }
}
