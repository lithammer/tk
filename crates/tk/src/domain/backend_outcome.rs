//! Typed Backend Adapter write outcomes.
//!
//! Edit environment failures use the Adapter method's `Result` error arm.
//! Creation always returns a value so its Adapter can classify pre-spawn
//! process errors separately from completed invocations whose effect may be
//! ambiguous. Backend verdicts stay typed so the sync engine can persist their
//! certainty to the Mutation Log.

use serde::{Deserialize, Serialize};

use super::backend_operation::BackendItemIdentity;

/// Backend verdict for editing an existing object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendEditOutcome {
    /// The Backend acknowledged the edit.
    Acknowledged,
    /// The Backend rejected the edit.
    Rejected(Failure),
}

/// Backend verdict for creating a new object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCreateOutcome {
    /// The Backend created the object and returned its canonical identity.
    Created(BackendItemIdentity),
    /// The Adapter has evidence that creation had no effect.
    Rejected(Failure),
    /// The Adapter cannot determine whether the Backend created the object.
    Indeterminate(Failure),
}

/// Backend Adapter classification of a [`Failure`] (ADR-0016 / CONTEXT.md
/// Adapter Failure).
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
    /// Lowercase label stored in `failure_json` and rendered by `tk sync log`.
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

/// Adapter-supplied failure evidence persisted in `mutations.failure_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// Human-readable diagnostic captured from the Adapter.
    pub detail: String,
    /// Adapter classification used by Sync Log and recovery policy.
    #[serde(default)]
    pub class: FailureClass,
    /// Adapter-provided retry delay, when reliable evidence exists.
    #[serde(default)]
    pub retry_after_s: Option<i64>,
}

impl Failure {
    /// Construct an unclassified failure without a retry hint.
    #[must_use]
    pub fn unknown(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            class: FailureClass::Unknown,
            retry_after_s: None,
        }
    }
}

impl BackendEditOutcome {
    /// Construct an unclassified Backend rejection.
    #[must_use]
    pub fn rejected(detail: impl Into<String>) -> Self {
        Self::Rejected(Failure::unknown(detail))
    }
}

impl BackendCreateOutcome {
    /// Construct an unclassified certified-no-effect rejection.
    #[must_use]
    pub fn rejected(detail: impl Into<String>) -> Self {
        Self::Rejected(Failure::unknown(detail))
    }

    /// Construct an unclassified result with unknown creation effect.
    #[must_use]
    pub fn indeterminate(detail: impl Into<String>) -> Self {
        Self::Indeterminate(Failure::unknown(detail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_serializes_detail_first() {
        let failure = Failure {
            detail: "boom".into(),
            class: FailureClass::Auth,
            retry_after_s: None,
        };
        assert_eq!(
            serde_json::to_string(&failure).unwrap(),
            r#"{"detail":"boom","class":"auth","retry_after_s":null}"#
        );
    }

    #[test]
    fn legacy_detail_only_row_decodes_with_unknown_class() {
        let failure: Failure = serde_json::from_str(r#"{"detail":"old"}"#).unwrap();
        assert_eq!(failure.class, FailureClass::Unknown);
        assert_eq!(failure.retry_after_s, None);
    }

    #[test]
    fn unknown_class_string_decodes_to_unknown() {
        let failure: Failure = serde_json::from_str(r#"{"detail":"x","class":"teapot"}"#).unwrap();
        assert_eq!(failure.class, FailureClass::Unknown);
    }

    #[test]
    fn extra_fields_are_ignored() {
        let failure: Failure =
            serde_json::from_str(r#"{"detail":"x","class":"auth","future":1}"#).unwrap();
        assert_eq!(failure.class, FailureClass::Auth);
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
            assert_eq!(class.text(), text);
            assert_eq!(
                serde_json::to_string(&class).unwrap(),
                format!(r#""{text}""#)
            );
            assert_eq!(
                serde_json::from_str::<FailureClass>(&format!(r#""{text}""#)).unwrap(),
                class
            );
        }
    }
}
