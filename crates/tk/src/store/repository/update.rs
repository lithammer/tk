//! Store writes for `tk update`.
//!
//! A content Mutation carries only the fields the caller named, even if they
//! match the Store (ADR-0058). Local changes and queued Mutations commit
//! together.
//! A parent change emits one `set_ticket_container` or `clear_ticket_container`,
//! never a pair (ADR-0055). Priority changes emit `set_ticket_priority`; Effort
//! changes emit `set_ticket_effort` only where Effort is Shared (ADR-0054).
//!
//! Content edits need only the Item's Backend Binding. A Containment set
//! needs both Items bound to the same Backend. Moving to a Container that
//! does not bind there clears the old Backend Containment, if any.
//!
//! A Mutation queued without a local field change leaves `updated_at` alone.

use rusqlite::{OptionalExtension, params};

use crate::clock::Clock;
use crate::domain::backend_binding::BackendBinding;
use crate::domain::effort::{BackendEffort, Effort};
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{
    ContainerRef, ContentEdit, EffortChange, MutationPayload, PriorityChange,
};
use crate::domain::mutation_type::MutationType;
use crate::domain::priority::Priority;
use crate::store::mutations;

use super::Store;
use super::containment::{self, ContainmentError};

/// An edit of one optional field: keep it, clear it, or set it. The caller
/// passes what it asked for; [`update_item`] narrows it to what changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FieldEdit<T> {
    /// Leave the field as-is; no Mutation is emitted.
    #[default]
    Unchanged,
    /// Set the field to NULL.
    Clear,
    Set(T),
}

impl<T: Copy + PartialEq> FieldEdit<T> {
    /// This edit against the stored value: `Unchanged` when it would write
    /// what is already there.
    fn against(self, current: Option<T>) -> Self {
        match self {
            Self::Clear if current.is_none() => Self::Unchanged,
            Self::Set(value) if current == Some(value) => Self::Unchanged,
            edit => edit,
        }
    }

