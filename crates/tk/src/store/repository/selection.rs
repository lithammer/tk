//! Selection State transitions: `tk accept`, `tk park`, and `tk unpark`.
//!
//! Selection State is a Local Field (ADR-0027): these transitions update
//! current state only and never append a Mutation, even for a Backend Ticket,
//! and they leave Dependencies and External Blockers untouched, so accepting a
//! blocked triage Ticket keeps it blocked. `accept` and `unpark` touch only
//! `selection_state` / `priority`; `park` additionally reads `status` to refuse
//! an `active` Ticket, upholding the `active ⟹ accepted` invariant (ADR-0029).

use rusqlite::{OptionalExtension, params};

use crate::clock::Clock;
use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::selection_state::SelectionState;
use crate::domain::status::ItemStatus;

use super::Store;

/// Outcome of a successful [`accept_ticket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptOutcome {
    /// A triage Ticket moved to accepted at the supplied Priority.
    Accepted {
        display_id: String,
        title: String,
        priority: Priority,
    },
    /// The Ticket was already accepted and no Priority was supplied — an
    /// idempotent no-op that does not touch `updated_at`.
    AlreadyAccepted { display_id: String },
}

/// Why [`accept_ticket`] could not accept the item. Each arm renders a curated
/// `tk accept` diagnostic; the command interpolates the user's id.
#[derive(Debug, thiserror::Error)]
pub enum AcceptError {
    /// The id resolved at the command layer but the row was gone by the time
    /// the write transaction opened (a race), or never existed.
    #[error("item not found")]
    NotFound,
    /// The target is an Epic; Selection State applies to Tickets only.
    #[error("item is an Epic")]
    NotATicket,
    /// The Ticket is in triage and no Priority was supplied; acceptance must
    /// rank the work.
    #[error("triage Ticket needs a Priority")]
    PriorityRequired,
    /// The Ticket is already accepted and a Priority was supplied; Priority
    /// changes go through `tk update`, not re-acceptance.
    #[error("accepted Ticket cannot be reprioritized via accept")]
    PriorityOnAccepted,
    /// The Ticket is parked; it must be unparked rather than accepted.
    #[error("Ticket is parked")]
    Parked,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Accept a Ticket: move it from triage to accepted with a Priority, or
/// confirm an already-accepted Ticket as an idempotent no-op.
///
/// `id` is the resolved internal `items.id` (the command resolves the Display
/// ID / Alias first). `priority` is the requested `--priority`, if any.
pub fn accept_ticket<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    id: &str,
    priority: Option<Priority>,
) -> Result<AcceptOutcome, AcceptError> {
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let row: Option<(String, String, ItemClass, Option<SelectionState>)> = tx
        .query_row(
            "select display_value, title, item_class, selection_state from items where id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((display_id, title, item_class, selection_state)) = row else {
        return Err(AcceptError::NotFound);
    };

    if item_class == ItemClass::Epic {
        return Err(AcceptError::NotATicket);
    }

    // A Ticket always carries a Selection State post-rebuild (ADR-0028); a NULL
    // would be store corruption, treated like the Epic case rather than a panic.
    match selection_state {
        Some(SelectionState::Triage) => {
            let Some(priority) = priority else {
                return Err(AcceptError::PriorityRequired);
            };
            tx.execute(
                "update items set selection_state = 'accepted', priority = ?2, updated_at = ?3 \
                 where id = ?1",
                params![id, priority.text(), clock.now_iso()],
            )?;
            tx.commit()?;
            Ok(AcceptOutcome::Accepted {
                display_id,
                title,
                priority,
            })
        }
        Some(SelectionState::Accepted) => {
            if priority.is_some() {
                return Err(AcceptError::PriorityOnAccepted);
            }
            // Idempotent: no write, so `updated_at` is left untouched.
            Ok(AcceptOutcome::AlreadyAccepted { display_id })
        }
        Some(SelectionState::Parked) => Err(AcceptError::Parked),
        None => Err(AcceptError::NotATicket),
    }
}

