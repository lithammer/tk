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
//! structs ([`TitleBody`], [`EpicRef`], [`StatusChange`], [`DependencyRef`])
//! rather than on the outer enum: serializing the enum directly would
//! produce externally-tagged JSON (`{"UpdateTitleBody":{…}}`) that breaks
//! the flat row contract.

use serde::{Deserialize, Serialize};

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

impl MutationPayload {
    /// Serialize the inner per-variant struct to JSON for the
    /// `mutations.payload_json` column. The flat-object shape matches the
    /// CHECK constraint and the format Backend Adapters round-trip back out.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        match self {
            Self::UpdateTitleBody(v) => serde_json::to_string(v),
            Self::EpicRef(v) => serde_json::to_string(v),
            Self::ItemStatus(v) => serde_json::to_string(v),
            Self::DependencyRef(v) => serde_json::to_string(v),
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

/// Epic reference used by `add_ticket_to_epic` / `remove_ticket_from_epic`.
///
/// `epic_id` is the internal stable `items.id`, not the Display ID, so
/// Promotion cannot break the reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicRef {
    pub epic_id: String,
}

/// Status-change payload used by `set_item_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusChange {
    pub status: String,
}

/// Blocking Item reference used by `add_dependency` / `remove_dependency`.
///
/// `blocking_id` is the internal stable `items.id` of the Blocking Item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyRef {
    pub blocking_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_body_json_is_flat() {
        let json = MutationPayload::UpdateTitleBody(TitleBody {
            title: "T".into(),
            body: "B".into(),
        })
        .to_json_string();
        assert_eq!(json, r#"{"title":"T","body":"B"}"#);
    }

    #[test]
    fn epic_ref_json_is_flat() {
        let json = MutationPayload::EpicRef(EpicRef {
            epic_id: "epic-id".into(),
        })
        .to_json_string();
        assert_eq!(json, r#"{"epic_id":"epic-id"}"#);
    }

    #[test]
    fn status_change_json_is_flat() {
        let json = MutationPayload::ItemStatus(StatusChange {
            status: "done".into(),
        })
        .to_json_string();
        assert_eq!(json, r#"{"status":"done"}"#);
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
    fn title_body_round_trips_with_free_text() {
        let original = TitleBody {
            title: "Quote \" and backslash \\ and newline \n inside".into(),
            body: String::new(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: TitleBody = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, original);
    }
}
