//! Directional Backend Adapter operation contracts.
//!
//! [`AdoptedItem`] is complete intake data for the one Backend Ticket an
//! Adapter canonicalizes. [`BackendItemRefresh`] only carries backend-owned
//! fields for an existing Backend Item, so a Backend Pull cannot redefine its
//! Repository Store identity or Item Class (ADR-0034).

use super::mutation_payload::{StatusChange, TitleBody};
use super::status::ItemStatus;
use super::ticket_kind::TicketKind;

/// Backend-owned identity assigned to one Backend Item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemIdentity {
    /// Backend-native identifier used to address the object.
    pub backend_key: String,
    /// Adapter-owned Display ID used after Adopt or Promotion.
    pub display_id: String,
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
    RemoveTicketFromEpic {
        ticket: BackendItemAddress,
        epic: BackendItemAddress,
    },
}

/// A Promotion Mutation that creates a new Backend object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCreate {
    Ticket { snapshot: TitleBody },
    Epic { snapshot: TitleBody },
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
    /// Backend lifecycle mapped to tk's Item Status.
    pub status: ItemStatus,
}

/// Backend-owned fields returned by one Backend Pull refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemRefresh {
    pub title: String,
    pub body: String,
    /// Backend lifecycle mapped to tk's Item Status.
    pub status: ItemStatus,
    /// Ticket Kind when the Backend can refresh it; `None` preserves it.
    pub ticket_kind: Option<TicketKind>,
}
