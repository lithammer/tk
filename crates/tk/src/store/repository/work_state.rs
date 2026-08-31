//! Work State write (`tk start` / `tk stop`).
//!
//! Work State is a Local Field (ADR-0043): whether someone is working an Item
//! right now is never applied to a Backend and never recorded as a Mutation.
//! Two layers enforce that, and neither is enough alone. [`SetWorkStateError`]
//! removes the idiomatic path: with no Mutation arm, `append(..)?` does not
//! compile. It cannot remove every path — discarding the `Result` compiles
//! fine — so `a_backend_bound_item_records_no_mutation_for_either_direction`
//! closes the rest behaviourally. Do not read the type as making an append
//! impossible and retire that test as redundant; it is the half the type
//! cannot cover.
//!
//! Writing the other axis is [`super::status::close_item`], which does append.
//!
//! `done` stays terminal (ADR-0006): a closed Item refuses a Work State write
//! with [`SetWorkStateError::LockedDone`], which is what keeps `tk stop` on a
//! `done` Item the refusal it has always been even though it no longer touches
//! the Lifecycle column. An idempotent call — the target already stored —
//! succeeds without bumping `updated_at`.

use rusqlite::params;

use crate::clock::Clock;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::selection_state::SelectionState;
use crate::domain::work_state::WorkState;

use crate::domain::item_class::ItemClass;

use super::Store;
use super::status::{SetStatusError, StatusChangedItem, TransitionReadError, begin_transition};

