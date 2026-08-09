//! Directional Backend Adapter operation contracts.
//!
//! [`AdoptedItem`] is complete intake data for the one Backend Ticket an
//! Adapter canonicalizes. [`BackendItemRefresh`] only carries backend-owned
//! fields for an existing Backend Item, so a Backend Pull cannot redefine its
//! Repository Store identity or Item Class (ADR-0034).

use super::status::ItemStatus;
use super::ticket_kind::TicketKind;
use super::{
    mutation_payload::{Promotion, StatusChange, TitleBody},
    mutation_type::MutationType,
};

/// Backend-owned identity assigned to one Backend Item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemIdentity {
    /// Backend-native identifier used to address the object.
    pub backend_key: String,
    /// Adapter-owned Display ID used after Adopt or Promotion.
    pub display_id: String,
}

/// An ordinary Mutation applied to objects that already exist remotely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendEdit {
    /// Replace the title/body snapshot of an existing Backend Ticket.
    UpdateTicket {
        sequence: i64,
        item_id: String,
        ticket: BackendItemIdentity,
        snapshot: TitleBody,
    },
    /// Replace the title/body snapshot of an existing Backend Epic.
    UpdateEpic {
        sequence: i64,
        item_id: String,
        epic: BackendItemIdentity,
        snapshot: TitleBody,
    },
    /// Set the lifecycle status of an existing Backend Item.
    SetItemStatus {
        sequence: i64,
        item_id: String,
        item: BackendItemIdentity,
        change: StatusChange,
    },
    /// Add a Dependency between two existing Backend Items.
    AddDependency {
        sequence: i64,
        item_id: String,
        blocked: BackendItemIdentity,
        blocking: BackendItemIdentity,
    },
    /// Remove a Dependency between two existing Backend Items.
    RemoveDependency {
        sequence: i64,
        item_id: String,
        blocked: BackendItemIdentity,
        blocking: BackendItemIdentity,
    },
    /// Add an existing Backend Ticket to an existing Backend Epic.
    AddTicketToEpic {
        sequence: i64,
        item_id: String,
        ticket: BackendItemIdentity,
        epic: BackendItemIdentity,
    },
    /// Remove an existing Backend Ticket from an existing Backend Epic.
    RemoveTicketFromEpic {
        sequence: i64,
        item_id: String,
        ticket: BackendItemIdentity,
        epic: BackendItemIdentity,
    },
}

/// A Promotion Mutation that creates a new Backend object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCreate {
    /// Create a Backend Ticket from a frozen Promotion snapshot.
    Ticket {
        sequence: i64,
        item_id: String,
        promotion: Promotion,
    },
    /// Create a Backend Epic from a frozen Promotion snapshot.
    Epic {
        sequence: i64,
        item_id: String,
        promotion: Promotion,
    },
}

/// Typed delivery operation resolved immediately before an Adapter call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendOperation {
    Edit(BackendEdit),
    Create(BackendCreate),
}

impl BackendEdit {
    /// Return the Mutation Sequence whose delivery this operation represents.
    #[must_use]
    pub fn sequence(&self) -> i64 {
        match self {
            Self::UpdateTicket { sequence, .. }
            | Self::UpdateEpic { sequence, .. }
            | Self::SetItemStatus { sequence, .. }
            | Self::AddDependency { sequence, .. }
            | Self::RemoveDependency { sequence, .. }
            | Self::AddTicketToEpic { sequence, .. }
            | Self::RemoveTicketFromEpic { sequence, .. } => *sequence,
        }
    }

    /// Return the stable Repository Store Item identity targeted by the Mutation.
    #[must_use]
    pub fn item_id(&self) -> &str {
        match self {
            Self::UpdateTicket { item_id, .. }
            | Self::UpdateEpic { item_id, .. }
            | Self::SetItemStatus { item_id, .. }
            | Self::AddDependency { item_id, .. }
            | Self::RemoveDependency { item_id, .. }
            | Self::AddTicketToEpic { item_id, .. }
            | Self::RemoveTicketFromEpic { item_id, .. } => item_id,
        }
    }

    /// Return the Mutation Type encoded by this operation variant.
    #[must_use]
    pub fn mutation_type(&self) -> MutationType {
        match self {
            Self::UpdateTicket { .. } => MutationType::UpdateTicket,
            Self::UpdateEpic { .. } => MutationType::UpdateEpic,
            Self::SetItemStatus { .. } => MutationType::SetItemStatus,
            Self::AddDependency { .. } => MutationType::AddDependency,
            Self::RemoveDependency { .. } => MutationType::RemoveDependency,
            Self::AddTicketToEpic { .. } => MutationType::AddTicketToEpic,
            Self::RemoveTicketFromEpic { .. } => MutationType::RemoveTicketFromEpic,
        }
    }
}

impl BackendCreate {
    /// Return the Mutation Sequence whose delivery this operation represents.
    #[must_use]
    pub fn sequence(&self) -> i64 {
        match self {
            Self::Ticket { sequence, .. } | Self::Epic { sequence, .. } => *sequence,
        }
    }

    /// Return the stable Repository Store Item identity targeted by the Promotion.
    #[must_use]
    pub fn item_id(&self) -> &str {
        match self {
            Self::Ticket { item_id, .. } | Self::Epic { item_id, .. } => item_id,
        }
    }

    /// Return the Promotion Mutation Type encoded by this operation variant.
    #[must_use]
    pub fn mutation_type(&self) -> MutationType {
        match self {
            Self::Ticket { .. } => MutationType::PromoteTicket,
            Self::Epic { .. } => MutationType::PromoteEpic,
        }
    }

    /// Return the frozen Promotion snapshot used for Backend creation.
    #[must_use]
    pub fn promotion(&self) -> &Promotion {
        match self {
            Self::Ticket { promotion, .. } | Self::Epic { promotion, .. } => promotion,
        }
    }
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
    /// Backend title.
    pub title: String,
    /// Backend body.
    pub body: String,
    /// Backend lifecycle mapped to tk's Item Status.
    pub status: ItemStatus,
}

/// Backend-owned fields returned by one Backend Pull refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemRefresh {
    /// Backend title.
    pub title: String,
    /// Backend body.
    pub body: String,
    /// Backend lifecycle mapped to tk's Item Status.
    pub status: ItemStatus,
    /// Ticket Kind when the Backend can refresh it; `None` preserves it.
    pub ticket_kind: Option<TicketKind>,
}
