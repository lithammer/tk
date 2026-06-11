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
use crate::domain::selection_state::SelectionState;
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
    /// Optional Closing Reason captured on the `→ done` transition (ADR-0023).
    /// A Local Field: persisted to `items.closing_reason`, never to the
    /// Mutation Log. Only `tk done -m` supplies it; `tk start`/`tk stop` pass
    /// `None`. Set-once — a reason against an already-`done` item is refused
    /// with [`SetStatusError::AlreadyClosed`].
    pub closing_reason: Option<&'a str>,
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
/// strings are internal — `tk start` / `tk stop` / `tk done` interpolate the id
/// into their own lines.
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
    /// Refused: a Closing Reason was supplied for an item already `Done`.
    /// A Closing Reason is set-once at the transition (ADR-0023); re-closing
    /// is not an amend path. Carries the [`ItemClass`] for diagnostics.
    #[error("item is already done")]
    AlreadyClosed(ItemClass),
    /// Refused: cannot start a `triage` Ticket — only `accepted` work becomes
    /// `active` (ADR-0029). The Ticket must be accepted first. Produced only by
    /// the `Active` target.
    #[error("triage Ticket cannot be started")]
    TriageNotStartable,
    /// Refused: cannot start a `parked` Ticket — only `accepted` work becomes
    /// `active` (ADR-0029). The Ticket must be unparked first. Produced only by
    /// the `Active` target.
    #[error("parked Ticket cannot be started")]
    ParkedNotStartable,
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
    let tx = crate::store::write_transaction(&mut store.conn)?;

    let current = tx
        .query_row(
            "select origin, status, item_class, display_value, title, selection_state \
               from items where id = ?1",
            params![req.id],
            |row| {
                Ok((
                    row.get::<_, Origin>(0)?,
                    row.get::<_, ItemStatus>(1)?,
                    row.get::<_, ItemClass>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<SelectionState>>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((origin, current_status, item_class, display_id, title, selection_state)) = current
    else {
        // No write happened — drop the transaction implicitly.
        return Err(SetStatusError::NotFound);
    };

    if current_status == ItemStatus::Done && req.status != ItemStatus::Done {
        tx.commit()?;
        return Err(SetStatusError::LockedDone(item_class));
    }

    // Only `accepted` work becomes `active` (ADR-0029): a `triage` or `parked`
    // Ticket cannot be started. No write happened, so the transaction drops.
    // The CHECK added in migration 006 backstops any path that skips this guard.
    if req.status == ItemStatus::Active {
        match selection_state {
            Some(SelectionState::Triage) => return Err(SetStatusError::TriageNotStartable),
            Some(SelectionState::Parked) => return Err(SetStatusError::ParkedNotStartable),
            Some(SelectionState::Accepted) | None => {}
        }
    }

    if current_status == req.status {
        // Set-once (ADR-0023): a Closing Reason against an already-`done`
        // item is not an amend path. Re-closing without a reason stays the
        // idempotent no-op `tk done`/`tk start`/`tk stop` rely on.
        if req.status == ItemStatus::Done && req.closing_reason.is_some() {
            tx.commit()?;
            return Err(SetStatusError::AlreadyClosed(item_class));
        }
        tx.commit()?;
        return Ok(StatusChangedItem {
            display_id,
            title,
            item_class,
            status: req.status,
        });
    }

    // `closing_reason` is unconditionally written: only the `→ done`
    // transition carries a reason, and a non-`done` target always pairs with
    // `None`, so a `start`/`stop` write simply re-nulls an already-null field.
    // The column CHECK confines a non-null reason to `done` items.
    tx.execute(
        "update items set status = ?2, closing_reason = ?3, updated_at = ?4 where id = ?1",
        params![req.id, req.status, req.closing_reason, now_iso],
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
                closing_reason: None,
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
                closing_reason: None,
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
                closing_reason: None,
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
                closing_reason: None,
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
                closing_reason: None,
            },
        )
        .unwrap();
        assert_eq!(item.status, ItemStatus::Done);
    }

    #[test]
    fn closing_a_local_ticket_with_a_reason_persists_it_without_a_mutation() {
        let mut store = open_seeded();
        seed_open_ticket(&store, "t1", "tk-1", 1);

        let item = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Done,
                closing_reason: Some("Fixed in PR #12"),
            },
        )
        .unwrap();
        assert_eq!(item.status, ItemStatus::Done);

        let stored: Option<String> = store
            .conn
            .query_row(
                "select closing_reason from items where id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored.as_deref(), Some("Fixed in PR #12"));

        // Closing Reason is a Local Field (ADR-0023): it never rides the
        // Mutation Log, not even for the status change on a backend item.
        let mutation_count: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutation_count, 0);
    }

    #[test]
    fn backend_close_with_a_reason_keeps_the_status_payload_unchanged() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Done,
                closing_reason: Some("Shipped"),
            },
        )
        .unwrap();

        // The set_item_status Mutation still fires for a Backend item, but the
        // Closing Reason stays out of its payload — sync deferred to tk-109.
        let payload: String = store
            .conn
            .query_row("select payload_json from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(payload, r#"{"status":"done"}"#);

        let stored: Option<String> = store
            .conn
            .query_row(
                "select closing_reason from items where id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored.as_deref(), Some("Shipped"));
    }

    #[test]
    fn closing_an_already_done_item_with_a_reason_is_refused() {
        let mut store = open_seeded();
        seed_done_ticket(&store, "t1", "tk-1", 1);

        let err = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Done,
                closing_reason: Some("too late"),
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SetStatusError::AlreadyClosed(ItemClass::Ticket)
        ));

        // The refusal must not mutate the row.
        let stored: Option<String> = store
            .conn
            .query_row(
                "select closing_reason from items where id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, None);
    }

    fn seed_ticket_with_selection(
        store: &Store,
        id: &str,
        display: &str,
        status: &str,
        selection: &str,
        priority: Option<&str>,
    ) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: "Subject",
                status,
                priority,
                selection_state: Some(selection),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn starting_a_triage_ticket_is_rejected() {
        let mut store = open_seeded();
        seed_ticket_with_selection(&store, "t1", "tk-1", "open", "triage", None);

        let err = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Active,
                closing_reason: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SetStatusError::TriageNotStartable));

        let stored: String = store
            .conn
            .query_row("select status from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "open", "rejected start must not transition");
    }

    #[test]
    fn starting_a_parked_ticket_is_rejected() {
        let mut store = open_seeded();
        seed_ticket_with_selection(&store, "t1", "tk-1", "open", "parked", Some("P2"));

        let err = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Active,
                closing_reason: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SetStatusError::ParkedNotStartable));
    }

    #[test]
    fn starting_an_accepted_ticket_still_succeeds() {
        // The start-guard must reject only non-accepted work; accepted Tickets
        // start as before.
        let mut store = open_seeded();
        seed_ticket_with_selection(&store, "t1", "tk-1", "open", "accepted", Some("P2"));

        let item = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Active,
                closing_reason: None,
            },
        )
        .unwrap();
        assert_eq!(item.status, ItemStatus::Active);
    }

    #[test]
    fn done_is_allowed_from_triage_and_stays_terminal() {
        // tk-76 AC: `done` closes captured work from any Selection State, and
        // the v1 terminal rule still holds afterward.
        let mut store = open_seeded();
        seed_ticket_with_selection(&store, "t1", "tk-1", "open", "triage", None);

        let item = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Done,
                closing_reason: None,
            },
        )
        .unwrap();
        assert_eq!(item.status, ItemStatus::Done);

        // Terminal: a subsequent start is refused as done, not as triage.
        let err = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Active,
                closing_reason: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SetStatusError::LockedDone(_)));
    }

    #[test]
    fn done_is_allowed_from_parked_and_stays_terminal() {
        let mut store = open_seeded();
        seed_ticket_with_selection(&store, "t1", "tk-1", "open", "parked", Some("P2"));

        let item = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Done,
                closing_reason: None,
            },
        )
        .unwrap();
        assert_eq!(item.status, ItemStatus::Done);

        let err = set_item_status(
            &mut store,
            &clock(),
            SetStatusRequest {
                id: "t1",
                status: ItemStatus::Open,
                closing_reason: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SetStatusError::LockedDone(_)));
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
                closing_reason: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SetStatusError::NotFound));
    }
}
