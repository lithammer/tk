//! Selection State transitions (`tk accept`; `tk park` / `tk unpark` join in
//! tk-75).
//!
//! Selection State is a Local Field (ADR-0027): these transitions update
//! current state only and never append a Mutation, even for a Backend Ticket.
//! They are status-agnostic — a transition touches `selection_state` /
//! `priority`, never `status` — and leave Dependencies and External Blockers
//! untouched, so accepting a blocked triage Ticket keeps it blocked.

use rusqlite::{OptionalExtension, params};

use crate::clock::Clock;
use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::selection_state::SelectionState;

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
