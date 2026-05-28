//! `tk update` field-level write (title / body / priority / parent move).
//!
//! Diffs each requested field against the stored value inside the same
//! `begin immediate` transaction the read used, writes only the changed
//! columns, and emits one Mutation per changed field-group (title/body
//! becomes a single full snapshot; parent change is `remove_ticket_from_epic`
//! plus `add_ticket_to_epic` in that order; priority is a stored change
//! only — Backend Adapters do not consume Priority).
//!
//! `updated_at` is bumped only when at least one column actually changes;
//! a request whose values all match the stored state commits a no-op
//! transaction and returns the same snapshot the caller already had.

use rusqlite::params;

use crate::clock::Clock;
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{EpicRef, MutationPayload, TitleBody};
use crate::domain::mutation_type::MutationType;
use crate::domain::origin::Origin;
use crate::domain::priority::Priority;
use crate::store::mutations;

use super::{Store, origin_from_text};

/// Parent-Epic operation requested by the caller.
#[derive(Debug, Clone, Copy, Default)]
pub enum ParentOp<'a> {
    /// Leave `container_id` as-is; no parent Mutation is emitted.
    #[default]
    Unchanged,
    /// Set `container_id` to NULL (remove from current Epic).
    Clear,
    /// Set `container_id` to the given internal stable `items.id`.
    Set(&'a str),
}

/// Input for [`update_item`].
///
/// `item_class` drives the `update_ticket` vs `update_epic` Mutation
/// discriminator, so the [`Default`] impl picks [`ItemClass::Ticket`]
/// (the dominant case) — callers updating an Epic MUST set this field
/// explicitly. The command layer (tk-91) is the dedicated caller and is
/// responsible for forwarding the resolved class through.
#[derive(Debug, Clone, Default)]
pub struct UpdateRequest<'a> {
    /// Internal stable `items.id` of the row to update.
    pub id: &'a str,
    /// Item Class of the resolved row — drives the Mutation discriminator
    /// for the title/body snapshot.
    pub item_class: ItemClass,
    pub title: Option<&'a str>,
    pub body: Option<&'a str>,
    /// New Priority for Tickets, or `None` to leave unchanged. The caller
    /// is responsible for ensuring this is `None` when `item_class` is
    /// [`ItemClass::Epic`] — Epics have no Priority column.
    pub priority: Option<Priority>,
    pub parent: ParentOp<'a>,
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
    /// `id` does not resolve to a live row.
    #[error("item not found")]
    NotFound,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Mutation(#[from] mutations::AppendError),
}

impl ItemClass {
    fn update_mutation_type(self) -> MutationType {
        match self {
            Self::Ticket => MutationType::UpdateTicket,
            Self::Epic => MutationType::UpdateEpic,
        }
    }
}

struct Current {
    origin: Origin,
    title: String,
    body: String,
    priority: Option<String>,
    container_id: Option<String>,
}

