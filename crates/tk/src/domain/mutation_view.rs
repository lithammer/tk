//! Engine-built decoded view of one Mutation Log row handed to an adapter.
//!
//! Ported from `src/domain/mutation_view.zig`. The Zig version held borrowed
//! slices and pushed ownership cleanup back to `store.deinitMutationView`;
//! the Rust port owns its strings directly so `Drop` handles cleanup and the
//! store layer does not have to track aliasing.

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
}
