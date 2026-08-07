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

/// Input for [`append`].
#[derive(Debug, Clone, Copy)]
pub struct AppendRequest<'a> {
    pub mutation_type: MutationType,
    pub item_id: &'a str,
    pub item_class: ItemClass,
    pub payload: &'a MutationPayload,
    /// Promotion Operation grouping every Mutation one `tk promote`
    /// invocation appends (ADR-0036). `None` for a Mutation that stands on
    /// its own.
    pub promotion_operation_id: Option<&'a str>,
    pub now_iso: &'a str,
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
pub fn append(conn: &Connection, req: AppendRequest<'_>) -> Result<i64, AppendError> {
    let sequence = sequences::next(conn, "mutation_seq")?;
    let payload_json = req.payload.to_json_string();
    conn.execute(
        "insert into mutations(\
            sequence, mutation_type, item_id, item_class, payload_json, \
            state, failure_json, created_at, state_changed_at, promotion_operation_id\
         ) values (?1, ?2, ?3, ?4, ?5, 'pending', null, ?6, ?6, ?7)",
        params![
            sequence,
            req.mutation_type.text(),
            req.item_id,
            req.item_class.text(),
            payload_json,
            req.now_iso,
            req.promotion_operation_id,
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
            AppendRequest {
                mutation_type: MutationType::UpdateTicket,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::UpdateTitleBody(TitleBody {
                    title: "New title".into(),
                    body: "New body".into(),
                }),
                promotion_operation_id: None,
                now_iso: "2026-05-09T00:00:00.000Z",
            },
        )
        .unwrap();
        tx.commit().unwrap();

        let (mtype, item_id, item_class, payload, state, failure, promotion_operation_id): (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "select mutation_type, item_id, item_class, payload_json, state, failure_json, \
                        promotion_operation_id \
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
                        r.get(6)?,
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
        assert_eq!(promotion_operation_id, None);
    }

    #[test]
    fn append_writes_the_supplied_promotion_operation_id() {
        let conn = open_seeded();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);

        let tx = conn.unchecked_transaction().unwrap();
        append(
            &tx,
            AppendRequest {
                mutation_type: MutationType::UpdateTicket,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::UpdateTitleBody(TitleBody {
                    title: "New title".into(),
                    body: "New body".into(),
                }),
                promotion_operation_id: Some("promo-1"),
                now_iso: "2026-05-09T00:00:00.000Z",
            },
        )
        .unwrap();
        tx.commit().unwrap();

        let stored: Option<String> = conn
            .query_row(
                "select promotion_operation_id from mutations where sequence = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored.as_deref(), Some("promo-1"));
    }

    #[test]
    fn append_serializes_epic_ref_for_add_ticket_to_epic() {
        let conn = open_seeded();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);

        let tx = conn.unchecked_transaction().unwrap();
        append(
            &tx,
            AppendRequest {
                mutation_type: MutationType::AddTicketToEpic,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::EpicRef(EpicRef {
                    epic_id: "epic-internal-id".into(),
                }),
                promotion_operation_id: None,
                now_iso: "2026-05-09T00:00:00.000Z",
            },
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
            AppendRequest {
                mutation_type: MutationType::SetItemStatus,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::ItemStatus(StatusChange {
                    status: "done".into(),
                }),
                promotion_operation_id: None,
                now_iso: "2026-05-09T00:00:00.000Z",
            },
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
            AppendRequest {
                mutation_type: MutationType::AddDependency,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::DependencyRef(DependencyRef {
                    blocking_id: "blocker-id".into(),
                }),
                promotion_operation_id: None,
                now_iso: "2026-05-09T00:00:00.000Z",
            },
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
            AppendRequest {
                mutation_type: MutationType::UpdateTicket,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::UpdateTitleBody(TitleBody {
                    title: "A".into(),
                    body: String::new(),
                }),
                promotion_operation_id: None,
                now_iso: "2026-05-09T00:00:00.000Z",
            },
        )
        .unwrap();
        let two = append(
            &tx,
            AppendRequest {
                mutation_type: MutationType::UpdateTicket,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::UpdateTitleBody(TitleBody {
                    title: "B".into(),
                    body: String::new(),
                }),
                promotion_operation_id: None,
                now_iso: "2026-05-09T00:00:00.000Z",
            },
        )
        .unwrap();
        let three = append(
            &tx,
            AppendRequest {
                mutation_type: MutationType::UpdateTicket,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::UpdateTitleBody(TitleBody {
                    title: "C".into(),
                    body: String::new(),
                }),
                promotion_operation_id: None,
                now_iso: "2026-05-09T00:00:00.000Z",
            },
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
            AppendRequest {
                mutation_type: MutationType::UpdateTicket,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::UpdateTitleBody(TitleBody {
                    title: "X".into(),
                    body: String::new(),
                }),
                promotion_operation_id: None,
                now_iso: "2026-05-09T00:00:00.000Z",
            },
        )
        .unwrap();
        append(
            &tx,
            AppendRequest {
                mutation_type: MutationType::UpdateTicket,
                item_id: "t1",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::UpdateTitleBody(TitleBody {
                    title: "Y".into(),
                    body: String::new(),
                }),
                promotion_operation_id: None,
                now_iso: "2026-05-09T00:00:00.000Z",
            },
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
