//! Typed payload variants for Mutation Log entries.
//!
//! Ported from `src/domain/mutation_payload.zig`. Each variant maps to one or
//! more [`super::mutation_type::MutationType`] values. Backend Adapters consume
//! these typed payloads instead of command-specific JSON.

/// Typed payload union for a `mutations` row. Variant choice is determined by
/// the row's `mutation_type` discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationPayload {
    /// Payload for `update_ticket` and `update_epic` — full title/body snapshot
    /// of the current state after the edit.
    UpdateTitleBody(TitleBody),
    /// Payload for `add_ticket_to_epic` and `remove_ticket_from_epic` — the
    /// internal stable ID of the Epic being referenced.
    EpicRef(EpicRef),
    /// Payload for `set_item_status` — target Item Status after the change.
    ItemStatus(StatusChange),
    /// Payload for `add_dependency` and `remove_dependency` — the internal
    /// stable ID of the Blocking Item referenced by the Dependency.
    DependencyRef(DependencyRef),
}

/// Full title/body snapshot used by `update_ticket` / `update_epic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleBody {
    pub title: String,
    pub body: String,
}

/// Epic reference used by `add_ticket_to_epic` / `remove_ticket_from_epic`.
///
/// `epic_id` is the internal stable `items.id`, not the Display ID, so
/// Promotion cannot break the reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpicRef {
    pub epic_id: String,
}

/// Status-change payload used by `set_item_status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChange {
    pub status: String,
}

/// Blocking Item reference used by `add_dependency` / `remove_dependency`.
///
/// `blocking_id` is the internal stable `items.id` of the Blocking Item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyRef {
    pub blocking_id: String,
}
