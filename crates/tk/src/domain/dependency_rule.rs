//! What a Dependency edge means once both endpoints' Backend Intent is known
//! (ADR-0035).
//!
//! Two callers ask the same question of different graphs: `tk block` judges an
//! edge against the state the Repository Store holds now, and Promotion
//! preflight judges every edge against the state the whole operation will
//! produce. The rule lives in one place so those two answers cannot drift;
//! each caller renders a [`DependencyRejection`] in its own words, because one
//! interpolates the arguments the user typed and the other names both
//! endpoints and a remedy.

use super::backend_intent::BackendIntent;

/// What a Dependency edge means for the Mutation Log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyClassification {
    /// Both endpoints are bound to the same Backend, so the edge is backend
    /// intent and carries an `add_dependency` Mutation.
    BecomesBackendIntent,
    /// The Blocked Item carries no backend identity, so the edge is
    /// Repository Store current state and nothing more — whatever the
    /// Blocking Item is.
    StaysLocal,
    /// The resulting graph would be invalid; no edge may exist between these
    /// endpoints.
    Rejected(DependencyRejection),
}

/// Why a Dependency edge would make the resulting graph invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyRejection {
    /// A backend-bound Blocked Item waiting on a Local Blocking Item: its
    /// Dependency Mutation would name a reference the Backend cannot address.
    BackendBlockedLocalBlocking,
    /// Endpoints bound to two different Backends cannot share a Dependency
    /// Mutation.
    BackendKindMismatch,
}

/// Classify one Dependency edge from its endpoints' Backend Intent.
///
/// Pending Promotion counts as bound to the Backend its Promotion targets:
/// the edge's Mutation is ordered behind that Promotion and resolves against
/// the identity its receipt assigns (ADR-0036).
#[must_use]
pub fn classify(blocked: &BackendIntent, blocking: &BackendIntent) -> DependencyClassification {
    match (blocked.backend_kind(), blocking.backend_kind()) {
        (None, _) => DependencyClassification::StaysLocal,
        (Some(_), None) => {
            DependencyClassification::Rejected(DependencyRejection::BackendBlockedLocalBlocking)
        }
        (Some(blocked_kind), Some(blocking_kind)) if blocked_kind == blocking_kind => {
            DependencyClassification::BecomesBackendIntent
        }
        (Some(_), Some(_)) => {
            DependencyClassification::Rejected(DependencyRejection::BackendKindMismatch)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(kind: &str) -> BackendIntent {
        BackendIntent::Backend {
            backend_kind: kind.to_owned(),
        }
    }

    fn pending(kind: &str) -> BackendIntent {
        BackendIntent::PendingPromotion {
            backend_kind: kind.to_owned(),
        }
    }

    #[test]
    fn a_local_blocked_item_keeps_the_edge_local() {
        assert_eq!(
            classify(&BackendIntent::Local, &BackendIntent::Local),
            DependencyClassification::StaysLocal
        );
        assert_eq!(
            classify(&BackendIntent::Local, &backend("github")),
            DependencyClassification::StaysLocal
        );
    }

    #[test]
    fn a_backend_blocked_item_may_not_wait_on_a_local_blocking_item() {
        assert_eq!(
            classify(&backend("github"), &BackendIntent::Local),
            DependencyClassification::Rejected(DependencyRejection::BackendBlockedLocalBlocking)
        );
    }

    #[test]
    fn two_backends_may_not_share_an_edge() {
        assert_eq!(
            classify(&backend("github"), &backend("jira")),
            DependencyClassification::Rejected(DependencyRejection::BackendKindMismatch)
        );
    }

    #[test]
    fn one_backend_on_both_ends_becomes_intent() {
        assert_eq!(
            classify(&backend("github"), &backend("github")),
            DependencyClassification::BecomesBackendIntent
        );
    }

    #[test]
    fn pending_promotion_counts_as_bound_to_its_target_backend() {
        assert_eq!(
            classify(&pending("github"), &backend("github")),
            DependencyClassification::BecomesBackendIntent
        );
        assert_eq!(
            classify(&pending("github"), &BackendIntent::Local),
            DependencyClassification::Rejected(DependencyRejection::BackendBlockedLocalBlocking)
        );
        assert_eq!(
            classify(&pending("github"), &pending("jira")),
            DependencyClassification::Rejected(DependencyRejection::BackendKindMismatch)
        );
    }
}
