//! Backend Intent: whether an Item carries backend identity, will carry one
//! because its Promotion is already durable, or is Local with no Promotion
//! intent at all.
//!
//! Names CONTEXT.md's **Pending Promotion** — "a Local Ticket or Local Epic
//! with durable Promotion intent that has not yet received its backend
//! identity" — as a value. ADR-0036 makes it the question a write path asks
//! instead of Origin: an Item appends Mutations when it is a Backend Item *or*
//! when it is a Local Item whose Promotion is in the Mutation Log.
//!
//! The Backend rides the two backend-bound states as the storage spelling
//! rather than as [`BackendKind`](super::backend_kind::BackendKind).
//! `remotes.backend_kind` has a CHECK constraint but `items.backend_kind` has
//! none, so a typed enum here would need a corruption arm for values the
//! schema admits. Comparing two Items' Backends stays string equality.

/// Where an Item stands relative to backend identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendIntent {
    /// Local Origin with no Promotion in the Mutation Log: changes to this
    /// Item are Repository Store current state and nothing more.
    Local,
    /// Pending Promotion: Local Origin, with a `promote_ticket` /
    /// `promote_epic` Mutation awaiting or retrying Apply. The Backend is the
    /// one that Promotion targets, frozen in its payload at commit time rather
    /// than read from current Remote configuration (ADR-0036).
    PendingPromotion { backend_kind: String },
    /// Backend Origin: the Item already holds `items.backend_kind` and
    /// `items.backend_key`.
    Backend { backend_kind: String },
}

impl BackendIntent {
    /// The Backend this Item belongs to, or will belong to once its Promotion
    /// resolves. `None` only for a Local Item with no Promotion intent.
    #[must_use]
    pub fn backend_kind(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::PendingPromotion { backend_kind } | Self::Backend { backend_kind } => {
                Some(backend_kind)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_local_item_has_no_backend() {
        assert_eq!(BackendIntent::Local.backend_kind(), None);
        assert_eq!(
            BackendIntent::PendingPromotion {
                backend_kind: "github".into()
            }
            .backend_kind(),
            Some("github")
        );
        assert_eq!(
            BackendIntent::Backend {
                backend_kind: "jira".into()
            }
            .backend_kind(),
            Some("jira")
        );
    }
}