/// Current-state columns the park/unpark transitions inspect before deciding
/// whether a Selection State change is allowed. Both read the same row shape,
/// so a single typed reader keeps the two transitions in lockstep.
struct SelectionRow {
    display_id: String,
    title: String,
    item_class: ItemClass,
    selection_state: Option<SelectionState>,
    priority: Option<Priority>,
    /// Item Status, read so `park_ticket` can refuse an `active` Ticket
    /// (ADR-0029). `unpark_ticket` ignores it — unparking is always
    /// invariant-safe.
    status: ItemStatus,
}

/// Read the [`SelectionRow`] for `id` within an open write transaction, or
/// `None` when the row vanished between command-layer resolution and the lock.
fn read_selection_row(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
) -> rusqlite::Result<Option<SelectionRow>> {
    tx.query_row(
        "select display_value, title, item_class, selection_state, priority, status \
         from items where id = ?1",
        params![id],
        |r| {
            Ok(SelectionRow {
                display_id: r.get(0)?,
                title: r.get(1)?,
                item_class: r.get(2)?,
                selection_state: r.get(3)?,
                priority: r.get(4)?,
                status: r.get(5)?,
            })
        },
    )
    .optional()
}

/// Outcome of a successful [`park_ticket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkOutcome {
    /// An accepted Ticket moved to parked; its Priority is preserved and echoed
    /// so the caller can confirm the held work stays ranked.
    Parked {
        display_id: String,
        title: String,
        priority: Priority,
    },
    /// The Ticket was already parked — an idempotent no-op that does not touch
    /// `updated_at`.
    AlreadyParked { display_id: String },
}

/// Why [`park_ticket`] could not park the item. Each arm renders a curated
/// `tk park` diagnostic; the command interpolates the user's id.
#[derive(Debug, thiserror::Error)]
pub enum ParkError {
    /// The id resolved at the command layer but the row was gone by the time
    /// the write transaction opened (a race), or never existed.
    #[error("item not found")]
    NotFound,
    /// The target is an Epic; Selection State applies to Tickets only.
    #[error("item is an Epic")]
    NotATicket,
    /// The Ticket is in triage; it must be accepted (which assigns a Priority)
    /// before it can be parked.
    #[error("triage Ticket must be accepted first")]
    Triage,
    /// The Ticket is `active`; it must be stopped (`tk stop`) before it can be
    /// held, so `active ⟹ accepted` is preserved (ADR-0029).
    #[error("active Ticket must be stopped first")]
    Active,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Park a Ticket: move it from accepted to parked, preserving Priority, or
/// confirm an already-parked Ticket as an idempotent no-op.
///
/// Status-agnostic by design (ADR-0027): this touches `selection_state` only,
/// never `status`. Rejecting `tk park` on an active Ticket is tk-76's lifecycle
/// guard, not this transition's job. `id` is the resolved internal `items.id`.
pub fn park_ticket<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    id: &str,
) -> Result<ParkOutcome, ParkError> {
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let Some(SelectionRow {
        display_id,
        title,
        item_class,
        selection_state,
        priority,
        status,
    }) = read_selection_row(&tx, id)?
    else {
        return Err(ParkError::NotFound);
    };

    if item_class == ItemClass::Epic {
        return Err(ParkError::NotATicket);
    }

    // A Ticket always carries a Selection State post-rebuild (ADR-0028); a NULL
    // would be store corruption, treated like the Epic case rather than a panic.
    match selection_state {
        Some(SelectionState::Accepted) => {
            // Only `accepted` open work can be held: an `active` Ticket must be
            // stopped first, so `active ⟹ accepted` holds (ADR-0029). Door 2 of
            // the invariant; `tk start`'s guard is Door 1.
            if status == ItemStatus::Active {
                return Err(ParkError::Active);
            }
            // Accepted Tickets carry a Priority by the schema invariant
            // (ADR-0028); it is preserved across the hold so unparking returns
            // the work at the same rank. A NULL here would be store corruption,
            // mapped to NotATicket like the None arm below rather than a panic.
            let priority = priority.ok_or(ParkError::NotATicket)?;
            tx.execute(
                "update items set selection_state = 'parked', updated_at = ?2 where id = ?1",
                params![id, clock.now_iso()],
            )?;
            tx.commit()?;
            Ok(ParkOutcome::Parked {
                display_id,
                title,
                priority,
            })
        }
        Some(SelectionState::Parked) => {
            // Idempotent: no write, so `updated_at` is left untouched.
            Ok(ParkOutcome::AlreadyParked { display_id })
        }
        Some(SelectionState::Triage) => Err(ParkError::Triage),
        None => Err(ParkError::NotATicket),
    }
}

