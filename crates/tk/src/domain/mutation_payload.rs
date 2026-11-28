//! Typed payload variants for Mutation Log entries.
//!
//! The Repository Store writes each payload as a flat JSON object and uses
//! the row's [`super::mutation_type::MutationType`] to decode it. Serialize
//! the inner payload, not the outer enum: its tag is not part of the row
//! contract.

use serde::{Deserialize, Serialize};

use crate::domain::effort::Effort;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::priority::Priority;

/// Typed payload union for a `mutations` row. Variant choice is determined by
/// the row's `mutation_type` discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationPayload {
    /// Payload for `update_ticket` and `update_epic`.
    ContentEdit(ContentEdit),
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
    /// Serialize the flat object stored in `mutations.payload_json`.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        match self {
            Self::ContentEdit(v) => serde_json::to_string(v),
            Self::ContainerRef(v) => serde_json::to_string(v),
            Self::Lifecycle(v) => serde_json::to_string(v),
            Self::Priority(v) => serde_json::to_string(v),
            Self::Effort(v) => serde_json::to_string(v),
            Self::DependencyRef(v) => serde_json::to_string(v),
            Self::Promotion(v) => serde_json::to_string(v),
        }
        .expect("MutationPayload inner types are infallible serializers")
    }
}

/// At least one title or body replacement. Omission leaves a field alone;
/// an empty body clears it. Older snapshots decode as `Both` even with
/// extra keys; a one-field edit accepts only its own key (ADR-0058).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ContentEdit {
    Both { title: String, body: String },
    Title { title: String },
    Body { body: String },
}

impl ContentEdit {
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        match self {
            Self::Both { title, .. } | Self::Title { title } => Some(title),
            Self::Body { .. } => None,
        }
    }

    #[must_use]
    pub fn body(&self) -> Option<&str> {
        match self {
            Self::Both { body, .. } | Self::Body { body } => Some(body),
            Self::Title { .. } => None,
        }
    }
}

impl<'de> Deserialize<'de> for ContentEdit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Do not read a malformed full snapshot as a one-field edit.
        #[derive(Deserialize)]
        #[serde(untagged, deny_unknown_fields)]
        enum Single {
            Title { title: String },
            Body { body: String },
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Both { title: String, body: String },
            Single(Single),
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::Both { title, body } => Self::Both { title, body },
            Wire::Single(Single::Title { title }) => Self::Title { title },
            Wire::Single(Single::Body { body }) => Self::Body { body },
        })
    }
}

/// Full content supplied when creating a Backend item through Promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
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
/// not a delta, so replaying it converges; `None` clears Priority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorityChange {
    // A missing field is malformed intent; only explicit null clears Priority.
    #[serde(deserialize_with = "Option::deserialize")]
    pub priority: Option<Priority>,
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
            let original = ContentEdit::Both {
                title: title.into(),
                body: body.into(),
            };
            let json = MutationPayload::ContentEdit(original.clone()).to_json_string();
            assert_eq!(json, expected);
            let restored: ContentEdit = serde_json::from_str(&json).unwrap();
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
            priority: Some(Priority::P0),
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
