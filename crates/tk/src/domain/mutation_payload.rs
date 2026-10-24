//! Typed payload variants for Mutation Log entries.
//!
//! Each variant maps to one or more [`super::mutation_type::MutationType`]
//! values. The Mutation Log outbox writes the *inner* struct to
//! `mutations.payload_json` (a flat object per row), and Backend Adapters
//! read the same shape back — keeping the on-disk schema independent of
//! Rust's enum discriminator and friendly to the `json_valid()` CHECK
//! constraint on the column.
//!
//! `Serialize`/`Deserialize` therefore live on the per-variant payload
//! structs ([`TitleBody`], [`ContainerRef`], [`LifecycleChange`],
//! [`PriorityChange`], [`EffortChange`], [`DependencyRef`], [`Promotion`])
//! rather than on the outer enum: serializing the enum directly would produce externally-tagged JSON
//! (`{"UpdateTitleBody":{…}}`) that breaks the flat row contract.

use serde::{Deserialize, Serialize};

use crate::domain::effort::Effort;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::priority::Priority;

/// Typed payload union for a `mutations` row. Variant choice is determined by
/// the row's `mutation_type` discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationPayload {
    /// Payload for `update_ticket` and `update_epic` — full title/body snapshot
    /// of the current state after the edit.
    UpdateTitleBody(TitleBody),
    /// Payload for `set_ticket_container` and `clear_ticket_container` — the
    /// internal stable ID of the Container being referenced.
    ContainerRef(ContainerRef),
    /// Payload for `set_item_status` — target Lifecycle after the change.
    Lifecycle(LifecycleChange),
    /// Payload for `set_ticket_priority` — target Priority after the change.
    Priority(PriorityChange),
    /// Payload for `set_ticket_effort` — target Effort after the change.
    Effort(EffortChange),
    /// Payload for `add_dependency` and `remove_dependency` — the internal
    /// stable ID of the Blocking Item referenced by the Dependency.
    DependencyRef(DependencyRef),
    /// Payload for `promote_ticket` and `promote_epic` — the title/body
    /// snapshot frozen at Promotion commit and the Backend the operation
    /// targets.
    Promotion(Promotion),
}

impl MutationPayload {
    /// Serialize the inner per-variant struct to JSON for the
    /// `mutations.payload_json` column. The flat-object shape matches the
    /// CHECK constraint and the format Backend Adapters round-trip back out.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        match self {
            Self::UpdateTitleBody(v) => serde_json::to_string(v),
            Self::ContainerRef(v) => serde_json::to_string(v),
            Self::Lifecycle(v) => serde_json::to_string(v),
            Self::Priority(v) => serde_json::to_string(v),
            Self::Effort(v) => serde_json::to_string(v),
            Self::DependencyRef(v) => serde_json::to_string(v),
            Self::Promotion(v) => serde_json::to_string(v),
        }
        .expect("MutationPayload inner structs are infallible serializers")
    }
}

/// Full title/body snapshot used by `update_ticket` / `update_epic`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleBody {
    pub title: String,
    pub body: String,
}

/// Container reference used by `set_ticket_container` /
/// `clear_ticket_container`. The Container is an Epic or a Ticket (ADR-0055).
///
/// `container_id` is the internal stable `items.id`, not the Display ID, so
/// Promotion cannot break the reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerRef {
    pub container_id: String,
}

/// Lifecycle-change payload used by `set_item_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleChange {
    pub status: Lifecycle,
}

/// Priority-change payload used by `set_ticket_priority`. The target value,
/// not a delta, so replaying it converges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorityChange {
    pub priority: Priority,
}

/// Effort-change payload used by `set_ticket_effort`. The target value, so
/// replaying it converges; `None` clears the Effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortChange {
    pub effort: Option<Effort>,
}

/// Blocking Item reference used by `add_dependency` / `remove_dependency`.
///
/// `blocking_id` is the internal stable `items.id` of the Blocking Item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyRef {
    pub blocking_id: String,
}

/// Title/body snapshot and target Backend for `promote_ticket` /
/// `promote_epic`.
///
/// `title` and `body` freeze the Local item's state at Promotion commit; a
/// later edit before Promotion is applied appends an ordinary `update_ticket`
/// / `update_epic` Mutation instead of changing this snapshot. `backend_kind`
/// is the storage spelling (matching
/// [`super::backend_kind::BackendKind::text`]) of the Backend this Promotion
/// targets. It is recorded here, not read from Remote configuration, so a
/// later read of an Item's Pending Promotion state uses the Mutation Log
/// alone (ADR-0036 "Promotion Mutations target Local items").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Promotion {
    pub title: String,
    pub body: String,
    pub backend_kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_body_json_is_flat() {
        for (title, body, expected) in [
            ("T", "B", r#"{"title":"T","body":"B"}"#),
            (
                "Quote \" and backslash \\ and newline \n inside",
                "",
                r#"{"title":"Quote \" and backslash \\ and newline \n inside","body":""}"#,
            ),
        ] {
            let original = TitleBody {
                title: title.into(),
                body: body.into(),
            };
            let json = MutationPayload::UpdateTitleBody(original.clone()).to_json_string();
            assert_eq!(json, expected);
            let restored: TitleBody = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, original);
        }
    }

    #[test]
    fn container_ref_json_is_flat() {
        let json = MutationPayload::ContainerRef(ContainerRef {
            container_id: "container-id".into(),
        })
        .to_json_string();
        assert_eq!(json, r#"{"container_id":"container-id"}"#);
    }

    #[test]
    fn lifecycle_change_json_is_flat() {
        let json = MutationPayload::Lifecycle(LifecycleChange {
            status: Lifecycle::Done,
        })
        .to_json_string();
        assert_eq!(json, r#"{"status":"done"}"#);
    }

    #[test]
    fn priority_change_json_is_flat() {
        let json = MutationPayload::Priority(PriorityChange {
            priority: Priority::P0,
        })
        .to_json_string();
        assert_eq!(json, r#"{"priority":"P0"}"#);
    }

    #[test]
    fn dependency_ref_json_is_flat() {
        let json = MutationPayload::DependencyRef(DependencyRef {
            blocking_id: "blocker-id".into(),
        })
        .to_json_string();
        assert_eq!(json, r#"{"blocking_id":"blocker-id"}"#);
    }

    #[test]
    fn promotion_json_is_flat() {
        for (title, body, expected) in [
            (
                "T",
                "B",
                r#"{"title":"T","body":"B","backend_kind":"github"}"#,
            ),
            (
                "Quote \" and backslash \\ and newline \n inside",
                "",
                r#"{"title":"Quote \" and backslash \\ and newline \n inside","body":"","backend_kind":"github"}"#,
            ),
        ] {
            let original = Promotion {
                title: title.into(),
                body: body.into(),
                backend_kind: "github".into(),
            };
            let json = MutationPayload::Promotion(original.clone()).to_json_string();
            assert_eq!(json, expected);
            let restored: Promotion = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, original);
        }
    }
}