/// Outcome of a successful [`unpark_ticket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnparkOutcome {
    /// A parked Ticket moved back to accepted; its Priority is unchanged and
    /// echoed so the caller can confirm the rank it returns to the queue at.
    Unparked {
        display_id: String,
        title: String,
        priority: Priority,
    },
    /// The Ticket was already accepted — an idempotent no-op that does not touch
    /// `updated_at`.
    AlreadyAccepted { display_id: String },
}

/// Why [`unpark_ticket`] could not unpark the item. Each arm renders a curated
/// `tk unpark` diagnostic; the command interpolates the user's id.
#[derive(Debug, thiserror::Error)]
pub enum UnparkError {
    /// The id resolved at the command layer but the row was gone by the time
    /// the write transaction opened (a race), or never existed.
    #[error("item not found")]
    NotFound,
    /// The target is an Epic; Selection State applies to Tickets only.
    #[error("item is an Epic")]
    NotATicket,
    /// The Ticket is in triage; it must be accepted (which assigns a Priority)
    /// before it makes sense to talk about unparking.
    #[error("triage Ticket must be accepted first")]
    Triage,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Unpark a Ticket: move it from parked back to accepted, returning it to
/// automatic selection without changing its Priority, or confirm an
/// already-accepted Ticket as an idempotent no-op.
///
/// Mirrors [`park_ticket`]: status-agnostic, touches `selection_state` only,
/// never appends a Mutation (ADR-0027). `id` is the resolved internal
/// `items.id`.
pub fn unpark_ticket<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    id: &str,
) -> Result<UnparkOutcome, UnparkError> {
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let Some(SelectionRow {
        display_id,
        title,
        item_class,
        selection_state,
        priority,
        // `status` is read for `park_ticket`'s active guard; unparking is
        // invariant-safe at any status, so it is not consulted here.
        ..
    }) = read_selection_row(&tx, id)?
    else {
        return Err(UnparkError::NotFound);
    };

    if item_class == ItemClass::Epic {
        return Err(UnparkError::NotATicket);
    }

    // A Ticket always carries a Selection State post-rebuild (ADR-0028); a NULL
    // would be store corruption, treated like the Epic case rather than a panic.
    match selection_state {
        Some(SelectionState::Parked) => {
            // Parked Tickets carry a Priority by the schema invariant
            // (ADR-0028); it rides through unchanged so the work re-enters
            // selection at the same rank. A NULL here would be store corruption,
            // mapped to NotATicket like the None arm below rather than a panic.
            let priority = priority.ok_or(UnparkError::NotATicket)?;
            tx.execute(
                "update items set selection_state = 'accepted', updated_at = ?2 where id = ?1",
                params![id, clock.now_iso()],
            )?;
            tx.commit()?;
            Ok(UnparkOutcome::Unparked {
                display_id,
                title,
                priority,
            })
        }
        Some(SelectionState::Accepted) => {
            // Idempotent: no write, so `updated_at` is left untouched.
            Ok(UnparkOutcome::AlreadyAccepted { display_id })
        }
        Some(SelectionState::Triage) => Err(UnparkError::Triage),
        None => Err(UnparkError::NotATicket),
    }
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

    fn seed(store: &Store, id: &str, selection: &str, priority: Option<&str>) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display: id,
                title: id,
                priority,
                selection_state: Some(selection),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn selection_of(store: &Store, id: &str) -> (Option<String>, String, String) {
        store
            .conn
            .query_row(
                "select priority, selection_state, updated_at from items where id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    #[test]
    fn triage_with_priority_becomes_accepted() {
        let mut store = open_seeded();
        seed(&store, "t", "triage", None);
        let clock = FakeClock::new(1_778_284_800_000);
        let out = accept_ticket(&mut store, &clock, "t", Some(Priority::P1)).unwrap();
        assert_eq!(
            out,
            AcceptOutcome::Accepted {
                display_id: "t".into(),
                title: "t".into(),
                priority: Priority::P1
            }
        );
        let (priority, selection, _) = selection_of(&store, "t");
        assert_eq!(priority.as_deref(), Some("P1"));
        assert_eq!(selection, "accepted");
    }

    #[test]
    fn triage_without_priority_is_rejected() {
        let mut store = open_seeded();
        seed(&store, "t", "triage", None);
        let clock = FakeClock::new(1_778_284_800_000);
        assert!(matches!(
            accept_ticket(&mut store, &clock, "t", None),
            Err(AcceptError::PriorityRequired)
        ));
    }

    #[test]
    fn already_accepted_without_priority_is_idempotent_and_keeps_updated_at() {
        let mut store = open_seeded();
        seed(&store, "t", "accepted", Some("P2"));
        let before = selection_of(&store, "t").2;
        // A later clock would prove a spurious updated_at bump if one happened.
        let clock = FakeClock::new(1_900_000_000_000);
        let out = accept_ticket(&mut store, &clock, "t", None).unwrap();
        assert_eq!(
            out,
            AcceptOutcome::AlreadyAccepted {
                display_id: "t".into()
            }
        );
        assert_eq!(
            selection_of(&store, "t").2,
            before,
            "no-op must not bump updated_at"
        );
    }

    #[test]
    fn already_accepted_with_priority_is_rejected() {
        let mut store = open_seeded();
        seed(&store, "t", "accepted", Some("P2"));
        let clock = FakeClock::new(1_778_284_800_000);
        assert!(matches!(
            accept_ticket(&mut store, &clock, "t", Some(Priority::P0)),
            Err(AcceptError::PriorityOnAccepted)
        ));
    }

    #[test]
    fn parked_is_rejected() {
        let mut store = open_seeded();
        seed(&store, "t", "parked", Some("P2"));
        let clock = FakeClock::new(1_778_284_800_000);
        assert!(matches!(
            accept_ticket(&mut store, &clock, "t", None),
            Err(AcceptError::Parked)
        ));
    }

    #[test]
    fn epic_is_rejected() {
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "e",
                display: "e",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let clock = FakeClock::new(1_778_284_800_000);
        assert!(matches!(
            accept_ticket(&mut store, &clock, "e", Some(Priority::P1)),
            Err(AcceptError::NotATicket)
        ));
    }

    #[test]
    fn accepted_becomes_parked_preserving_priority() {
        let mut store = open_seeded();
        seed(&store, "t", "accepted", Some("P1"));
        let clock = FakeClock::new(1_900_000_000_000);
        let out = park_ticket(&mut store, &clock, "t").unwrap();
        assert_eq!(
            out,
            ParkOutcome::Parked {
                display_id: "t".into(),
                title: "t".into(),
                priority: Priority::P1
            }
        );
        let (priority, selection, _) = selection_of(&store, "t");
        assert_eq!(priority.as_deref(), Some("P1"));
        assert_eq!(selection, "parked");
    }

    #[test]
    fn already_parked_is_idempotent_and_keeps_updated_at() {
        let mut store = open_seeded();
        seed(&store, "t", "parked", Some("P2"));
        let before = selection_of(&store, "t").2;
        let clock = FakeClock::new(1_900_000_000_000);
        let out = park_ticket(&mut store, &clock, "t").unwrap();
        assert_eq!(
            out,
            ParkOutcome::AlreadyParked {
                display_id: "t".into()
            }
        );
        assert_eq!(
            selection_of(&store, "t").2,
            before,
            "no-op must not bump updated_at"
        );
    }

    #[test]
    fn parking_an_active_ticket_is_rejected() {
        // tk-76 Door 2: an accepted Ticket being worked must be stopped before
        // it can be held; parking it directly would violate `active ⟹ accepted`.
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "t",
                display: "t",
                title: "t",
                status: "active",
                priority: Some("P2"),
                selection_state: Some("accepted"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let clock = FakeClock::new(1_778_284_800_000);
        assert!(matches!(
            park_ticket(&mut store, &clock, "t"),
            Err(ParkError::Active)
        ));
        let selection: String = store
            .conn
            .query_row(
                "select selection_state from items where id = 't'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(selection, "accepted", "rejected park must not change state");
    }

    #[test]
    fn parking_triage_is_rejected() {
        let mut store = open_seeded();
        seed(&store, "t", "triage", None);
        let clock = FakeClock::new(1_778_284_800_000);
        assert!(matches!(
            park_ticket(&mut store, &clock, "t"),
            Err(ParkError::Triage)
        ));
    }

    #[test]
    fn parking_an_epic_is_rejected() {
        let mut store = open_seeded();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "e",
                display: "e",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let clock = FakeClock::new(1_778_284_800_000);
        assert!(matches!(
            park_ticket(&mut store, &clock, "e"),
            Err(ParkError::NotATicket)
        ));
    }

    #[test]
    fn parked_becomes_accepted_preserving_priority() {
        let mut store = open_seeded();
        seed(&store, "t", "parked", Some("P1"));
        let clock = FakeClock::new(1_900_000_000_000);
        let out = unpark_ticket(&mut store, &clock, "t").unwrap();
        assert_eq!(
            out,
            UnparkOutcome::Unparked {
                display_id: "t".into(),
                title: "t".into(),
                priority: Priority::P1
            }
        );
        let (priority, selection, _) = selection_of(&store, "t");
        assert_eq!(priority.as_deref(), Some("P1"));
        assert_eq!(selection, "accepted");
    }

    #[test]
    fn already_accepted_unpark_is_idempotent_and_keeps_updated_at() {
        let mut store = open_seeded();
        seed(&store, "t", "accepted", Some("P2"));
        let before = selection_of(&store, "t").2;
        let clock = FakeClock::new(1_900_000_000_000);
        let out = unpark_ticket(&mut store, &clock, "t").unwrap();
        assert_eq!(
            out,
            UnparkOutcome::AlreadyAccepted {
                display_id: "t".into()
            }
        );
        assert_eq!(
            selection_of(&store, "t").2,
            before,
            "no-op must not bump updated_at"
        );
    }

    #[test]
    fn unparking_triage_is_rejected() {
        let mut store = open_seeded();
        seed(&store, "t", "triage", None);
        let clock = FakeClock::new(1_778_284_800_000);
        assert!(matches!(
            unpark_ticket(&mut store, &clock, "t"),
            Err(UnparkError::Triage)
        ));
    }

    #[test]
    fn unpark_emits_no_mutation() {
        let mut store = open_seeded();
        seed(&store, "t", "parked", Some("P2"));
        let clock = FakeClock::new(1_778_284_800_000);
        unpark_ticket(&mut store, &clock, "t").unwrap();
        let mutations: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0, "Selection State changes never emit Mutations");
    }

    #[test]
    fn park_emits_no_mutation() {
        let mut store = open_seeded();
        seed(&store, "t", "accepted", Some("P2"));
        let clock = FakeClock::new(1_778_284_800_000);
        park_ticket(&mut store, &clock, "t").unwrap();
        let mutations: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0, "Selection State changes never emit Mutations");
    }

    #[test]
    fn accept_emits_no_mutation() {
        let mut store = open_seeded();
        seed(&store, "t", "triage", None);
        let clock = FakeClock::new(1_778_284_800_000);
        accept_ticket(&mut store, &clock, "t", Some(Priority::P1)).unwrap();
        let mutations: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0, "Selection State changes never emit Mutations");
    }
}
