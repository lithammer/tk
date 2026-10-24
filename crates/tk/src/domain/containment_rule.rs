//! What Containment means once both Items' Backend Binding is known
//! (ADR-0035, ADR-0055).
//!
//! Promotion and Re-Adopt ask this of different resulting graphs. The rule
//! lives here so both capture Containment only when the Ticket and its
//! Container share one Backend, while mixed-Origin Containment remains
//! Repository Store state.

use super::backend_binding::BackendBinding;

/// What Containment means for the Mutation Log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Both Items share one Backend, so Containment carries a
    /// `set_ticket_container` Mutation.
    BecomesBackendIntent,
    /// The Items do not share one Backend, so Containment stays in the
    /// Repository Store only.
    StaysLocal,
}

/// Classify Containment from the Ticket and its Container's resulting
/// Bindings.
#[must_use]
pub fn classify(ticket: &BackendBinding, container: &BackendBinding) -> Classification {
    match (ticket.backend_kind(), container.backend_kind()) {
        (Some(ticket_kind), Some(container_kind)) if ticket_kind == container_kind => {
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
