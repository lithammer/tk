//! Snapshot of one backend-owned Item observed during a Backend Pull.
//!
//! A self-owned value (no borrows) so it can cross the Backend Adapter
//! boundary freely: produced by `Adapter::fetch_snapshots` and consumed by
//! `store::merge_backend_snapshots`.

use super::item_class::ItemClass;
use super::status::ItemStatus;
use super::ticket_kind::TicketKind;

/// One backend-owned Item snapshot returned from a Backend Pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemSnapshot {
    /// Backend kind discriminator — `"github"` or `"jira"`.
    pub backend_kind: String,
    /// Backend-native identifier (e.g. GitHub issue number or Jira issue key).
    pub backend_key: String,
    /// Display ID assigned by the adapter.
    ///
    /// MUST be in a namespace that cannot collide with the local store prefix.
    /// The github adapter uses `gh-<issue-number>`; the jira adapter uses the
    /// natural Jira key (e.g. `PROJ-123`). After tk-22 lands, the
    /// prefix-change command must revalidate against the configured adapter;
    /// until then this is enforced by `validate_remote_against_local_prefix`
    /// at `tk remote set` time.
    pub display_id: String,
    /// Item Class (`ItemClass::Ticket` or `ItemClass::Epic`).
    pub item_class: ItemClass,
    /// Ticket Kind for tickets; `None` for epics.
    pub ticket_kind: Option<TicketKind>,
    /// Title rendered by the backend.
    pub title: String,
    /// Body rendered by the backend (may be empty).
    pub body: String,
    /// Item Status mapped from the backend's lifecycle state.
    pub status: ItemStatus,
    /// Reserved field — `backend_updated_at` (ISO-8601 string) is collected by
    /// adapters but ignored by the engine in v1. Kept on the snapshot so
    /// future change-detection work can fill in without a contract churn.
    pub backend_updated_at: String,
}
