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

use serde::{Deserialize, Serialize};

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

/// Backend Adapter classification of a [`Failure`] (ADR-0016 / CONTEXT.md
/// Adapter Failure). The snake_case serde spelling is the on-disk contract
/// inside `mutations.failure_json`; the Backend Adapter assigns the class,
/// while retry and recoverability policy belong to the sync engine and
/// recovery workflows.
///
/// `Unknown` is both the conservative default for an unclassified failure and
/// the `#[serde(other)]` catch-all, so a class a newer binary writes still
/// decodes in an older one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    RateLimited,
    Validation,
    SyncConflict,
    Auth,
    Transient,
    #[default]
    #[serde(other)]
    Unknown,
}

impl FailureClass {
    /// Lowercase label `tk sync log` renders; identical to the snake_case serde
    /// spelling stored in `failure_json` (pinned by a test).
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::Validation => "validation",
            Self::SyncConflict => "sync_conflict",
            Self::Auth => "auth",
            Self::Transient => "transient",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for FailureClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

/// Adapter-supplied evidence that a Mutation was rejected.
///
/// This *is* the persisted `mutations.failure_json` shape — the adapter's
/// return value and the stored record are one type (ADR-0016 amendment,
/// following the `MutationPayload` precedent). `detail` serializes first so the
/// common `unknown` row has a predictable byte order; `class` and
/// `retry_after_s` carry `#[serde(default)]` so a legacy `{"detail":"…"}` row
/// still decodes (class → `unknown`, retry_after_s → `None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// Human-readable failure detail captured from the adapter (typically the
    /// backend CLI's stderr).
    pub detail: String,
    /// Adapter classification driving Sync Log rendering and, later, recovery
    /// policy.
    #[serde(default)]
    pub class: FailureClass,
    /// Seconds the adapter advises waiting before retry; `None` in v1 — no
    /// backend CLI surfaces a reliable reset time.
    #[serde(default)]
    pub retry_after_s: Option<i64>,
}

impl ApplyOutcome {
    /// Convenience constructor for the empty-receipt acceptance case.
    #[must_use]
    pub fn accepted() -> Self {
        Self::Accepted(Receipt::default())
    }

    /// Convenience constructor for a rejection carrying `detail`, classified
    /// [`FailureClass::Unknown`] with no retry hint. Adapters that classify
    /// build [`Failure`] directly.
    #[must_use]
    pub fn rejected(detail: impl Into<String>) -> Self {
        Self::Rejected(Failure {
            detail: detail.into(),
            class: FailureClass::Unknown,
            retry_after_s: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_serializes_detail_first() {
        // ADR-0016: detail-first byte order; class + retry_after_s follow.
        let f = Failure {
            detail: "boom".into(),
            class: FailureClass::Auth,
            retry_after_s: None,
        };
        assert_eq!(
            serde_json::to_string(&f).unwrap(),
            r#"{"detail":"boom","class":"auth","retry_after_s":null}"#
        );
    }

    #[test]
    fn legacy_detail_only_row_decodes_with_unknown_class() {
        // A pre-graduation `{"detail":"…"}` row must still parse (ADR-0016).
        let f: Failure = serde_json::from_str(r#"{"detail":"old"}"#).unwrap();
        assert_eq!(f.detail, "old");
        assert_eq!(f.class, FailureClass::Unknown);
        assert_eq!(f.retry_after_s, None);
    }

    #[test]
    fn unknown_class_string_decodes_to_unknown() {
        // #[serde(other)] forward-compat: a class this build does not know
        // decodes to Unknown instead of erroring.
        let f: Failure = serde_json::from_str(r#"{"detail":"x","class":"teapot"}"#).unwrap();
        assert_eq!(f.class, FailureClass::Unknown);
    }

    #[test]
    fn extra_fields_are_ignored() {
        // No deny_unknown_fields: an older binary reading a newer row's extra
        // column must not refuse the row.
        let f: Failure =
            serde_json::from_str(r#"{"detail":"x","class":"auth","future":1}"#).unwrap();
        assert_eq!(f.class, FailureClass::Auth);
    }

    #[test]
    fn every_class_round_trips_through_its_snake_case_spelling() {
        for (class, text) in [
            (FailureClass::RateLimited, "rate_limited"),
            (FailureClass::Validation, "validation"),
            (FailureClass::SyncConflict, "sync_conflict"),
            (FailureClass::Auth, "auth"),
            (FailureClass::Transient, "transient"),
            (FailureClass::Unknown, "unknown"),
        ] {
            // text() and the serde spelling must not drift.
            assert_eq!(class.text(), text);
            let json = serde_json::to_value(class).unwrap();
            assert_eq!(json, serde_json::Value::String(text.to_string()));
            assert_eq!(serde_json::from_value::<FailureClass>(json).unwrap(), class);
        }
    }
}
