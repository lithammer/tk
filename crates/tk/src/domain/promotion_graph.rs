//! The Repository Store snapshot Promotion reads and Re-Adopt narrows to its
//! target's relationships.
//!
//! Facts only: Items, their Dependency edges, and the Promotion child choice.
//! The planners decide what changes, what the Backend can represent, and how
//! the outbox is ordered.
//!
//! [`crate::store::promotion::read_graph`] is the only producer.

use crate::domain::backend_binding::BackendBinding;
use crate::domain::item_class::ItemClass;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::selection_state::SelectionState;
use crate::domain::ticket_kind::TicketKind;

/// One Ticket or Epic in the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphItem {
    /// Internal stable `items.id`. Every reference within the snapshot uses it
    /// rather than the Display ID, which Promotion replaces.
    pub id: String,
    /// Current Display ID, for rendering findings against what the user typed.
    pub display_id: String,
    pub item_class: ItemClass,
    /// `Some` for Tickets, `None` for Epics (`items.ticket_kind` CHECK).
    pub ticket_kind: Option<TicketKind>,
    /// `Some` for Tickets, `None` for Epics — Selection State is Ticket-only
    /// (ADR-0027). Promotion refuses a `triage` Ticket.
    pub selection_state: Option<SelectionState>,
    /// Backend-shared Lifecycle (ADR-0043). Work State is local, so it is
    /// no part of what Promotion drafts.
    pub status: Lifecycle,
    /// Title and body as they stand now; the Promotion payload freezes this
    /// snapshot at commit.
    pub title: String,
    pub body: String,
    /// `items.created_seq`. Findings render in creation order, so the order is
    /// carried as data rather than left to the order Items were collected in.
    pub created_seq: i64,
    /// Containing Epic's internal id, when the Item is a Ticket in one.
    pub container_id: Option<String>,
    pub backend_binding: BackendBinding,
}

/// One Dependency edge in the snapshot, as internal `items.id` endpoints.
///
/// Both endpoints are always present in [`PromotionGraph::items`], including
/// endpoints outside the promoted set and Blocking Items that are already
/// `done` — a done Blocking Item resolves readiness but keeps its Dependency
/// (ADR-0035).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphDependency {
    pub blocked_id: String,
    pub blocking_id: String,
}

/// Everything one Promotion or Re-Adopt relationship preflight reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionGraph {
    /// Internal id of the Item named on the command line.
    pub target_id: String,
    /// Whether `--children` was passed. The planner, not the read, decides
    /// which contained Tickets are Promotion Children.
    pub children_requested: bool,
    /// The target, any contained Tickets, any containing Epic, and every
    /// Dependency endpoint reachable in one hop from those — in creation
    /// order.
    pub items: Vec<GraphItem>,
    pub dependencies: Vec<GraphDependency>,
}