    /// Whether the edit writes the column, and the value it writes.
    fn column(self) -> (bool, Option<T>) {
        match self {
            Self::Unchanged => (false, None),
            Self::Clear => (true, None),
            Self::Set(value) => (true, Some(value)),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct UpdateRequest<'a> {
    /// Internal stable `items.id` of the row to update.
    pub id: &'a str,
    pub content: Option<ContentEdit>,
    /// The caller keeps this unchanged on Epics; the schema rejects a value.
    pub priority: FieldEdit<Priority>,
    /// Like `priority`, the caller keeps this [`FieldEdit::Unchanged`] on an
    /// Epic; the `items.effort` CHECK backs that up.
    pub effort: FieldEdit<Effort>,
    /// The Container's internal stable `items.id`.
    pub parent: FieldEdit<&'a str>,
}

/// Snapshot returned on a successful update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedItem {
    pub display_id: String,
    pub title: String,
    pub item_class: ItemClass,
}

/// Why [`update_item`] did not return an [`UpdatedItem`]. `NotFound` renders
/// at exit 1; its `#[error]` string is internal — `tk update` interpolates the
/// id into its own line.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    BackendKind(#[from] crate::domain::backend_kind::ParseBackendKindError),
    /// `id` does not resolve to a live row.
    #[error("item not found")]
    NotFound,
    /// `parent` names a Container the depth cap refuses (ADR-0055).
    #[error(transparent)]
    Containment(#[from] ContainmentError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    BackendBinding(#[from] mutations::BackendBindingError),
    #[error(transparent)]
    Mutation(#[from] mutations::AppendError),
}

struct Current {
    item_class: ItemClass,
    title: String,
    body_changed: bool,
    priority: Option<Priority>,
    effort: Option<Effort>,
    container_id: Option<String>,
    display_id: String,
}

struct ColumnWrites<'a> {
    title: Option<&'a str>,
    body: Option<&'a str>,
    priority: FieldEdit<Priority>,
    effort: FieldEdit<Effort>,
}

/// Apply per-field updates to a Ticket or Epic.
pub fn update_item<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    req: UpdateRequest<'_>,
) -> Result<UpdatedItem, UpdateError> {
    let now_iso = clock.now_iso();
    let tx = crate::store::write_transaction(&mut store.conn)?;
    let content = req.content.as_ref();
    let body = content.and_then(ContentEdit::body);

    let current = tx
        .query_row(
            "select title, case when ?2 is null then 0 else body is not ?2 end, \
                    priority, container_id, display_value, effort, item_class \
               from items where id = ?1",
            params![req.id, body],
            |r| {
                Ok(Current {
                    title: r.get(0)?,
                    body_changed: r.get(1)?,
                    priority: r.get(2)?,
                    container_id: r.get(3)?,
                    display_id: r.get(4)?,
                    effort: r.get(5)?,
                    item_class: r.get(6)?,
                })
            },
        )
        .optional()?;
    let Some(current) = current else {
        return Err(UpdateError::NotFound);
    };

    let new_title = content
        .and_then(ContentEdit::title)
        .filter(|title| *title != current.title);
    let new_body = body.filter(|_| current.body_changed);
    let new_priority = req.priority.against(current.priority);
    let new_effort = req.effort.against(current.effort);

    let parent = req.parent.against(current.container_id.as_deref());

    let changes_local_state = new_title.is_some()
        || new_body.is_some()
        || new_priority != FieldEdit::Unchanged
        || new_effort != FieldEdit::Unchanged
        || parent != FieldEdit::Unchanged;

    if changes_local_state {
        write_columns(
            &tx,
            req.id,
            &ColumnWrites {
                title: new_title,
                body: new_body,
                priority: new_priority,
                effort: new_effort,
            },
            &now_iso,
        )?;
    }
    match parent {
        FieldEdit::Set(container_id) => containment::set(&tx, req.id, container_id)?,
        FieldEdit::Clear => containment::clear(&tx, req.id)?,
        FieldEdit::Unchanged => {}
    }

    let title = new_title.map_or(current.title, str::to_owned);
    // A Pending Promotion counts as bound: the edit waits behind the
    // Mutation that will give the Item its Backend identity (ADR-0036).
    if (changes_local_state || req.content.is_some())
        && let binding = mutations::resolve_backend_binding(&tx, req.id)?
        && let Some(backend) = binding.backend_kind()
    {
        let container_mutation = match parent {
            FieldEdit::Set(new_id) if containment_is_backend_intent(&tx, &binding, new_id)? => {
                Some((MutationType::SetTicketContainer, new_id))
            }
            // Cleared, or moved to a Container the Backend cannot hold: the
            // Backend still has the parent tk just dropped.
            FieldEdit::Set(_) | FieldEdit::Clear => match current.container_id.as_deref() {
                Some(old_id) if containment_is_backend_intent(&tx, &binding, old_id)? => {
                    Some((MutationType::ClearTicketContainer, old_id))
                }
                _ => None,
            },
            FieldEdit::Unchanged => None,
        };
        if let Some((mutation_type, container_id)) = container_mutation {
            mutations::append(
                &tx,
                mutations::AppendRequest {
                    mutation_type,
                    item_id: req.id,
                    item_class: current.item_class,
                    payload: &MutationPayload::ContainerRef(ContainerRef {
                        container_id: container_id.to_owned(),
                    }),
                    promotion_operation_id: None,
                    now_iso: &now_iso,
                },
            )?;
        }
        if let Some(content) = req.content {
            mutations::append(
                &tx,
                mutations::AppendRequest {
                    mutation_type: current.item_class.update_mutation_type(),
                    item_id: req.id,
                    item_class: current.item_class,
                    payload: &MutationPayload::ContentEdit(content),
                    promotion_operation_id: None,
                    now_iso: &now_iso,
                },
            )?;
        }
        if let (true, priority) = new_priority.column() {
            mutations::append(
                &tx,
                mutations::AppendRequest {
                    mutation_type: MutationType::SetTicketPriority,
                    item_id: req.id,
                    item_class: current.item_class,
                    payload: &MutationPayload::Priority(PriorityChange { priority }),
                    promotion_operation_id: None,
                    now_iso: &now_iso,
                },
            )?;
        }
        if let (true, effort) = new_effort.column()
            && let BackendEffort::Supported(effort) =
                BackendEffort::for_backend(backend.parse()?, effort)
        {
            mutations::append(
                &tx,
                mutations::AppendRequest {
                    mutation_type: MutationType::SetTicketEffort,
                    item_id: req.id,
                    item_class: current.item_class,
                    payload: &MutationPayload::Effort(EffortChange { effort }),
                    promotion_operation_id: None,
                    now_iso: &now_iso,
                },
            )?;
        }
    }

    tx.commit()?;
    Ok(UpdatedItem {
        display_id: current.display_id,
        title,
        item_class: current.item_class,
    })
}

