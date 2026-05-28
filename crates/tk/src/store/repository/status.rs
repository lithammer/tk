//! Item Status lifecycle write (`tk start` / `tk stop` / `tk done`).
//!
//! Backend-origin transitions append a `set_item_status` Mutation in the
//! same transaction; local-origin transitions only update current state.
//! Idempotent calls (already at the requested state) succeed without
//! bumping `updated_at` or writing a Mutation row.
//!
//! ADR-0006 makes `Done` terminal in v1. A pre-read short-circuit refuses
//! any request that would leave `Done`, returning [`SetStatusError::LockedDone`]
//! so callers can render a typed diagnostic before the
//! `items_no_escape_from_done` schema trigger fires. The trigger remains
//! as defence-in-depth for write paths that skip the pre-read.

use rusqlite::{OptionalExtension, params};

use crate::clock::Clock;
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{MutationPayload, StatusChange};
use crate::domain::mutation_type::MutationType;
use crate::domain::origin::Origin;
use crate::domain::status::ItemStatus;
use crate::store::mutations;

use super::Store;

/// Input for [`set_item_status`].
#[derive(Debug, Clone)]
pub struct SetStatusRequest<'a> {
    /// Internal stable `items.id` of the row to transition.
    pub id: &'a str,
    /// Target Item Status to persist.
    pub status: ItemStatus,
}

/// Snapshot returned on a successful lifecycle write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChangedItem {
    pub display_id: String,
    pub title: String,
    pub item_class: ItemClass,
    pub status: ItemStatus,
}

/// Why [`set_item_status`] did not commit a transition. Success is
/// `Ok(StatusChangedItem)` — a real transition or a no-op for an
/// already-current state. The miss variants render at exit 1; the `#[error]`
/// strings are internal — `tk start/stop/done` interpolate the id into their
/// own lines, and `tk worktree start` treats a miss as a best-effort no-op.
#[derive(Debug, thiserror::Error)]
pub enum SetStatusError {
    /// The requested `id` does not resolve to a live row.
    #[error("item not found")]
    NotFound,
    /// Refused: the item is already `Done` and the request is for any
    /// non-`Done` status. Carries the persisted [`ItemClass`] so callers
    /// can render "Ticket" vs "Epic" without a second round-trip.
    #[error("item is already done")]
    LockedDone(ItemClass),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Mutation(#[from] mutations::AppendError),
}

/// Apply a lifecycle Item Status transition to a Ticket or Epic.
pub fn set_item_status<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    req: SetStatusRequest<'_>,
) -> Result<StatusChangedItem, SetStatusError> {
    let now_iso = clock.now_iso();
    let tx = store.conn.transaction()?;

    let current = tx
        .query_row(
            "select origin, status, item_class, display_value, title \
               from items where id = ?1",
            params![req.id],
            |row| {
                Ok((
                    row.get::<_, Origin>(0)?,
                    row.get::<_, ItemStatus>(1)?,
                    row.get::<_, ItemClass>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((origin, current_status, item_class, display_id, title)) = current else {
        // No write happened — drop the transaction implicitly.
        return Err(SetStatusError::NotFound);
    };

    if current_status == ItemStatus::Done && req.status != ItemStatus::Done {
        tx.commit()?;
        return Err(SetStatusError::LockedDone(item_class));
    }

    if current_status == req.status {
        tx.commit()?;
        return Ok(StatusChangedItem {
            display_id,
            title,
            item_class,
            status: req.status,
        });
    }

    tx.execute(
        "update items set status = ?2, updated_at = ?3 where id = ?1",
        params![req.id, req.status, now_iso],
    )?;

    if origin == Origin::Backend {
        mutations::append(
            &tx,
            MutationType::SetItemStatus,
            req.id,
            item_class,
            &MutationPayload::ItemStatus(StatusChange {
                status: req.status.text().to_owned(),
            }),
            &now_iso,
        )?;
    }

    tx.commit()?;
    Ok(StatusChangedItem {
        display_id,
        title,
        item_class,
        status: req.status,
    })
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

    fn seed_open_ticket(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: "Ticket",
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
                title: "Ticket",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("1"),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_done_ticket(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: "Done Ticket",
                status: "done",
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
    fn sets_status_to_active_on_a_local_ticket() {
        let mut store = open_seeded();
        seed_open_ticket(&store, "t1", "tk-1", 1);

        let item = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Active,
            },
        )
        .unwrap();

        assert_eq!(item.status, ItemStatus::Active);
        assert_eq!(item.item_class, ItemClass::Ticket);

        let stored: String = store
            .conn
            .query_row("select status from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "active");

        let mutation_count: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutation_count, 0);
    }

    #[test]
    fn backend_transition_appends_set_item_status_mutation() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Active,
            },
        )
        .unwrap();

        let (mt, payload): (String, String) = store
            .conn
            .query_row(
                "select mutation_type, payload_json from mutations",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(mt, "set_item_status");
        assert_eq!(payload, r#"{"status":"active"}"#);
    }

    #[test]
    fn idempotent_call_does_not_write_mutation_or_change_timestamp() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        // Active → already at status `open` (default fixture). Asking for
        // open is a no-op.
        set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Open,
            },
        )
        .unwrap();

        let updated_at: String = store
            .conn
            .query_row("select updated_at from items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(updated_at, "2026-05-09T00:00:00.000Z");
        let mutation_count: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutation_count, 0);
    }

    #[test]
    fn reopening_a_done_item_returns_locked_done() {
        let mut store = open_seeded();
        seed_done_ticket(&store, "t1", "tk-1", 1);

        let err = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Open,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SetStatusError::LockedDone(ItemClass::Ticket)));

        let stored: String = store
            .conn
            .query_row("select status from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "done");
    }

    #[test]
    fn closing_a_done_item_is_allowed_and_idempotent() {
        let mut store = open_seeded();
        seed_done_ticket(&store, "t1", "tk-1", 1);

        let item = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Done,
            },
        )
        .unwrap();
        assert_eq!(item.status, ItemStatus::Done);
    }

    #[test]
    fn unknown_id_returns_not_found() {
        let mut store = open_seeded();
        let err = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "missing",
                status: ItemStatus::Active,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SetStatusError::NotFound));
    }
}
