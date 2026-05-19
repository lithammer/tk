//! Engine-built decoded view of one Mutation Log row handed to an adapter.
//!
//! Lives in `src/domain/` because it is pure data with no SQLite, filesystem,
//! Git, or subprocess dependencies. Produced by `store.loadApplicableMutations`
//! and consumed by `Adapter.applyMutation`. Ownership cleanup lives in
//! `store.deinitMutationView` because the store is what allocates the slices
//! when decoding from `mutations` rows.

const ItemClass = @import("item_class.zig").ItemClass;
const MutationPayload = @import("mutation_payload.zig").MutationPayload;
const MutationType = @import("mutation_type.zig").MutationType;

/// Engine projection of a `mutations` row plus the joined `items` snapshot
/// the adapter needs in v1.
///
/// Only the fields adapters need in v1 are included; richer columns can be
/// added without churning the row schema.
pub const MutationView = struct {
    /// `mutations.sequence` — monotonically increasing within a Repository
    /// Store, identifies this Mutation Log entry.
    sequence: i64,
    /// Typed Mutation kind from `MutationType`.
    mutation_type: MutationType,
    /// Internal stable `items.id` (NOT the Display ID — promote-safe).
    item_id: []const u8,
    /// Item Class of the target Item.
    item_class: ItemClass,
    /// Typed payload variant. Tag determined by `mutation_type`.
    payload: MutationPayload,
    /// Backend kind of the target Item, when known. `null` for items that
    /// have never reached the backend (local-origin pre-Promotion).
    backend_kind: ?[]const u8,
    /// Backend-native identifier of the target Item, when known. `null` for
    /// items that have never reached the backend (local-origin pre-Promotion).
    backend_key: ?[]const u8,
};
