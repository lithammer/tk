//! Directional Backend Adapter operation contracts.
//!
//! [`AdoptedItem`] is complete intake data for the one Backend Ticket an
//! Adapter canonicalizes. [`BackendItemRefresh`] only carries backend-owned
//! fields for an existing Backend Item, so a Backend Pull cannot redefine its
//! Repository Store identity or Item Class (ADR-0034).

use super::lifecycle::Lifecycle;
use super::mutation_payload::{StatusChange, TitleBody};
use super::ticket_kind::TicketKind;
use thiserror::Error;

/// Backend-owned identity assigned to one Backend Item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemIdentity {
    /// Backend-native identifier used to address the object.
    pub backend_key: String,
    /// Adapter-owned Display ID used after Adopt or Promotion.
    pub display_id: String,
}

impl std::fmt::Display for BackendItemIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.display_id, self.backend_key)
    }
}

/// Canonical Backend identity and content observed while recovering a
/// Promotion whose creation outcome was indeterminate.
///
/// The Backend Adapter owns canonicalizing the Backend key and Display ID;
/// recovery workflows use the returned identity to bind the Pending
/// Promotion and the title/body to reconcile the local item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemInspection {
    /// Canonical identity assigned by the Backend Adapter.
    pub identity: BackendItemIdentity,
    /// Current Backend title observed during inspection.
    pub title: String,
    /// Current Backend body observed during inspection.
    pub body: String,
    /// Ticket Kind mapped from the Backend representation.
    pub ticket_kind: TicketKind,
}

/// Backend-native address of an object that already exists remotely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemAddress {
    pub backend_key: String,
}

/// An ordinary Mutation applied to objects that already exist remotely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendEdit {
    UpdateTicket {
        ticket: BackendItemAddress,
        snapshot: TitleBody,
    },
    UpdateEpic {
        epic: BackendItemAddress,
        snapshot: TitleBody,
    },
    SetItemStatus {
        item: BackendItemAddress,
        change: StatusChange,
    },
    AddDependency {
        blocked: BackendItemAddress,
        blocking: BackendItemAddress,
    },
    RemoveDependency {
        blocked: BackendItemAddress,
        blocking: BackendItemAddress,
    },
    AddTicketToEpic {
        ticket: BackendItemAddress,
        epic: BackendItemAddress,
    },
    /// Clearing a Ticket's containing Epic names only the Ticket: Epic
    /// membership is 0..1, so the Repository Store's cleared `container_id` is
    /// the whole intent and no counterpart identity has to be addressable for
    /// the removal to be delivered.
    RemoveTicketFromEpic { ticket: BackendItemAddress },
}

/// A Promotion Mutation that creates a new Backend object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCreate {
    Ticket {
        snapshot: TitleBody,
        /// Current Ticket Kind from the Repository Store. This is transient
        /// delivery data, not persisted Promotion intent.
        ticket_kind: TicketKind,
    },
    Epic {
        snapshot: TitleBody,
    },
}

/// Typed delivery operation resolved immediately before an Adapter call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendOperation {
    Edit(BackendEdit),
    Create(BackendCreate),
}

/// Canonical Backend Ticket data returned by an Adapter Adopt operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedItem {
    /// Adapter-canonical backend key for the adopted Ticket.
    pub backend_key: String,
    /// Adapter-owned Display ID for the Backend Ticket.
    pub display_id: String,
    /// Ticket Kind mapped from the Backend's issue classification.
    pub ticket_kind: TicketKind,
    pub title: String,
    pub body: String,
    /// Backend Lifecycle: the only Item Status axis an Adapter observes
    /// (ADR-0043).
    pub status: Lifecycle,
}

/// Backend-owned fields returned by one Backend Pull refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemRefresh {
    pub title: String,
    pub body: String,
    /// Backend Lifecycle: the only Item Status axis an Adapter observes
    /// (ADR-0043). Work State is local and is not part of what Pull reads.
    pub status: Lifecycle,
    /// Ticket Kind when the Backend can refresh it; `None` preserves it.
    pub ticket_kind: Option<TicketKind>,
}

/// One keyed refresh returned as part of a Backend Pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendPullItem {
    /// The exact requested Backend address this refresh answers.
    pub address: BackendItemAddress,
    /// Backend-owned fields observed for the addressed Item.
    pub refresh: BackendItemRefresh,
}

/// A validated, all-or-nothing refresh of an exact Backend working set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendPull {
    items: Vec<BackendPullItem>,
}

impl BackendPull {
    /// Validate exact count, order, and byte-for-byte Backend keys.
    pub fn new(
        requested: &[BackendItemAddress],
        items: Vec<BackendPullItem>,
    ) -> Result<Self, BackendPullError> {
        if requested.len() != items.len() {
            return Err(BackendPullError::Count {
                requested: requested.len(),
                returned: items.len(),
            });
        }
        for (index, (expected, item)) in requested.iter().zip(&items).enumerate() {
            if item.address.backend_key != expected.backend_key {
                return Err(BackendPullError::Key {
                    index,
                    requested: expected.backend_key.clone(),
                    returned: item.address.backend_key.clone(),
                });
            }
        }
        Ok(Self { items })
    }

    /// Consume the validated Pull in Repository Store merge shape.
    #[must_use]
    pub fn into_refreshes(self) -> Vec<(String, BackendItemRefresh)> {
        self.items
            .into_iter()
            .map(|item| (item.address.backend_key, item.refresh))
            .collect()
    }
}

/// A Backend Adapter returned a Pull that did not match its request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BackendPullError {
    #[error("Backend Pull returned {returned} items for {requested} requested keys")]
    Count { requested: usize, returned: usize },
    #[error(
        "Backend Pull item at index {index} returned key '{returned}' for requested key '{requested}'"
    )]
    Key {
        index: usize,
        requested: String,
        returned: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(key: &str) -> BackendItemAddress {
        BackendItemAddress {
            backend_key: key.into(),
        }
    }

    fn item(key: &str) -> BackendPullItem {
        BackendPullItem {
            address: address(key),
            refresh: BackendItemRefresh {
                title: key.into(),
                body: String::new(),
                status: Lifecycle::Open,
                ticket_kind: Some(TicketKind::Task),
            },
        }
    }

    #[test]
    fn backend_pull_requires_exact_count_order_and_keys() {
        let requested = [address("a"), address("b")];

        assert!(matches!(
            BackendPull::new(&requested, vec![item("a")]),
            Err(BackendPullError::Count {
                requested: 2,
                returned: 1
            })
        ));
        assert!(matches!(
            BackendPull::new(&requested, vec![item("b"), item("a")]),
            Err(BackendPullError::Key {
                index: 0,
                requested,
                returned
            }) if requested == "a" && returned == "b"
        ));

        let pull = BackendPull::new(&requested, vec![item("a"), item("b")]).unwrap();
        assert_eq!(
            pull.into_refreshes()
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
