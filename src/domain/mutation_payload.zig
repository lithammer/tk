//! Typed payload variants for Mutation Log entries.
//!
//! Each variant maps to one or more `MutationType` values. Backend Adapters
//! consume these typed Mutation Log payloads instead of command-specific JSON.
//! Lives in `src/domain/` because it is pure data — no SQLite, filesystem,
//! Git, or subprocess dependencies — and is consumed by `store/`, `remote/`,
//! and the future `sync/` engine alike.

/// Typed payload union for a `mutations` row.
pub const MutationPayload = union(enum) {
    /// Payload for `update_ticket` and `update_epic` — full title/body
    /// snapshot of the current state after the edit.
    update_title_body: TitleBody,
    /// Payload for `add_ticket_to_epic` and `remove_ticket_from_epic` —
    /// the internal stable ID of the Epic being referenced.
    epic_ref: EpicRef,
    /// Payload for `set_item_status` — target Item Status after the change.
    item_status: StatusChange,
    /// Payload for `add_dependency` and `remove_dependency` — the internal
    /// stable ID of the Blocking Item referenced by the Dependency.
    dependency_ref: DependencyRef,

    /// Full title/body snapshot used by `update_ticket` / `update_epic`.
    pub const TitleBody = struct {
        title: []const u8,
        body: []const u8,
    };

    /// Epic reference used by `add_ticket_to_epic` / `remove_ticket_from_epic`.
    ///
    /// `epic_id` is the internal stable `items.id`, not the Display ID, so
    /// Promotion cannot break the reference.
    pub const EpicRef = struct {
        epic_id: []const u8,
    };

    /// Status-change payload used by `set_item_status`.
    pub const StatusChange = struct {
        status: []const u8,
    };

    /// Blocking Item reference used by `add_dependency` / `remove_dependency`.
    pub const DependencyRef = struct {
        blocking_id: []const u8,
    };
};
