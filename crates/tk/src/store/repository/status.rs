//! Item Status close write (`tk done`).
//!
//! Closing is the only Lifecycle transition tk performs: this writer takes no
//! target, so a reopen is not constructible (ADR-0006 makes `done` terminal in
//! v1). It lands `status = 'done'` and clears Work State in the same statement,
//! which is what keeps a `(done, active)` row unreachable without asking a
//! CHECK to abort a write (ADR-0043). Closing a backend-bound Item appends a
//! `set_item_status` Mutation in the same transaction; a Local Item with no
//! Promotion intent only updates current state. An already-`done` Item closes
//! idempotently, without bumping `updated_at` or writing a Mutation row.
//!
//! The other axis is [`super::work_state`], which appends nothing; both share
//! the `begin_transition` preamble so the two reads cannot drift.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::clock::Clock;
use crate::domain::item_class::ItemClass;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::mutation_payload::{LifecycleChange, MutationPayload};
use crate::domain::mutation_type::MutationType;
use crate::domain::selection_state::SelectionState;
use crate::domain::work_state::WorkState;
use crate::store::mutations;

use super::Store;

/// Snapshot returned on a successful Item Status write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChangedItem {
    pub display_id: String,
    pub title: String,
    pub item_class: ItemClass,
}

/// Why an Item Status write did not commit a transition. Success is
/// `Ok(StatusChangedItem)` — a real transition or a no-op for an
/// already-current state. The miss variants render at exit 1; the `#[error]`
/// strings are internal — `tk start` / `tk stop` / `tk done` interpolate the id
/// into their own lines.
///
/// This is the COMMAND-FACING surface: `commands::item_status` calls both
/// writers, so every variant is reachable where it is matched, and the render
/// match holding the ADR-0017 byte-identity contracts stays single-sourced.
/// The Work State writer raises the narrower
/// [`super::work_state::SetWorkStateError`], which flattens into this one.
#[derive(Debug, thiserror::Error)]
pub enum SetStatusError {
    /// The requested `id` does not resolve to a live row.
    #[error("item not found")]
    NotFound,
    /// Refused: the item is already `Done`, and Work State is not writable on
    /// closed work. Carries the persisted [`ItemClass`] so callers can render
    /// "Ticket" vs "Epic" without a second round-trip. Produced only by
    /// [`super::work_state::set_work_state`], which raises its own variant —
    /// closing an already-closed Item is idempotent, not a refusal.
    #[error("item is already done")]
    LockedDone(ItemClass),
    /// Refused: a Closing Reason was supplied for an item already `Done`.
    /// A Closing Reason is set-once at the transition (ADR-0023); re-closing
    /// is not an amend path. Carries the [`ItemClass`] for diagnostics.
    #[error("item is already done")]
    AlreadyClosed(ItemClass),
    /// Refused: cannot start a `triage` Ticket — only `accepted` work becomes
    /// `active` (ADR-0029). The Ticket must be accepted first. Produced only by
    /// the [`WorkState::Active`] target.
    #[error("triage Ticket cannot be started")]
    TriageNotStartable,
    /// Refused: cannot start a `parked` Ticket — only `accepted` work becomes
    /// `active` (ADR-0029). The Ticket must be unparked first. Produced only by
    /// the [`WorkState::Active`] target.
    #[error("parked Ticket cannot be started")]
    ParkedNotStartable,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    BackendBinding(#[from] mutations::BackendBindingError),
    #[error(transparent)]
    Mutation(#[from] mutations::AppendError),
}

/// Why the shared preamble could not hand back a row.
///
/// Deliberately narrower than either writer's error: reading a row can fail
/// in exactly two ways, and giving the preamble its own type keeps each
/// writer's `?` from silently widening what that writer can raise.
#[derive(Debug, thiserror::Error)]
pub(super) enum TransitionReadError {
    #[error("item not found")]
    NotFound,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

impl From<TransitionReadError> for SetStatusError {
    /// Flattening, not wrapping: each variant maps onto the wide variant of
    /// the same name so the command's render match never has to destructure.
    fn from(err: TransitionReadError) -> Self {
        match err {
            TransitionReadError::NotFound => Self::NotFound,
            TransitionReadError::Sqlite(err) => Self::Sqlite(err),
        }
    }
}

/// Current state both Item Status writers decide from. Each reads the whole
/// row and consults its own subset: the close writer never looks at
/// `selection_state`, the Work State writer never at `lifecycle` beyond the
/// done lock.
pub(super) struct TransitionRow {
    pub(super) lifecycle: Lifecycle,
    pub(super) work_state: WorkState,
    pub(super) item_class: ItemClass,
    pub(super) display_id: String,
    pub(super) title: String,
    pub(super) selection_state: Option<SelectionState>,
}

impl TransitionRow {
    /// The snapshot a write or a no-op returns; built from the same pre-read
    /// either way, so an idempotent call reports what a transition would have.
    pub(super) fn changed(self) -> StatusChangedItem {
        StatusChangedItem {
            display_id: self.display_id,
            title: self.title,
            item_class: self.item_class,
        }
    }
}

/// Open the write transaction and read the row, sampling `now_iso` once.
///
/// Shared by both axes so their reads cannot drift, and so the `updated_at`
/// discipline stays in one place: the timestamp is sampled here but spent only
/// by a branch that actually writes, which is what keeps an idempotent call
/// from bumping the column. The helper takes no decision and appends no
/// Mutation — every guard belongs to the writer that owns its diagnostic.
///
/// A missing row leaves the transaction unwritten, so dropping it rolls back
/// nothing.
pub(super) fn begin_transition<'a, C: Clock + ?Sized>(
    conn: &'a mut Connection,
    clock: &C,
    id: &str,
) -> Result<(Transaction<'a>, String, TransitionRow), TransitionReadError> {
    let now_iso = clock.now_iso();
    let tx = crate::store::write_transaction(conn)?;
    let row = tx
        .query_row(
            "select status, work_state, item_class, display_value, title, selection_state \
               from items where id = ?1",
            params![id],
            |row| {
                Ok(TransitionRow {
                    lifecycle: row.get(0)?,
                    work_state: row.get(1)?,
                    item_class: row.get(2)?,
                    display_id: row.get(3)?,
                    title: row.get(4)?,
                    selection_state: row.get(5)?,
                })
            },
        )
        .optional()?;
    match row {
        Some(row) => Ok((tx, now_iso, row)),
        None => Err(TransitionReadError::NotFound),
    }
}

/// Close a Ticket or Epic, optionally recording a Closing Reason (ADR-0023).
///
/// There is no target parameter: `done` is the only Lifecycle value this
/// writer can land, so no caller can ask for the reopen ADR-0006 forbids.
/// Work State is cleared alongside, because closed work is not in progress.
/// An already-`done` Item succeeds as a no-op unless a Closing Reason is
/// supplied, which is refused as [`SetStatusError::AlreadyClosed`].
pub fn close_item<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    id: &str,
    closing_reason: Option<&str>,
) -> Result<StatusChangedItem, SetStatusError> {
    let (tx, now_iso, row) = begin_transition(&mut store.conn, clock, id)?;

    if row.lifecycle == Lifecycle::Done {
        // Set-once (ADR-0023): a Closing Reason against an already-`done` item
        // is not an amend path. Re-closing without a reason stays the
        // idempotent no-op `tk done` relies on, so `updated_at` is untouched.
        if closing_reason.is_some() {
            return Err(SetStatusError::AlreadyClosed(row.item_class));
        }
        tx.commit()?;
        return Ok(row.changed());
    }

    // Work State is cleared in the same statement that lands `done`: a write
    // can decline to produce `(done, active)`, where a CHECK could only abort
    // the write after the fact (ADR-0043).
    tx.execute(
        "update items set status = ?2, work_state = ?3, closing_reason = ?4, updated_at = ?5 \
           where id = ?1",
        params![
            id,
            Lifecycle::Done.text(),
            WorkState::Idle.text(),
            closing_reason,
            now_iso,
        ],
    )?;

    // Backend Binding, not Origin, decides whether the close is also backend
    // intent (ADR-0036): a Pending Promotion Item's close is ordered behind the
    // Promotion that will give it a backend identity.
    if mutations::resolve_backend_binding(&tx, id)?.is_backend_bound() {
        mutations::append(
            &tx,
            mutations::AppendRequest {
                mutation_type: MutationType::SetItemStatus,
                item_id: id,
                item_class: row.item_class,
                payload: &MutationPayload::Lifecycle(LifecycleChange {
                    status: Lifecycle::Done,
                }),
                promotion_operation_id: None,
                now_iso: &now_iso,
            },
        )?;
    }

    tx.commit()?;
    Ok(row.changed())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, commit_promotion, insert_fixture_item, item_axes, mutation_count,
        mutation_types,
    };
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store::for_test(conn)
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

