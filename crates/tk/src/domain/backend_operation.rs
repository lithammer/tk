//! Directional Backend Adapter read contracts.
//!
//! [`AdoptedItem`] is complete intake data for the one Backend Ticket an
//! Adapter canonicalizes. [`BackendItemRefresh`] only carries backend-owned
//! fields for an existing Backend Item, so a Backend Pull cannot redefine its
//! Repository Store identity or Item Class (ADR-0034).

use super::status::ItemStatus;
use super::ticket_kind::TicketKind;

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
