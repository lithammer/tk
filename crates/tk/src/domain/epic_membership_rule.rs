//! What Epic Membership means once both Items' Backend Binding is known
//! (ADR-0035).
//!
//! Promotion and Re-Adopt ask this of different resulting graphs. The rule
//! lives here so both capture membership only when the Ticket and Epic share
//! one Backend, while mixed-Origin membership remains Repository Store state.

use super::backend_binding::BackendBinding;

/// What an Epic Membership means for the Mutation Log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpicMembershipClassification {
    /// Both Items share one Backend, so membership carries an
    /// `add_ticket_to_epic` Mutation.
    BecomesBackendIntent,
    /// The Items do not share one Backend, so membership remains local current
    /// state.
    StaysLocal,
}

/// Classify Epic Membership from the Ticket and Epic's resulting Bindings.
#[must_use]
pub fn classify(ticket: &BackendBinding, epic: &BackendBinding) -> EpicMembershipClassification {
    match (ticket.backend_kind(), epic.backend_kind()) {
        (Some(ticket_kind), Some(epic_kind)) if ticket_kind == epic_kind => {
            EpicMembershipClassification::BecomesBackendIntent
        }
        _ => EpicMembershipClassification::StaysLocal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(kind: &str) -> BackendBinding {
        BackendBinding::Backend {
            backend_kind: kind.to_owned(),
        }
    }

    #[test]
    fn only_one_backend_on_both_items_becomes_intent() {
        assert_eq!(
            classify(&backend("github"), &backend("github")),
            EpicMembershipClassification::BecomesBackendIntent
        );
        assert_eq!(
            classify(&BackendBinding::Local, &backend("github")),
            EpicMembershipClassification::StaysLocal
        );
        assert_eq!(
            classify(&backend("github"), &BackendBinding::Local),
            EpicMembershipClassification::StaysLocal
        );
        assert_eq!(
            classify(&backend("github"), &backend("jira")),
            EpicMembershipClassification::StaysLocal
        );
    }
}