    fn seed_ticket_with_selection(
        store: &Store,
        id: &str,
        display: &str,
        selection: &str,
        priority: Option<&str>,
    ) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: "Subject",
                priority,
                selection_state: Some(selection),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn clock() -> FakeClock {
        FakeClock::new(1_778_284_800_000)
    }

    #[test]
    fn closing_a_local_ticket_writes_no_mutation() {
        let mut store = open_seeded();
        seed_open_ticket(&store, "t1", "tk-1", 1);

        let item = close_item(&mut store, &clock(), "t1", None).unwrap();

        assert_eq!(item.item_class, ItemClass::Ticket);
        assert_eq!(
            item_axes(&store.conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );

        assert_eq!(mutation_count(&store.conn).unwrap(), 0);
    }

    #[test]
    fn close_clears_work_state_on_an_active_item() {
        // The clear has no other witness: a `(done, active)` row derives
        // Item Status `done` at every carrier, so a close that forgot it would
        // render identically and break nothing until the row was read as two
        // axes. Only the read-back catches it.
        let mut store = open_seeded();
        seed_open_ticket(&store, "t1", "tk-1", 1);
        crate::store::repository::work_state::set_work_state(
            &mut store,
            &clock(),
            "t1",
            WorkState::Active,
        )
        .unwrap();

        close_item(&mut store, &clock(), "t1", None).unwrap();

        assert_eq!(
            item_axes(&store.conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle),
            "closing must clear Work State, not just land the Lifecycle"
        );
    }

    #[test]
    fn closing_a_backend_item_appends_a_set_item_status_mutation() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        close_item(&mut store, &clock(), "t1", None).unwrap();

        let (mt, payload): (String, String) = store
            .conn
            .query_row(
                "select mutation_type, payload_json from mutations",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(mt, "set_item_status");
        assert_eq!(payload, r#"{"status":"done"}"#);
    }

    #[test]
    fn pending_promotion_close_appends_set_item_status_behind_the_promotion() {
        // ADR-0036: `tk done` between `tk promote` and the next `tk sync` still
        // reaches the Backend, ordered behind the Promotion.
        let mut store = open_seeded();
        seed_open_ticket(&store, "t1", "tk-1", 1);
        commit_promotion(&mut store.conn, "t1");

        close_item(&mut store, &clock(), "t1", None).unwrap();

        assert_eq!(
            mutation_types(&store.conn).unwrap(),
            vec!["promote_ticket", "set_item_status"]
        );
        let payload: String = store
            .conn
            .query_row(
                "select payload_json from mutations where mutation_type = 'set_item_status'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(payload, r#"{"status":"done"}"#);
    }

    #[test]
    fn closing_a_done_item_is_allowed_and_idempotent() {
        let mut store = open_seeded();
        seed_done_ticket(&store, "t1", "tk-1", 1);
        let before: String = store
            .conn
            .query_row("select updated_at from items where id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();

        // A later clock would prove a spurious updated_at bump if one happened.
        let item = close_item(&mut store, &FakeClock::new(1_900_000_000_000), "t1", None).unwrap();
        // The no-op branch builds its snapshot from the same pre-read the
        // write branch uses; nothing else asserts the fields it returns.
        assert_eq!(item.display_id, "tk-1");
        assert_eq!(item.title, "Done Ticket");

        assert_eq!(
            item_axes(&store.conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );
        let updated_at: String = store
            .conn
            .query_row("select updated_at from items where id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(updated_at, before, "no-op must not bump updated_at");
    }

    #[test]
    fn closing_a_local_ticket_with_a_reason_persists_it_without_a_mutation() {
        let mut store = open_seeded();
        seed_open_ticket(&store, "t1", "tk-1", 1);

        close_item(&mut store, &clock(), "t1", Some("Fixed in PR #12")).unwrap();

        assert_eq!(
            item_axes(&store.conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );

        let stored_reason: Option<String> = store
            .conn
            .query_row(
                "select closing_reason from items where id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored_reason.as_deref(), Some("Fixed in PR #12"));

        // Closing Reason is a Local Field (ADR-0023): it never rides the
        // Mutation Log, not even for the status change on a backend item.
        assert_eq!(mutation_count(&store.conn).unwrap(), 0);
    }

    #[test]
    fn backend_close_with_a_reason_keeps_the_status_payload_unchanged() {
        let mut store = open_seeded();
        seed_backend_ticket(&store, "t1", "tk-1", 1);

        close_item(&mut store, &clock(), "t1", Some("Shipped")).unwrap();

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

        let err = close_item(&mut store, &clock(), "t1", Some("too late")).unwrap_err();
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

    #[test]
    fn closing_is_allowed_from_every_selection_state() {
        // `tk done` closes captured and held work without accepting it first:
        // the start-guard is the Work State writer's, not this one's.
        for (id, display, selection, priority) in [
            ("t1", "tk-1", "triage", None),
            ("t2", "tk-2", "parked", Some("P2")),
        ] {
            let mut store = open_seeded();
            seed_ticket_with_selection(&store, id, display, selection, priority);

            close_item(&mut store, &clock(), id, None).unwrap();

            assert_eq!(
                item_axes(&store.conn, id).unwrap(),
                (Lifecycle::Done, WorkState::Idle),
                "a {selection} Ticket must close"
            );
        }
    }

    #[test]
    fn unknown_id_returns_not_found() {
        let mut store = open_seeded();
        let err = close_item(&mut store, &clock(), "missing", None).unwrap_err();
        assert!(matches!(err, SetStatusError::NotFound));
    }
}
