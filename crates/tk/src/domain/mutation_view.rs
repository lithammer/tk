//! Engine-built decoded view of one Mutation Log row handed to an adapter.
//!
//! A self-owned value (no borrows) so the store layer hands it to an adapter
//! without tracking aliasing and `Drop` handles cleanup.

use super::item_class::ItemClass;
use super::mutation_payload::MutationPayload;
use super::mutation_type::MutationType;

/// Engine projection of a `mutations` row plus the joined `items` snapshot the
/// adapter needs in v1. Only the fields adapters need in v1 are included;
/// richer columns can be added without churning the row schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationView {
    /// `mutations.sequence` — monotonically increasing within a Repository
    /// Store, identifies this Mutation Log entry.
    pub sequence: i64,
    /// Typed Mutation kind from [`MutationType`].
    pub mutation_type: MutationType,
    /// Internal stable `items.id` (NOT the Display ID — promote-safe).
    pub item_id: String,
    /// Item Class of the target Item.
    pub item_class: ItemClass,
    /// Typed payload variant. Tag determined by `mutation_type`.
    pub payload: MutationPayload,
    /// Backend kind of the target Item, when known. `None` for items that
    /// have never reached the backend (local-origin pre-Promotion).
    pub backend_kind: Option<String>,
    /// Backend-native identifier of the target Item, when known. `None` for
    /// items that have never reached the backend (local-origin pre-Promotion).
    pub backend_key: Option<String>,
    /// Backend-native identifier of the *related* Item a relationship Mutation
    /// points at — the Blocking Item for `add_dependency`/`remove_dependency`
    /// (and, once sub-issue sync lands, the parent Epic). The Mutation payload
    /// stores the counterpart's internal `items.id` (promote-safe); the
    /// store-layer load resolves it to a `backend_key` here so the adapter
    /// reaches the backend without a store of its own. `None` for every
    /// non-relationship Mutation; for a relationship Mutation it is `Some` by
    /// construction, since those Mutations are emitted only for same-backend
    /// pairs (the counterpart is therefore a Backend Item with a key).
    pub counterpart_backend_key: Option<String>,
}
