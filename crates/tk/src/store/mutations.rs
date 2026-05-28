//! Mutation Log outbox writer (ADR-0003 current-state + outbox).
//!
//! Mutations originate inside a [`crate::store::repository`] write
//! transaction the caller already owns: the writer allocates the next
//! `mutation_seq`, serializes the payload to a flat JSON object, and
//! inserts one `pending` row into the `mutations` table. It never begins
//! or commits a transaction.
//!
//! All Mutations are queued first, drained later (tk-97). State is
//! `pending` on insert and only the Sync Engine transitions it onwards
//! (`applied`, `failed`, or `skipped`); writers here never construct any
//! other state directly.

use rusqlite::{Connection, params};
use thiserror::Error;

use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::MutationPayload;
use crate::domain::mutation_type::MutationType;
use crate::store::sequences;

/// Errors returned by [`append`].
#[derive(Debug, Error)]
pub enum AppendError {
    /// Underlying SQLite error from the sequence allocation or insert.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// `mutation_seq` row missing from `sequences`. Repository Store
    /// corruption rather than recoverable application state.
    #[error(transparent)]
    Sequence(#[from] sequences::SequenceError),
}

/// Append one Mutation row to the `mutations` outbox.
///
/// `conn` is expected to be inside an active `begin immediate` transaction;
/// committing or rolling back is the caller's responsibility. The payload
/// is serialized as a flat JSON object per [`MutationPayload::to_json_string`]
/// so the column's `json_valid()` CHECK constraint always holds.
///
/// Returns the freshly allocated `mutation_seq` so callers that need the
/// row identifier downstream (e.g. surfacing a pending Mutation count) can
/// avoid a follow-up `SELECT`.
pub fn append(
    conn: &Connection,
    mutation_type: MutationType,
    item_id: &str,
    item_class: ItemClass,
    payload: &MutationPayload,
    now_iso: &str,
) -> Result<i64, AppendError> {
    let sequence = sequences::next(conn, "mutation_seq")?;
    let payload_json = payload.to_json_string();
    conn.execute(
        "insert into mutations(\
            sequence, mutation_type, item_id, item_class, payload_json, \
            state, failure_json, created_at, state_changed_at\
         ) values (?1, ?2, ?3, ?4, ?5, 'pending', null, ?6, ?6)",
        params![
            sequence,
            mutation_type.text(),
            item_id,
            item_class.text(),
            payload_json,
            now_iso,
        ],
    )?;
    Ok(sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::mutation_payload::{DependencyRef, EpicRef, StatusChange, TitleBody};
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, insert_fixture_item};
    use rusqlite::Connection;

    fn open_seeded() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn
    }

