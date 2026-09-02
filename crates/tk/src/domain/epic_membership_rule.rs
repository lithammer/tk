//! What Epic Membership means once both Items' Backend Binding is known
//! (ADR-0035).
//!
//! Promotion and Re-Adopt ask this of different resulting graphs. The rule
//! lives here so both capture membership only when the Ticket and Epic share
//! one Backend, while mixed-Origin membership remains Repository Store state.

use super::backend_binding::BackendBinding;

/// What Epic Membership means for the Mutation Log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Both Items share one Backend, so membership carries an
    /// `add_ticket_to_epic` Mutation.
    BecomesBackendIntent,
    /// The Items do not share one Backend, so membership stays in the
    /// Repository Store only.
    StaysLocal,
}

/// Classify Epic Membership from the Ticket and Epic's resulting Bindings.
#[must_use]
pub fn classify(ticket: &BackendBinding, epic: &BackendBinding) -> Classification {
    match (ticket.backend_kind(), epic.backend_kind()) {
        (Some(ticket_kind), Some(epic_kind)) if ticket_kind == epic_kind => {
            Classification::BecomesBackendIntent
        }
        _ => Classification::StaysLocal,
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
    fn matching_backend_bindings_become_intent() {
        assert_eq!(
            classify(&backend("github"), &backend("github")),
            Classification::BecomesBackendIntent
        );
        assert_eq!(
            classify(&BackendBinding::Local, &backend("github")),
            Classification::StaysLocal
        );
        assert_eq!(
            classify(&backend("github"), &BackendBinding::Local),
            Classification::StaysLocal
        );
        assert_eq!(
            classify(&backend("github"), &backend("jira")),
            Classification::StaysLocal
        );
    }
}