/// Apply per-field updates to a Ticket or Epic.
pub fn update_item<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    req: UpdateRequest<'_>,
) -> Result<UpdatedItem, UpdateError> {
    let now_iso = clock.now_iso();
    let tx = store.conn.transaction()?;

    let current = tx
        .query_row(
            "select origin, title, body, priority, container_id \
               from items where id = ?1",
            params![req.id],
            |r| {
                let origin: String = r.get(0)?;
                let title: String = r.get(1)?;
                let body: String = r.get(2)?;
                let priority: Option<String> = r.get(3)?;
                let container_id: Option<String> = r.get(4)?;
                Ok(Current {
                    origin: origin_from_text(&origin),
                    title,
                    body,
                    priority,
                    container_id,
                })
            },
        )
        .ok();
    let Some(current) = current else {
        return Err(UpdateError::NotFound);
    };

    let new_title: Option<&str> = req
        .title
        .filter(|requested| *requested != current.title.as_str());
    let new_body: Option<&str> = req
        .body
        .filter(|requested| *requested != current.body.as_str());
    let new_priority: Option<Priority> = req.priority.filter(|requested| {
        current
            .priority
            .as_deref()
            .is_none_or(|stored| stored != requested.text())
    });

    // Parent delta: derive old_epic_id (for remove_ticket_from_epic) and
    // new_epic_id (for add_ticket_to_epic).
    let (old_epic_id, new_epic_id, parent_changed) = match req.parent {
        ParentOp::Unchanged => (None, None, false),
        ParentOp::Clear => {
            if current.container_id.is_some() {
                (current.container_id.as_deref(), None, true)
            } else {
                (None, None, false)
            }
        }
        ParentOp::Set(target) => match current.container_id.as_deref() {
            Some(existing) if existing == target => (None, None, false),
            existing => (existing, Some(target), true),
        },
    };

    let title_or_body_changed = new_title.is_some() || new_body.is_some();
    let any_change = title_or_body_changed || new_priority.is_some() || parent_changed;

    if !any_change {
        let snap = tx
            .query_row(
                "select display_value, title from items where id = ?1",
                params![req.id],
                |r| {
                    let display: String = r.get(0)?;
                    let title: String = r.get(1)?;
                    Ok((display, title))
                },
            )
            .ok();
        let Some((display_id, title)) = snap else {
            return Err(UpdateError::NotFound);
        };
        tx.commit()?;
        return Ok(UpdatedItem {
            display_id,
            title,
            item_class: req.item_class,
        });
    }

    let effective_title: &str = new_title.unwrap_or(&current.title);
    let effective_body: &str = new_body.unwrap_or(&current.body);

    write_columns(
        &tx,
        req.id,
        effective_title,
        effective_body,
        new_priority,
        parent_changed,
        new_epic_id,
        &now_iso,
    )?;

    if current.origin == Origin::Backend {
        if let Some(old_id) = old_epic_id {
            mutations::append(
                &tx,
                MutationType::RemoveTicketFromEpic,
                req.id,
                req.item_class,
                &MutationPayload::EpicRef(EpicRef {
                    epic_id: old_id.to_owned(),
                }),
                &now_iso,
            )?;
        }
        if let Some(new_id) = new_epic_id {
            mutations::append(
                &tx,
                MutationType::AddTicketToEpic,
                req.id,
                req.item_class,
                &MutationPayload::EpicRef(EpicRef {
                    epic_id: new_id.to_owned(),
                }),
                &now_iso,
            )?;
        }
        if title_or_body_changed {
            mutations::append(
                &tx,
                req.item_class.update_mutation_type(),
                req.id,
                req.item_class,
                &MutationPayload::UpdateTitleBody(TitleBody {
                    title: effective_title.to_owned(),
                    body: effective_body.to_owned(),
                }),
                &now_iso,
            )?;
        }
    }

    let snap = tx
        .query_row(
            "select display_value, title from items where id = ?1",
            params![req.id],
            |r| {
                let display: String = r.get(0)?;
                let title: String = r.get(1)?;
                Ok((display, title))
            },
        )
        .ok();
    let Some((display_id, title)) = snap else {
        return Err(UpdateError::NotFound);
    };
    tx.commit()?;
    Ok(UpdatedItem {
        display_id,
        title,
        item_class: req.item_class,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_columns(
    conn: &rusqlite::Connection,
    id: &str,
    title: &str,
    body: &str,
    priority: Option<Priority>,
    parent_changed: bool,
    new_epic_id: Option<&str>,
    now_iso: &str,
) -> Result<(), rusqlite::Error> {
    let new_container_class: Option<&str> = new_epic_id.map(|_| "epic");
    if parent_changed {
        if let Some(p) = priority {
            conn.execute(
                "update items set title = ?2, body = ?3, priority = ?4, \
                                  container_id = ?5, container_class = ?6, updated_at = ?7 \
                  where id = ?1",
                params![
                    id,
                    title,
                    body,
                    p.text(),
                    new_epic_id,
                    new_container_class,
                    now_iso
                ],
            )?;
        } else {
            conn.execute(
                "update items set title = ?2, body = ?3, \
                                  container_id = ?4, container_class = ?5, updated_at = ?6 \
                  where id = ?1",
                params![id, title, body, new_epic_id, new_container_class, now_iso],
            )?;
        }
    } else if let Some(p) = priority {
        conn.execute(
            "update items set title = ?2, body = ?3, priority = ?4, updated_at = ?5 \
              where id = ?1",
            params![id, title, body, p.text(), now_iso],
        )?;
    } else {
        conn.execute(
            "update items set title = ?2, body = ?3, updated_at = ?4 where id = ?1",
            params![id, title, body, now_iso],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, insert_fixture_item};
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store { conn }
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

    fn clock() -> FakeClock {
        FakeClock::new(1_778_284_800_000)
    }

    #[test]
    fn update_title_and_body_emits_one_update_ticket_mutation_for_backend() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        let item = update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                item_class: ItemClass::Ticket,
                title: Some("New title"),
                body: Some("New body"),
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
        assert_eq!(updated_at, "2026-05-09T00:00:00.000Z");

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
                item_class: ItemClass::Ticket,
                title: Some("Renamed"),
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
    fn no_op_update_does_not_bump_updated_at() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                item_class: ItemClass::Ticket,
                title: Some("Original"),
                body: Some("Body0"),
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
    fn priority_change_does_not_emit_mutation_even_on_backend_ticket() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "t1",
                item_class: ItemClass::Ticket,
                priority: Some(Priority::P0),
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
        let mutations: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0);
    }

    #[test]
    fn parent_move_emits_remove_then_add_for_backend_ticket() {
        let mut store = open_seeded();
        seed_epic(&store, "e-old", "tk-1", 1);
        seed_epic(&store, "e-new", "tk-2", 2);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "t1",
                display: "tk-3",
                title: "Ticket",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("99"),
                container_id: Some("e-old"),
                container_class: Some("epic"),
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
                item_class: ItemClass::Ticket,
                parent: ParentOp::Set("e-new"),
                ..UpdateRequest::default()
            },
        )
        .unwrap();

        let (container_id, container_class): (Option<String>, Option<String>) = store
            .conn
            .query_row(
                "select container_id, container_class from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(container_id.as_deref(), Some("e-new"));
        assert_eq!(container_class.as_deref(), Some("epic"));

        let mutation_rows: Vec<(String, String)> = store
            .conn
            .prepare("select mutation_type, payload_json from mutations order by sequence")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            mutation_rows,
            vec![
                (
                    "remove_ticket_from_epic".to_string(),
                    r#"{"epic_id":"e-old"}"#.to_string(),
                ),
                (
                    "add_ticket_to_epic".to_string(),
                    r#"{"epic_id":"e-new"}"#.to_string(),
                ),
            ]
        );
    }

    #[test]
    fn parent_clear_emits_remove_only() {
        let mut store = open_seeded();
        seed_epic(&store, "e1", "tk-1", 1);
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
                container_class: Some("epic"),
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
                item_class: ItemClass::Ticket,
                parent: ParentOp::Clear,
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
        let mt: String = store
            .conn
            .query_row("select mutation_type from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mt, "remove_ticket_from_epic");
    }

    #[test]
    fn unknown_id_returns_not_found() {
        let mut store = open_seeded();
        let err = update_item(
            &mut store,
            &clock(),
            UpdateRequest {
                id: "missing",
                item_class: ItemClass::Ticket,
                title: Some("X"),
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
            &clock(),
            UpdateRequest {
                id: "t1",
                item_class: ItemClass::Ticket,
                parent: ParentOp::Clear,
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
    fn combined_parent_move_and_title_change_emits_three_mutations_in_order() {
        let mut store = open_seeded();
        seed_epic(&store, "e-old", "tk-1", 1);
        seed_epic(&store, "e-new", "tk-2", 2);
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
                container_class: Some("epic"),
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
                item_class: ItemClass::Ticket,
                title: Some("Renamed"),
                parent: ParentOp::Set("e-new"),
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
                "remove_ticket_from_epic".to_string(),
                "add_ticket_to_epic".to_string(),
                "update_ticket".to_string(),
            ]
        );
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
                item_class: ItemClass::Epic,
                title: Some("Updated Epic"),
                body: Some(""),
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