    fn seed_backend_ticket(conn: &Connection, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
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

    #[test]
    fn append_writes_pending_row_with_serialized_title_body() {
        let conn = open_seeded();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);

        let tx = conn.unchecked_transaction().unwrap();
        append(
            &tx,
            MutationType::UpdateTicket,
            "t1",
            ItemClass::Ticket,
            &MutationPayload::UpdateTitleBody(TitleBody {
                title: "New title".into(),
                body: "New body".into(),
            }),
            "2026-05-09T00:00:00.000Z",
        )
        .unwrap();
        tx.commit().unwrap();

        let (mtype, item_id, item_class, payload, state, failure): (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "select mutation_type, item_id, item_class, payload_json, state, failure_json \
                 from mutations where sequence = 1",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(mtype, "update_ticket");
        assert_eq!(item_id, "t1");
        assert_eq!(item_class, "ticket");
        assert_eq!(payload, r#"{"title":"New title","body":"New body"}"#);
        assert_eq!(state, "pending");
        assert_eq!(failure, None);
    }

    #[test]
    fn append_serializes_epic_ref_for_add_ticket_to_epic() {
        let conn = open_seeded();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);

        let tx = conn.unchecked_transaction().unwrap();
        append(
            &tx,
            MutationType::AddTicketToEpic,
            "t1",
            ItemClass::Ticket,
            &MutationPayload::EpicRef(EpicRef {
                epic_id: "epic-internal-id".into(),
            }),
            "2026-05-09T00:00:00.000Z",
        )
        .unwrap();
        tx.commit().unwrap();

        let payload: String = conn
            .query_row(
                "select payload_json from mutations where sequence = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(payload, r#"{"epic_id":"epic-internal-id"}"#);
    }

    #[test]
    fn append_serializes_status_change_payload() {
        let conn = open_seeded();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);

        let tx = conn.unchecked_transaction().unwrap();
        append(
            &tx,
            MutationType::SetItemStatus,
            "t1",
            ItemClass::Ticket,
            &MutationPayload::ItemStatus(StatusChange {
                status: "done".into(),
            }),
            "2026-05-09T00:00:00.000Z",
        )
        .unwrap();
        tx.commit().unwrap();

        let payload: String = conn
            .query_row(
                "select payload_json from mutations where sequence = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(payload, r#"{"status":"done"}"#);
    }

    #[test]
    fn append_serializes_dependency_ref_payload() {
        let conn = open_seeded();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);

        let tx = conn.unchecked_transaction().unwrap();
        append(
            &tx,
            MutationType::AddDependency,
            "t1",
            ItemClass::Ticket,
            &MutationPayload::DependencyRef(DependencyRef {
                blocking_id: "blocker-id".into(),
            }),
            "2026-05-09T00:00:00.000Z",
        )
        .unwrap();
        tx.commit().unwrap();

        let payload: String = conn
            .query_row(
                "select payload_json from mutations where sequence = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(payload, r#"{"blocking_id":"blocker-id"}"#);
    }

    #[test]
    fn append_returns_monotonically_increasing_sequences() {
        let conn = open_seeded();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);

        let tx = conn.unchecked_transaction().unwrap();
        let one = append(
            &tx,
            MutationType::UpdateTicket,
            "t1",
            ItemClass::Ticket,
            &MutationPayload::UpdateTitleBody(TitleBody {
                title: "A".into(),
                body: String::new(),
            }),
            "2026-05-09T00:00:00.000Z",
        )
        .unwrap();
        let two = append(
            &tx,
            MutationType::UpdateTicket,
            "t1",
            ItemClass::Ticket,
            &MutationPayload::UpdateTitleBody(TitleBody {
                title: "B".into(),
                body: String::new(),
            }),
            "2026-05-09T00:00:00.000Z",
        )
        .unwrap();
        let three = append(
            &tx,
            MutationType::UpdateTicket,
            "t1",
            ItemClass::Ticket,
            &MutationPayload::UpdateTitleBody(TitleBody {
                title: "C".into(),
                body: String::new(),
            }),
            "2026-05-09T00:00:00.000Z",
        )
        .unwrap();
        tx.commit().unwrap();

        assert_eq!((one, two, three), (1, 2, 3));
        let count: i64 = conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn append_advances_the_mutation_sequence_counter() {
        let conn = open_seeded();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);

        let initial: i64 = conn
            .query_row(
                "select value from sequences where name = 'mutation_seq'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(initial, 0);

        let tx = conn.unchecked_transaction().unwrap();
        append(
            &tx,
            MutationType::UpdateTicket,
            "t1",
            ItemClass::Ticket,
            &MutationPayload::UpdateTitleBody(TitleBody {
                title: "X".into(),
                body: String::new(),
            }),
            "2026-05-09T00:00:00.000Z",
        )
        .unwrap();
        append(
            &tx,
            MutationType::UpdateTicket,
            "t1",
            ItemClass::Ticket,
            &MutationPayload::UpdateTitleBody(TitleBody {
                title: "Y".into(),
                body: String::new(),
            }),
            "2026-05-09T00:00:00.000Z",
        )
        .unwrap();
        tx.commit().unwrap();

        let after: i64 = conn
            .query_row(
                "select value from sequences where name = 'mutation_seq'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, 2);
    }
}