/// Why [`set_work_state`] did not commit.
///
/// Narrower than [`SetStatusError`] on purpose: with no Mutation arm,
/// `mutations::append(..)?` in this module does not compile. That kills the
/// idiomatic path rather than every path — see the module doc for the
/// behavioural half. Every variant flattens onto the same-named variant of
/// [`SetStatusError`] for rendering.
#[derive(Debug, thiserror::Error)]
pub enum SetWorkStateError {
    /// The requested `id` does not resolve to a live row.
    #[error("item not found")]
    NotFound,
    /// Refused: the Item is `done`, and closed work is neither started nor
    /// stopped (ADR-0006). Carries the [`ItemClass`] so the caller renders
    /// "Ticket" vs "Epic" without a second round-trip.
    #[error("item is already done")]
    LockedDone(ItemClass),
    /// Refused: only `accepted` work becomes `active` (ADR-0029), so a
    /// `triage` Ticket must be accepted first.
    #[error("triage Ticket cannot be started")]
    TriageNotStartable,
    /// Refused: only `accepted` work becomes `active` (ADR-0029), so a
    /// `parked` Ticket must be unparked first.
    #[error("parked Ticket cannot be started")]
    ParkedNotStartable,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

impl From<TransitionReadError> for SetWorkStateError {
    fn from(err: TransitionReadError) -> Self {
        match err {
            TransitionReadError::NotFound => Self::NotFound,
            TransitionReadError::Sqlite(err) => Self::Sqlite(err),
        }
    }
}

impl From<SetWorkStateError> for SetStatusError {
    /// Flattening, not wrapping: `commands::item_status` renders one match
    /// over both writers, and a wrapping conversion would force it to
    /// destructure — forking the ADR-0017 byte contracts this shape exists to
    /// keep single-sourced. A variant added here and not there is a
    /// non-exhaustive match, so the two cannot drift silently.
    fn from(err: SetWorkStateError) -> Self {
        match err {
            SetWorkStateError::NotFound => Self::NotFound,
            SetWorkStateError::LockedDone(class) => Self::LockedDone(class),
            SetWorkStateError::TriageNotStartable => Self::TriageNotStartable,
            SetWorkStateError::ParkedNotStartable => Self::ParkedNotStartable,
            SetWorkStateError::Sqlite(err) => Self::Sqlite(err),
        }
    }
}

/// Move a Ticket or Epic to `target` Work State.
///
/// `id` is the resolved internal `items.id`. Writes `work_state` and
/// `updated_at` only, never the Lifecycle, and appends nothing.
pub fn set_work_state<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    id: &str,
    target: WorkState,
) -> Result<StatusChangedItem, SetWorkStateError> {
    let (tx, now_iso, row) = begin_transition(&mut store.conn, clock, id)?;

    // Done is terminal (ADR-0006): closed work is neither started nor stopped.
    // No write happened, so the transaction drops.
    if row.lifecycle == Lifecycle::Done {
        return Err(SetWorkStateError::LockedDone(row.item_class));
    }

    // Only `accepted` work becomes `active` (ADR-0029, relocated onto
    // `work_state` by ADR-0043): a `triage` or `parked` Ticket cannot be
    // started. An Epic carries no Selection State and is always startable. The
    // CHECK in migration 011 backstops any path that skips this guard.
    if target == WorkState::Active {
        match row.selection_state {
            Some(SelectionState::Triage) => return Err(SetWorkStateError::TriageNotStartable),
            Some(SelectionState::Parked) => return Err(SetWorkStateError::ParkedNotStartable),
            Some(SelectionState::Accepted) | None => {}
        }
    }

    if row.work_state == target {
        // Idempotent: no write, so `updated_at` is left untouched.
        tx.commit()?;
        return Ok(row.changed());
    }

    tx.execute(
        "update items set work_state = ?2, updated_at = ?3 where id = ?1",
        params![id, target.text(), now_iso],
    )?;

    tx.commit()?;
    Ok(row.changed())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::domain::item_class::ItemClass;
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, insert_fixture_item, item_axes, mutation_count};
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store::for_test(conn)
    }

    fn open_ticket<'a>(id: &'a str, display: &'a str) -> FixtureItem<'a> {
        FixtureItem {
            id,
            display,
            title: "Ticket",
            created_seq: 1,
            ..FixtureItem::default()
        }
    }

    fn backend_ticket<'a>(id: &'a str, display: &'a str) -> FixtureItem<'a> {
        FixtureItem {
            origin: "backend",
            backend_kind: Some("github"),
            backend_key: Some("1"),
            ..open_ticket(id, display)
        }
    }

    fn clock() -> FakeClock {
        FakeClock::new(1_778_284_800_000)
    }

    #[test]
    fn starting_a_local_ticket_writes_only_work_state() {
        let mut store = open_seeded();
        insert_fixture_item(&store.conn, open_ticket("t1", "tk-1")).unwrap();

        let item = set_work_state(&mut store, &clock(), "t1", WorkState::Active).unwrap();

        assert_eq!(item.item_class, ItemClass::Ticket);
        assert_eq!(
            item_axes(&store.conn, "t1").unwrap(),
            (Lifecycle::Open, WorkState::Active),
            "starting must move Work State and leave the Lifecycle alone"
        );
    }

    #[test]
    fn stopping_returns_the_item_to_idle() {
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                status: "active",
                ..open_ticket("t1", "tk-1")
            },
        )
        .unwrap();

        set_work_state(&mut store, &clock(), "t1", WorkState::Idle).unwrap();

        assert_eq!(
            item_axes(&store.conn, "t1").unwrap(),
            (Lifecycle::Open, WorkState::Idle)
        );
    }

    #[test]
    fn a_backend_bound_item_records_no_mutation_for_either_direction() {
        // Work State is never applied to a Backend (ADR-0043) — gh-52's
        // expressibility half. This module cannot append one at all; the test
        // pins the observable so a future writer added here is caught.
        let mut store = open_seeded();
        insert_fixture_item(&store.conn, backend_ticket("t1", "tk-1")).unwrap();

        set_work_state(&mut store, &clock(), "t1", WorkState::Active).unwrap();
        set_work_state(&mut store, &clock(), "t1", WorkState::Idle).unwrap();

        assert_eq!(mutation_count(&store.conn).unwrap(), 0);
    }

    #[test]
    fn an_already_active_item_is_started_idempotently() {
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                status: "active",
                ..backend_ticket("t1", "tk-1")
            },
        )
        .unwrap();

        // A later clock would prove a spurious updated_at bump if one happened.
        set_work_state(
            &mut store,
            &FakeClock::new(1_900_000_000_000),
            "t1",
            WorkState::Active,
        )
        .unwrap();

        assert_eq!(updated_at(&store), "2026-05-09T00:00:00.000Z");
        assert_eq!(mutation_count(&store.conn).unwrap(), 0);
    }

    #[test]
    fn an_already_idle_item_is_stopped_idempotently() {
        // The other direction of the same contract: `tk stop` on work that was
        // never started writes nothing at all.
        let mut store = open_seeded();
        insert_fixture_item(&store.conn, backend_ticket("t1", "tk-1")).unwrap();

        set_work_state(
            &mut store,
            &FakeClock::new(1_900_000_000_000),
            "t1",
            WorkState::Idle,
        )
        .unwrap();

        assert_eq!(updated_at(&store), "2026-05-09T00:00:00.000Z");
        assert_eq!(mutation_count(&store.conn).unwrap(), 0);
    }

    fn updated_at(store: &Store) -> String {
        store
            .conn
            .query_row("select updated_at from items", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn stopping_a_done_item_returns_locked_done() {
        // The user-facing half of ADR-0006 survives the split: `tk stop` on a
        // closed Item is still refused with "is done and cannot be reopened",
        // even though the write it now guards is a Work State one.
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                status: "done",
                ..open_ticket("t1", "tk-1")
            },
        )
        .unwrap();

        let err = set_work_state(&mut store, &clock(), "t1", WorkState::Idle).unwrap_err();
        assert!(matches!(
            err,
            SetWorkStateError::LockedDone(ItemClass::Ticket)
        ));

        assert_eq!(
            item_axes(&store.conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );
    }

    #[test]
    fn the_done_lock_is_checked_before_the_selection_guard() {
        // A closed triage Ticket is refused as done, not as triage: the guard
        // order decides which diagnostic the user reads, and `done` is the
        // terminal fact (ADR-0006).
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                status: "done",
                priority: None,
                selection_state: Some("triage"),
                ..open_ticket("t1", "tk-1")
            },
        )
        .unwrap();

        let err = set_work_state(&mut store, &clock(), "t1", WorkState::Active).unwrap_err();
        assert!(matches!(err, SetWorkStateError::LockedDone(_)));
    }

    #[test]
    fn starting_a_non_accepted_ticket_is_rejected_and_leaves_the_row_alone() {
        // Both halves of `active ⟹ accepted` (ADR-0029, relocated onto
        // `work_state`), asserted symmetrically: the refusal names the right
        // Selection State *and* writes nothing. A guard that returned the error
        // after the UPDATE would leave the CHECK as the only defence.
        for (selection, priority, expected) in
            [("triage", None, "triage"), ("parked", Some("P2"), "parked")]
        {
            let mut store = open_seeded();
            insert_fixture_item(
                &store.conn,
                FixtureItem {
                    priority,
                    selection_state: Some(selection),
                    ..open_ticket("t1", "tk-1")
                },
            )
            .unwrap();

            let err = set_work_state(&mut store, &clock(), "t1", WorkState::Active).unwrap_err();
            match (expected, &err) {
                ("triage", SetWorkStateError::TriageNotStartable)
                | ("parked", SetWorkStateError::ParkedNotStartable) => {}
                _ => panic!("a {selection} Ticket must refuse a start, got {err:?}"),
            }

            assert_eq!(
                item_axes(&store.conn, "t1").unwrap(),
                (Lifecycle::Open, WorkState::Idle),
                "a rejected start must not move Work State"
            );
        }
    }

    #[test]
    fn starting_an_accepted_ticket_still_succeeds() {
        // The start-guard must reject only non-accepted work.
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                selection_state: Some("accepted"),
                ..open_ticket("t1", "tk-1")
            },
        )
        .unwrap();

        set_work_state(&mut store, &clock(), "t1", WorkState::Active).unwrap();

        assert_eq!(
            item_axes(&store.conn, "t1").unwrap(),
            (Lifecycle::Open, WorkState::Active)
        );
    }

    #[test]
    fn starting_an_epic_succeeds_without_a_selection_state() {
        // Work State covers Epics too (ADR-0043), unlike Ticket-only Selection
        // State: the relocated conjunct's NULL arm must not read as a refusal.
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                ..open_ticket("e1", "tk-1")
            },
        )
        .unwrap();

        set_work_state(&mut store, &clock(), "e1", WorkState::Active).unwrap();

        assert_eq!(
            item_axes(&store.conn, "e1").unwrap(),
            (Lifecycle::Open, WorkState::Active)
        );
    }

    #[test]
    fn unknown_id_returns_not_found() {
        let mut store = open_seeded();
        let err = set_work_state(&mut store, &clock(), "missing", WorkState::Active).unwrap_err();
        assert!(matches!(err, SetWorkStateError::NotFound));
    }
}