/// Whether a Containment change is backend intent: the Ticket and the
/// Container must both be backend-bound, to the *same* Backend.
///
/// The typed Containment operation carries both Backend identities, resolved
/// immediately before delivery. Appending intent only when both endpoints
/// already bind the same Backend ensures that resolution can succeed. This is
/// the same rule Promotion preflight applies to a Containment draft (ADR-0035)
/// and `dependency_rule` applies to an edge.
fn containment_is_backend_intent(
    conn: &rusqlite::Connection,
    ticket_binding: &BackendBinding,
    container_id: &str,
) -> Result<bool, mutations::BackendBindingError> {
    let container = mutations::resolve_backend_binding(conn, container_id)?;
    Ok(ticket_binding.backend_kind().is_some()
        && ticket_binding.backend_kind() == container.backend_kind())
}

fn write_columns(
    conn: &rusqlite::Connection,
    id: &str,
    writes: &ColumnWrites<'_>,
    now_iso: &str,
) -> Result<(), rusqlite::Error> {
    let (priority_changed, new_priority) = writes.priority.column();
    let (effort_changed, new_effort) = writes.effort.column();
    conn.execute(
        "update items set title = coalesce(?2, title), body = coalesce(?3, body), \
                          priority = case when ?8 then ?4 else priority end, \
                          effort = case when ?5 then ?6 else effort end, \
                          updated_at = ?7 \
          where id = ?1",
        params![
            id,
            writes.title,
            writes.body,
            new_priority,
            effort_changed,
            new_effort,
            now_iso,
            priority_changed
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, commit_promotion, insert_fixture_item, mutation_types,
    };
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store::for_test(conn)
    }

    fn seed_local_ticket(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: "Original",
                body: "Body0",
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_backend_ticket(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: "Original",
                body: "Body0",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some(display),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_backend_ticket_in(
        store: &Store,
        id: &str,
        display: &str,
        container_id: &str,
        created_seq: i64,
    ) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: "Ticket",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some(display),
                container_id: Some(container_id),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_epic(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_backend_epic(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some(display),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn clock() -> FakeClock {
        FakeClock::new(1_778_284_800_000)
    }

    #[test]
    fn update_title_and_body_emits_one_update_ticket_mutation_for_backend() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        let item = update_item(
            &mut store,
            &FakeClock::new(1_900_000_000_000),
            UpdateRequest {
                id: "t1",
                content: Some(ContentEdit::Both {
                    title: "New title".into(),
                    body: "New body".into(),
                }),
                ..UpdateRequest::default()
            },
        )
        .unwrap();
        assert_eq!(item.title, "New title");
        let (title, body, updated_at): (String, String, String) = store
            .conn
            .query_row(
                "select title, body, updated_at from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "New title");
        assert_eq!(body, "New body");
        assert_eq!(updated_at, "2030-03-17T17:46:40.000Z");

        let (mt, payload): (String, String) = store
            .conn
            .query_row(
                "select mutation_type, payload_json from mutations",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(mt, "update_ticket");
        assert_eq!(payload, r#"{"title":"New title","body":"New body"}"#);
    }

    #[test]
    fn local_update_writes_no_mutation() {
        let mut store = open_seeded();
        seed_local_ticket(&store, "t1", "tk-1", 1);

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                content: Some(ContentEdit::Title {
                    title: "Renamed".into(),
                }),
                ..UpdateRequest::default()
            },
        )
        .unwrap();
        let mutations: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0);
    }

    #[test]
    fn update_on_an_applying_promotion_appends_intent_behind_it() {
        let mut store = open_seeded();
        seed_local_ticket(&store, "t1", "tk-1", 1);
        commit_promotion(&mut store.conn, "t1");
        store
            .conn
            .execute(
                "update mutations set state = 'applying' where mutation_type = 'promote_ticket'",
                [],
            )
            .unwrap();

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                content: Some(ContentEdit::Title {
                    title: "Renamed while applying".into(),
                }),
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        assert_eq!(
            mutation_types(&store.conn).unwrap(),
            vec!["promote_ticket", "update_ticket"]
        );
    }

    #[test]
    fn update_on_a_pending_promotion_item_appends_intent_behind_the_promotion() {
        // ADR-0036: an edit made between `tk promote` and the next `tk sync`
        // reaches the Backend, ordered behind the Promotion that gives the
        // Item its backend identity.
        let mut store = open_seeded();
        seed_local_ticket(&store, "t1", "tk-1", 1);
        commit_promotion(&mut store.conn, "t1");

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                content: Some(ContentEdit::Title {
                    title: "Renamed".into(),
                }),
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        assert_eq!(
            mutation_types(&store.conn).unwrap(),
            vec!["promote_ticket", "update_ticket"]
        );
        let payload: String = store
            .conn
            .query_row(
                "select payload_json from mutations where mutation_type = 'update_ticket'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(payload, r#"{"title":"Renamed"}"#);
    }

    #[test]
    fn explicit_backend_content_edit_keeps_timestamp_when_local_values_match() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        update_item(
            &mut store,
            &FakeClock::new(1_900_000_000_000),
            UpdateRequest {
                id: "t1",
                content: Some(ContentEdit::Both {
                    title: "Original".into(),
                    body: "Body0".into(),
                }),
                ..UpdateRequest::default()
            },
        )
        .unwrap();
        let updated_at: String = store
            .conn
            .query_row("select updated_at from items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(updated_at, "2026-05-09T00:00:00.000Z");
        let payload: String = store
            .conn
            .query_row("select payload_json from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(payload, r#"{"title":"Original","body":"Body0"}"#);
    }

    #[test]
    fn priority_change_on_a_backend_ticket_emits_a_set_ticket_priority_mutation() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                priority: FieldEdit::Set(Priority::P0),
                ..UpdateRequest::default()
            },
        )
        .unwrap();
        let priority: String = store
            .conn
            .query_row("select priority from items where id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(priority, "P0");
        let (mutation_type, payload): (String, String) = store
            .conn
            .query_row(
                "select mutation_type, payload_json from mutations",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(mutation_type, "set_ticket_priority");
        assert_eq!(payload, r#"{"priority":"P0"}"#);
    }

    #[test]
    fn effort_changes_on_a_backend_ticket_emit_set_ticket_effort_mutations() {
        // A clear is a Mutation too, spelled `null`, so Apply can delete the
        // Backend value; a repeated clear changes nothing and emits nothing.
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);
        for effort in [
            FieldEdit::Set(Effort::Low),
            FieldEdit::Clear,
            FieldEdit::Clear,
        ] {
            update_item(
                &mut store,
                &clock(),
                UpdateRequest {
                    id: "t1",
                    effort,
                    ..UpdateRequest::default()
                },
            )
            .unwrap();
        }

        let effort: Option<String> = store
            .conn
            .query_row("select effort from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(effort, None);
        let mutations: Vec<(String, String)> = store
            .conn
            .prepare("select mutation_type, payload_json from mutations order by sequence")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            mutations,
            [
                ("set_ticket_effort".into(), r#"{"effort":"low"}"#.into()),
                ("set_ticket_effort".into(), r#"{"effort":null}"#.into()),
            ]
        );
    }

    #[test]
    fn priority_update_is_allowed_for_a_parked_ticket() {
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Held",
                priority: Some("P2"),
                selection_state: Some("parked"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                priority: FieldEdit::Set(Priority::P0),
                ..UpdateRequest::default()
            },
        )
        .expect("parked Ticket is reprioritizable");

        let (priority, selection): (String, String) = store
            .conn
            .query_row(
                "select priority, selection_state from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(priority, "P0");
        assert_eq!(selection, "parked", "reprioritizing must not unpark");
    }

    #[test]
    fn parent_move_emits_one_set_for_backend_ticket() {
        let mut store = open_seeded();
        seed_backend_epic(&store, "e-old", "tk-1", 1);
        seed_backend_epic(&store, "e-new", "tk-2", 2);
        seed_backend_ticket_in(&store, "t1", "tk-3", "e-old", 3);

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                parent: FieldEdit::Set("e-new"),
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        assert_eq!(
            container_of(&store, "t1"),
            (Some("e-new".into()), Some("epic".into()))
        );
        assert_eq!(
            mutation_rows(&store),
            vec![(
                "set_ticket_container".to_string(),
                r#"{"container_id":"e-new"}"#.to_string(),
            )]
        );
    }

    #[test]
    fn parent_clear_emits_clear_only() {
        let mut store = open_seeded();
        seed_backend_epic(&store, "e1", "tk-1", 1);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "t1",
                display: "tk-2",
                title: "Ticket",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("99"),
                container_id: Some("e1"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                parent: FieldEdit::Clear,
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        let container_id: Option<String> = store
            .conn
            .query_row("select container_id from items where id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(container_id.is_none());
        assert_eq!(
            mutation_types(&store.conn).unwrap(),
            vec!["clear_ticket_container"]
        );
    }

    #[test]
    fn unknown_id_returns_not_found() {
        let mut store = open_seeded();
        let err = update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "missing",
                content: Some(ContentEdit::Title { title: "X".into() }),
                ..UpdateRequest::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::NotFound));
    }

    #[test]
    fn clear_parent_is_idempotent_when_ticket_has_no_parent() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);
        update_item(
            &mut store,
            &FakeClock::new(1_900_000_000_000),
            UpdateRequest {
                id: "t1",
                parent: FieldEdit::Clear,
                ..UpdateRequest::default()
            },
        )
        .unwrap();
        let updated_at: String = store
            .conn
            .query_row("select updated_at from items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(updated_at, "2026-05-09T00:00:00.000Z");
        let mutations: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0);
    }

    #[test]
    fn combined_parent_move_and_title_change_emits_two_mutations_in_order() {
        let mut store = open_seeded();
        seed_backend_epic(&store, "e-old", "tk-1", 1);
        seed_backend_epic(&store, "e-new", "tk-2", 2);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "t1",
                display: "tk-3",
                title: "Original",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("99"),
                container_id: Some("e-old"),
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                content: Some(ContentEdit::Title {
                    title: "Renamed".into(),
                }),
                parent: FieldEdit::Set("e-new"),
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        let rows: Vec<String> = store
            .conn
            .prepare("select mutation_type from mutations order by sequence")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            rows,
            vec![
                "set_ticket_container".to_string(),
                "update_ticket".to_string(),
            ]
        );
    }

    #[test]
    fn parent_move_into_a_local_epic_emits_no_containment_mutation() {
        // A Containment Mutation names the Epic, and nothing resolves a backend
        // identity for it, so a Pending Promotion Ticket moved under a Local
        // Epic would hand the Adapter an unaddressable parent. Promotion
        // preflight applies the same both-endpoints rule (ADR-0035).
        let mut store = open_seeded();
        seed_epic(&store, "e1", "tk-1", 1);
        seed_local_ticket(&store, "t1", "tk-2", 2);
        commit_promotion(&mut store.conn, "t1");

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                parent: FieldEdit::Set("e1"),
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        let container_id: Option<String> = store
            .conn
            .query_row("select container_id from items where id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            container_id.as_deref(),
            Some("e1"),
            "the move is current state either way"
        );
        assert_eq!(mutation_types(&store.conn).unwrap(), vec!["promote_ticket"]);
    }

    #[test]
    fn parent_move_emits_containment_when_both_endpoints_share_a_backend() {
        let mut store = open_seeded();
        seed_backend_epic(&store, "e1", "gh-1", 1);
        seed_local_ticket(&store, "t1", "tk-2", 2);
        commit_promotion(&mut store.conn, "t1");

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                parent: FieldEdit::Set("e1"),
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        assert_eq!(
            mutation_types(&store.conn).unwrap(),
            vec!["promote_ticket", "set_ticket_container"],
            "the Containment is ordered behind the Promotion that addresses the Ticket"
        );
    }

    fn mutation_rows(store: &Store) -> Vec<(String, String)> {
        store
            .conn
            .prepare("select mutation_type, payload_json from mutations order by sequence")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn container_of(store: &Store, id: &str) -> (Option<String>, Option<String>) {
        store
            .conn
            .query_row(
                "select container_id, container_class from items where id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    #[test]
    fn moving_under_a_ticket_stores_its_class_and_emits_one_set() {
        let mut store = open_seeded();
        seed_backend_epic(&store, "e1", "gh-1", 1);
        seed_backend_ticket_in(&store, "t1", "gh-2", "e1", 2);
        seed_backend_ticket(&store, "holder", "gh-3", 3);

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                parent: FieldEdit::Set("holder"),
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        assert_eq!(
            container_of(&store, "t1"),
            (Some("holder".into()), Some("ticket".into()))
        );
        assert_eq!(
            mutation_rows(&store),
            vec![(
                "set_ticket_container".to_string(),
                r#"{"container_id":"holder"}"#.to_string(),
            )],
            "one set moves the Ticket; no clear of the old Epic precedes it"
        );
    }

    #[test]
    fn moving_to_a_container_the_backend_cannot_hold_clears_the_old_parent() {
        let mut store = open_seeded();
        seed_backend_epic(&store, "e1", "gh-1", 1);
        seed_backend_ticket_in(&store, "t1", "gh-2", "e1", 2);
        seed_local_ticket(&store, "local", "tk-3", 3);

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                parent: FieldEdit::Set("local"),
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        assert_eq!(
            container_of(&store, "t1"),
            (Some("local".into()), Some("ticket".into())),
            "the move is current state either way"
        );
        assert_eq!(
            mutation_rows(&store),
            vec![(
                "clear_ticket_container".to_string(),
                r#"{"container_id":"e1"}"#.to_string(),
            )],
            "GitHub still holds the old parent, so the clear is what converges it"
        );
    }

    #[test]
    fn a_refused_container_leaves_the_whole_update_unwritten() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "a", "gh-1", 1);
        seed_backend_ticket(&store, "b", "gh-2", 2);
        seed_backend_ticket(&store, "c", "gh-3", 3);
        let mv = |id, title: Option<&str>, parent| UpdateRequest {
            id,
            content: title.map(|title| ContentEdit::Title {
                title: title.into(),
            }),
            parent,
            ..UpdateRequest::default()
        };
        update_item(&mut store, &clock(), mv("a", None, FieldEdit::Set("b"))).unwrap();
        let queued = mutation_types(&store.conn).unwrap();
        assert_eq!(queued, vec!["set_ticket_container"], "a is held by b");

        // b holds a, so b cannot go under a Ticket; c cannot go under a, which
        // b holds; a Ticket cannot hold itself.
        for (id, parent) in [("b", "c"), ("c", "a"), ("c", "c")] {
            let err = update_item(
                &mut store,
                &clock(),
                mv(id, Some("Renamed"), FieldEdit::Set(parent)),
            )
            .unwrap_err();
            assert!(
                matches!(err, UpdateError::Containment(_)),
                "{id} under {parent}: {err:?}"
            );
        }

        assert_eq!(container_of(&store, "b"), (None, None));
        assert_eq!(container_of(&store, "c"), (None, None));
        let titles: Vec<String> = store
            .conn
            .prepare("select title from items where id in ('b', 'c')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(titles.iter().all(|t| t == "Original"), "{titles:?}");
        assert_eq!(mutation_types(&store.conn).unwrap(), queued);
    }

    #[test]
    fn epic_update_emits_update_epic_mutation() {
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "e1",
                display: "tk-1",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Original",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("10"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "e1",
                content: Some(ContentEdit::Both {
                    title: "Updated Epic".into(),
                    body: String::new(),
                }),
                ..UpdateRequest::default()
            },
        )
        .unwrap();
        let mt: String = store
            .conn
            .query_row("select mutation_type from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mt, "update_epic");
    }
}
