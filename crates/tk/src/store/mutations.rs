//! Mutation Log outbox writer (ADR-0003 current-state + outbox).
//!
//! Mutations originate inside a [`crate::store::repository`] write
//! transaction the caller already owns: the writer allocates the next
//! `mutation_seq`, serializes the payload to a flat JSON object, and
//! inserts one `pending` row into the `mutations` table. It never begins
//! or commits a transaction.
//!
//! All Mutations are queued first, drained later (tk-97). State is
//! `pending` on insert. `transition` is the only writer of `mutations.state`
//! thereafter; the workflow that determines an outcome keeps its own domain
//! preconditions and hands the seam the edge. `mark_applied` wraps it with
//! the Store invariant that an `applied` transition advances the Sync Cursor in
//! the same transaction.
//!
//! The one read here, [`resolve_backend_binding`], answers the question the
//! outbox itself defines: whether a Local Item is already Pending Promotion
//! (ADR-0036).

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::domain::backend_binding::BackendBinding;
use crate::domain::backend_outcome::Failure;
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{DependencyRef, EpicRef, MutationPayload, Promotion};
use crate::domain::mutation_state::MutationState;
use crate::domain::mutation_type::{AddressedCounterpart, MutationType};
use crate::domain::origin::Origin;
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
    let sequence = sequences::next(conn, sequences::Counter::Mutation)?;
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

/// Decode the internal Item ID addressed as a Mutation's counterpart.
///
/// The Mutation Type owns whether its payload addresses an Epic, a Blocking
/// Item, or no counterpart. Keeping that rule with its decoder stops Store
/// workflows from duplicating the Mutation Log payload contract.
pub fn addressed_counterpart_id(
    mutation_type: MutationType,
    payload_json: &str,
) -> Result<Option<String>, serde_json::Error> {
    Ok(match mutation_type.addressed_counterpart() {
        AddressedCounterpart::None => None,
        AddressedCounterpart::Epic => Some(serde_json::from_str::<EpicRef>(payload_json)?.epic_id),
        AddressedCounterpart::BlockingItem => {
            Some(serde_json::from_str::<DependencyRef>(payload_json)?.blocking_id)
        }
    })
}

/// One Mutation a withdrawal may take with it, with the payload left undecoded.
///
/// The counterpart decode stays with the caller because the two withdrawals
/// answer a malformed payload differently: Promotion Cancellation treats it as
/// corruption and refuses, while Detach leaves the row for Sync to diagnose
/// and stays available as local recovery.
#[derive(Debug, Clone)]
pub(crate) struct WithdrawalCandidate {
    pub sequence: i64,
    pub state: MutationState,
    pub mutation_type: MutationType,
    pub item_id: String,
    pub promotion_operation_id: Option<String>,
    payload_json: String,
}

impl WithdrawalCandidate {
    /// The Item ID this Mutation addresses beyond its own target, per its
    /// Mutation Type (ADR-0038).
    pub(crate) fn addressed_counterpart_id(&self) -> Result<Option<String>, serde_json::Error> {
        addressed_counterpart_id(self.mutation_type, &self.payload_json)
    }
}

/// Every Mutation a withdrawal may take with it, in Mutation Sequence order.
///
/// Only `pending` and `failed` rows can qualify: global Mutation Sequence order
/// holds every later Mutation behind a nonterminal Promotion, so collateral was
/// never attempted. Promotions are excluded because a Promotion creates a
/// Backend identity rather than needing one — Promotion Cancellation withdraws
/// those itself, choosing `cancelled` or `abandoned` per Promotion (ADR-0039),
/// and Detach never withdraws one at all. That also keeps `applying` out: the
/// `mutations` CHECK admits that state for a Promotion alone.
///
/// Which rows of these a withdrawal actually takes is the caller's question —
/// Promotion Cancellation asks about a set of Items losing a prospective
/// identity, Detach about one Item losing a concrete one.
pub(crate) fn withdrawal_candidates(
    conn: &Connection,
) -> rusqlite::Result<Vec<WithdrawalCandidate>> {
    let mut stmt = conn.prepare(
        "select sequence, state, mutation_type, item_id, promotion_operation_id, payload_json \
           from mutations \
          where state in ('pending', 'failed') \
            and mutation_type not in ('promote_ticket', 'promote_epic') \
          order by sequence asc",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(WithdrawalCandidate {
            sequence: row.get(0)?,
            state: row.get(1)?,
            mutation_type: row.get(2)?,
            item_id: row.get(3)?,
            promotion_operation_id: row.get(4)?,
            payload_json: row.get(5)?,
        })
    })?;
    rows.collect()
}

/// A `mutations.state` write that [`MutationState`]'s transition table refuses.
///
/// Every caller narrows the row under its own domain precondition before
/// reaching the seam, so this names a Store-layer contract break rather than an
/// operator mistake. It stays separate from the SQLite fault channel: folding
/// the two together would route a busy Repository Store into a corruption
/// diagnostic instead of the retry guidance each caller's `Storage` variant
/// renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IllegalTransition {
    /// The edge is absent from the transition table, or the row was not in
    /// `from` when the update ran.
    #[error("mutation {sequence} cannot move from {from} to {to}")]
    Edge {
        sequence: i64,
        from: MutationState,
        to: MutationState,
    },
    /// The edge records Mutation Failure evidence and the caller supplied none.
    #[error("moving mutation {sequence} to {to} requires a Mutation Failure")]
    MissingEvidence { sequence: i64, to: MutationState },
}

/// Errors returned by `transition`.
#[derive(Debug, Error)]
pub enum TransitionError {
    /// Underlying SQLite error from the state update.
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error(transparent)]
    Illegal(#[from] IllegalTransition),
}

/// Input for `transition`.
#[derive(Debug, Clone, Copy)]
pub struct TransitionRequest<'a> {
    pub sequence: i64,
    /// The row's current state, as the caller read it inside this transaction.
    pub from: MutationState,
    pub to: MutationState,
    /// Evidence for the edges that record one; [`MutationState`]'s transition
    /// table names them, and supplying none there is
    /// [`IllegalTransition::MissingEvidence`].
    pub failure: Option<&'a Failure>,
    pub now: &'a str,
}

/// What an edge does to the row's `failure_json`.
enum FailureColumn<'a> {
    /// A fresh attempt or a settled effect leaves no evidence behind.
    Clear,
    /// A human-curated terminal state keeps the evidence `tk sync log` renders.
    Preserve,
    Record(&'a Failure),
}

/// Move one Mutation Log row along one edge of [`MutationState`]'s transition
/// table, owning the `failure_json` and `state_changed_at` bookkeeping the edge
/// implies.
///
/// `conn` is expected to be inside the caller's active write transaction. The
/// update re-asserts `from` in SQL, so a row the caller misread is refused
/// rather than dragged onto an edge that was never validated.
///
/// Domain preconditions that are not about the edge — whether the Mutation is a
/// Promotion, whether its Item is still Local, whether another Mutation holds
/// the global `applying` barrier — stay with the workflow that owns their
/// diagnostics.
pub(crate) fn transition(
    conn: &Connection,
    req: TransitionRequest<'_>,
) -> Result<(), TransitionError> {
    const PRESERVING: &str = "update mutations \
            set state = ?3, state_changed_at = ?4 \
          where sequence = ?1 and state = ?2";
    const OVERWRITING: &str = "update mutations \
            set state = ?3, state_changed_at = ?4, failure_json = ?5 \
          where sequence = ?1 and state = ?2";

    let illegal = || {
        TransitionError::Illegal(IllegalTransition::Edge {
            sequence: req.sequence,
            from: req.from,
            to: req.to,
        })
    };
    let evidence = || {
        req.failure.ok_or(TransitionError::Illegal(
            IllegalTransition::MissingEvidence {
                sequence: req.sequence,
                to: req.to,
            },
        ))
    };

    // Exhaustive in both positions: a Mutation state added later must not
    // compile until every edge into and out of it has been decided.
    let column = match req.to {
        MutationState::Pending => match req.from {
            MutationState::Applying => FailureColumn::Clear,
            MutationState::Pending
            | MutationState::Failed
            | MutationState::Skipped
            | MutationState::Cancelled
            | MutationState::Abandoned
            | MutationState::Applied => return Err(illegal()),
        },
        MutationState::Failed => match req.from {
            MutationState::Pending | MutationState::Failed | MutationState::Applying => {
                FailureColumn::Record(evidence()?)
            }
            MutationState::Skipped
            | MutationState::Cancelled
            | MutationState::Abandoned
            | MutationState::Applied => {
                return Err(illegal());
            }
        },
        MutationState::Applying => match req.from {
            MutationState::Pending | MutationState::Failed => FailureColumn::Clear,
            MutationState::Applying => FailureColumn::Record(evidence()?),
            MutationState::Skipped
            | MutationState::Cancelled
            | MutationState::Abandoned
            | MutationState::Applied => {
                return Err(illegal());
            }
        },
        MutationState::Skipped => match req.from {
            MutationState::Failed => FailureColumn::Preserve,
            MutationState::Pending
            | MutationState::Applying
            | MutationState::Skipped
            | MutationState::Cancelled
            | MutationState::Abandoned
            | MutationState::Applied => return Err(illegal()),
        },
        // Withdrawing an unobserved creation lands in `abandoned` instead, so
        // `cancelled` keeps meaning that nothing was created (ADR-0039).
        MutationState::Cancelled => match req.from {
            MutationState::Pending | MutationState::Failed => FailureColumn::Preserve,
            MutationState::Applying
            | MutationState::Skipped
            | MutationState::Cancelled
            | MutationState::Abandoned
            | MutationState::Applied => return Err(illegal()),
        },
        // The indeterminate creation's evidence is why the withdrawal
        // happened, so it survives the edge.
        MutationState::Abandoned => match req.from {
            MutationState::Applying => FailureColumn::Preserve,
            MutationState::Pending
            | MutationState::Failed
            | MutationState::Skipped
            | MutationState::Cancelled
            | MutationState::Abandoned
            | MutationState::Applied => return Err(illegal()),
        },
        MutationState::Applied => match req.from {
            MutationState::Pending | MutationState::Failed | MutationState::Applying => {
                FailureColumn::Clear
            }
            MutationState::Skipped
            | MutationState::Cancelled
            | MutationState::Abandoned
            | MutationState::Applied => {
                return Err(illegal());
            }
        },
    };

    let changed = match column {
        FailureColumn::Preserve => conn.execute(
            PRESERVING,
            params![req.sequence, req.from.text(), req.to.text(), req.now],
        )?,
        FailureColumn::Clear => conn.execute(
            OVERWRITING,
            params![
                req.sequence,
                req.from.text(),
                req.to.text(),
                req.now,
                None::<String>
            ],
        )?,
        FailureColumn::Record(failure) => conn.execute(
            OVERWRITING,
            params![
                req.sequence,
                req.from.text(),
                req.to.text(),
                req.now,
                serde_json::to_string(failure).expect("Failure serializes infallibly")
            ],
        )?,
    };
    if changed == 0 {
        return Err(illegal());
    }
    Ok(())
}

/// Mark one Mutation applied and monotonically advance the primary Sync Cursor.
///
/// The caller owns the surrounding write transaction. Keeping both writes in
/// this Store boundary prevents any recovery or normal Sync path from exposing
/// an applied Mutation that the cursor has not observed.
pub(crate) fn mark_applied(
    conn: &Connection,
    sequence: i64,
    from: MutationState,
    now: &str,
) -> Result<(), TransitionError> {
    transition(
        conn,
        TransitionRequest {
            sequence,
            from,
            to: MutationState::Applied,
            failure: None,
            now,
        },
    )?;
    conn.execute(
        "update sync_cursors \
            set last_applied_sequence = max(last_applied_sequence, ?1), updated_at = ?2 \
          where remote_name = 'primary'",
        params![sequence, now],
    )?;
    Ok(())
}

/// Errors returned by [`resolve_backend_binding`].
#[derive(Debug, Error)]
pub enum BackendBindingError {
    /// Underlying SQLite error from the `items` or `mutations` read.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// A `promote_*` row's `payload_json` did not decode as a Promotion
    /// payload. Repository Store corruption, the same fault the sync-side
    /// decode names.
    #[error("malformed payload_json: {0}")]
    PayloadJson(#[from] serde_json::Error),
    /// An Item with backend Origin carrying no `backend_kind`. The `items`
    /// Origin CHECK forbids the pair, so this is Repository Store corruption;
    /// the store layer surfaces it as a typed fault rather than panicking.
    #[error("item {0} has backend Origin with no backend_kind")]
    CorruptBackendKind(String),
}

/// Resolve the Backend Binding of the Item at internal `items.id` `item_id`.
///
/// Backend Origin answers from `items` alone. A Local Item is Pending
/// Promotion when the Mutation Log holds a `promote_ticket` / `promote_epic`
/// Mutation for it in a nonterminal state. An `applied` Promotion has already
/// converted the Item to Backend Origin, and a `cancelled` one is withdrawn
/// intent, so neither keeps the Item pending — which is the whole of what
/// returns a cancelled item to Local Backend Binding (ADR-0038).
///
/// The Backend of a Pending Promotion comes from that Mutation's payload, not
/// from the configured Remote: the target Backend is intent frozen at commit
/// time, so no Repository Store path that reads this state has to consult
/// current Remote configuration (ADR-0036 "Promotion Mutations target Local
/// items").
///
/// An `item_id` no `items` row matches is a caller fault, not a domain
/// outcome, and surfaces as [`BackendBindingError::Sqlite`].
pub fn resolve_backend_binding(
    conn: &Connection,
    item_id: &str,
) -> Result<BackendBinding, BackendBindingError> {
    let (origin, backend_kind): (Origin, Option<String>) = conn.query_row(
        "select origin, backend_kind from items where id = ?1",
        params![item_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;

    if origin == Origin::Backend {
        let backend_kind = backend_kind
            .ok_or_else(|| BackendBindingError::CorruptBackendKind(item_id.to_string()))?;
        return Ok(BackendBinding::Backend { backend_kind });
    }

    let payload_json: Option<String> = conn
        .query_row(
            "select payload_json from mutations \
              where item_id = ?1 \
                and mutation_type in ('promote_ticket','promote_epic') \
                and state in ('pending','failed','applying') \
              order by sequence asc limit 1",
            params![item_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(payload_json) = payload_json else {
        return Ok(BackendBinding::Local);
    };

    let payload: Promotion = serde_json::from_str(&payload_json)?;
    Ok(BackendBinding::PendingPromotion {
        backend_kind: payload.backend_kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_kind::BackendKind;
    use crate::domain::backend_outcome::FailureClass;
    use crate::domain::mutation_payload::{DependencyRef, EpicRef, StatusChange, TitleBody};
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, insert_fixture_item, insert_fixture_mutation,
        insert_fixture_remote,
    };
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

    fn seed_local_ticket(conn: &Connection, id: &str, display: &str) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Local",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_promotion(conn: &Connection, item_id: &str, state: &str, backend_kind: &str) {
        let payload = MutationPayload::Promotion(Promotion {
            title: "Local".into(),
            body: String::new(),
            backend_kind: backend_kind.into(),
        })
        .to_json_string();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                mutation_type: "promote_ticket",
                item_id,
                payload_json: &payload,
                state,
                failure_json: (state == "failed").then_some(r#"{"detail":"boom"}"#),
                promotion_operation_id: Some("promo-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn a_backend_item_carries_the_backend_it_already_belongs_to() {
        let conn = open_seeded();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);

        assert_eq!(
            resolve_backend_binding(&conn, "t1").unwrap(),
            BackendBinding::Backend {
                backend_kind: "github".into()
            }
        );
    }

    #[test]
    fn a_local_item_without_a_promotion_has_no_backend_binding() {
        let conn = open_seeded();
        seed_local_ticket(&conn, "t1", "tk-1");

        assert_eq!(
            resolve_backend_binding(&conn, "t1").unwrap(),
            BackendBinding::Local
        );
    }

    #[test]
    fn a_pending_promotion_takes_its_backend_from_the_payload_not_the_remote() {
        // The whole point of recording the Backend on the payload (ADR-0036):
        // resolving Pending Promotion never consults Remote configuration, so a
        // Remote that disagrees with the frozen intent cannot change the answer.
        let conn = open_seeded();
        seed_local_ticket(&conn, "t1", "tk-1");
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "jira",
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        seed_promotion(&conn, "t1", "pending", "github");

        assert_eq!(
            resolve_backend_binding(&conn, "t1").unwrap(),
            BackendBinding::PendingPromotion {
                backend_kind: "github".into()
            }
        );
    }

    #[test]
    fn a_failed_promotion_still_leaves_the_item_pending_promotion() {
        let conn = open_seeded();
        seed_local_ticket(&conn, "t1", "tk-1");
        seed_promotion(&conn, "t1", "failed", "github");

        assert_eq!(
            resolve_backend_binding(&conn, "t1").unwrap(),
            BackendBinding::PendingPromotion {
                backend_kind: "github".into()
            }
        );
    }

    #[test]
    fn an_applying_promotion_still_leaves_the_item_pending_promotion() {
        let conn = open_seeded();
        seed_local_ticket(&conn, "t1", "tk-1");
        seed_promotion(&conn, "t1", "applying", "github");

        assert_eq!(
            resolve_backend_binding(&conn, "t1").unwrap(),
            BackendBinding::PendingPromotion {
                backend_kind: "github".into()
            }
        );
    }

    #[test]
    fn a_resolved_promotion_does_not_make_a_local_item_pending() {
        // `applied` has already moved its Item to Backend Origin and
        // `cancelled` is withdrawn intent; a Local Item behind either is plain
        // Local. A Promotion never reaches `skipped` — the `mutations` CHECK
        // forbids it (ADR-0038).
        for state in ["applied", "cancelled"] {
            let conn = open_seeded();
            seed_local_ticket(&conn, "t1", "tk-1");
            seed_promotion(&conn, "t1", state, "github");

            assert_eq!(
                resolve_backend_binding(&conn, "t1").unwrap(),
                BackendBinding::Local,
                "a {state} Promotion is resolved intent"
            );
        }
    }

    #[test]
    fn a_non_promotion_mutation_does_not_make_an_item_pending_promotion() {
        let conn = open_seeded();
        seed_local_ticket(&conn, "t1", "tk-1");
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"T","body":""}"#,
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        assert_eq!(
            resolve_backend_binding(&conn, "t1").unwrap(),
            BackendBinding::Local
        );
    }

    #[test]
    fn every_promotion_mutation_type_makes_its_item_pending_promotion() {
        // `resolve_backend_binding`'s `mutation_type in (...)` list is a second
        // encoding of `MutationType::is_promotion`. A Promotion kind the SQL
        // does not name resolves as Local, silently reopening every write gate
        // ADR-0036 put on Backend Binding — so a new kind belongs in that query
        // (and in the `mutations` CHECK), not in an exemption here.
        let payload = MutationPayload::Promotion(Promotion {
            title: "Local".into(),
            body: String::new(),
            backend_kind: "github".into(),
        })
        .to_json_string();

        for mutation_type in MutationType::ALL.into_iter().filter(|t| t.is_promotion()) {
            let conn = open_seeded();
            seed_local_ticket(&conn, "t1", "tk-1");
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id: "e1",
                    display: "tk-2",
                    item_class: "epic",
                    ticket_kind: None,
                    priority: None,
                    title: "Local epic",
                    created_seq: 2,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
            // The `mutations` composite foreign key pins item_class to the
            // Item's own class, so each Promotion kind needs its own target.
            let (item_id, item_class) = match mutation_type {
                MutationType::PromoteEpic => ("e1", "epic"),
                _ => ("t1", "ticket"),
            };
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    mutation_type: mutation_type.text(),
                    item_id,
                    item_class,
                    payload_json: &payload,
                    ..FixtureMutation::default()
                },
            )
            .unwrap();

            assert_eq!(
                resolve_backend_binding(&conn, item_id).unwrap(),
                BackendBinding::PendingPromotion {
                    backend_kind: "github".into()
                },
                "{mutation_type} must leave its Item Pending Promotion"
            );
        }
    }

    #[test]
    fn a_promotion_payload_that_does_not_decode_is_a_typed_fault() {
        let conn = open_seeded();
        seed_local_ticket(&conn, "t1", "tk-1");
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"T"}"#,
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let err = resolve_backend_binding(&conn, "t1").unwrap_err();
        assert!(
            matches!(err, BackendBindingError::PayloadJson(_)),
            "a promote_* row without a Backend is store corruption, got {err:?}"
        );
    }

    // ---- transition ------------------------------------------------------

    const LATER: &str = "2026-05-10T00:00:00.000Z";

    /// Seeds one Mutation row. The Mutation Type is a parameter because the
    /// `mutations` CHECK admits `applying` only for a Promotion and `skipped`
    /// only for everything else, so an edge touching either state can only be
    /// exercised with the matching kind.
    fn seed_row(
        conn: &Connection,
        mutation_type: MutationType,
        state: MutationState,
        failure_json: Option<&str>,
    ) {
        seed_local_ticket(conn, "t1", "tk-1");
        let payload = if mutation_type.is_promotion() {
            MutationPayload::Promotion(Promotion {
                title: "Local".into(),
                body: String::new(),
                backend_kind: "github".into(),
            })
        } else {
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "Local".into(),
                body: String::new(),
            })
        }
        .to_json_string();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                mutation_type: mutation_type.text(),
                item_id: "t1",
                payload_json: &payload,
                state: state.text(),
                failure_json,
                promotion_operation_id: mutation_type.is_promotion().then_some("promo-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    fn read_row(conn: &Connection) -> (MutationState, Option<String>, String) {
        conn.query_row(
            "select state, failure_json, state_changed_at from mutations where sequence = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    fn failure() -> Failure {
        Failure {
            detail: "rejected".into(),
            class: FailureClass::Validation,
            retry_after_s: None,
        }
    }

    fn transition_row(
        conn: &Connection,
        from: MutationState,
        to: MutationState,
        failure: Option<&Failure>,
    ) -> Result<(), TransitionError> {
        transition(
            conn,
            TransitionRequest {
                sequence: 1,
                from,
                to,
                failure,
                now: LATER,
            },
        )
    }

    #[test]
    fn beginning_a_creation_clears_the_previous_attempts_failure() {
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::PromoteTicket,
            MutationState::Failed,
            Some(r#"{"detail":"old"}"#),
        );

        transition_row(&conn, MutationState::Failed, MutationState::Applying, None).unwrap();

        assert_eq!(
            read_row(&conn),
            (MutationState::Applying, None, LATER.to_string())
        );
    }

    #[test]
    fn a_certified_rejection_records_its_failure_evidence() {
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::PromoteTicket,
            MutationState::Applying,
            None,
        );

        transition_row(
            &conn,
            MutationState::Applying,
            MutationState::Failed,
            Some(&failure()),
        )
        .unwrap();

        let (state, stored, _) = read_row(&conn);
        assert_eq!(state, MutationState::Failed);
        assert_eq!(
            serde_json::from_str::<Failure>(&stored.unwrap()).unwrap(),
            failure()
        );
    }

    #[test]
    fn an_indeterminate_creation_records_its_failure_and_stays_applying() {
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::PromoteTicket,
            MutationState::Applying,
            None,
        );

        transition_row(
            &conn,
            MutationState::Applying,
            MutationState::Applying,
            Some(&failure()),
        )
        .unwrap();

        let (state, stored, changed_at) = read_row(&conn);
        assert_eq!(state, MutationState::Applying);
        assert!(stored.is_some());
        assert_eq!(changed_at, LATER);
    }

    #[test]
    fn sync_skip_preserves_the_failure_it_curated_away() {
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::UpdateTicket,
            MutationState::Failed,
            Some(r#"{"detail":"boom"}"#),
        );

        transition_row(&conn, MutationState::Failed, MutationState::Skipped, None).unwrap();

        assert_eq!(
            read_row(&conn),
            (
                MutationState::Skipped,
                Some(r#"{"detail":"boom"}"#.to_string()),
                LATER.to_string()
            )
        );
    }

    #[test]
    fn returning_an_indeterminate_creation_to_the_queue_clears_its_failure() {
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::PromoteTicket,
            MutationState::Applying,
            Some(r#"{"detail":"?"}"#),
        );

        transition_row(&conn, MutationState::Applying, MutationState::Pending, None).unwrap();

        assert_eq!(
            read_row(&conn),
            (MutationState::Pending, None, LATER.to_string())
        );
    }

    #[test]
    fn applying_a_mutation_clears_the_failure_of_the_attempt_that_preceded_it() {
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::UpdateTicket,
            MutationState::Failed,
            Some(r#"{"detail":"boom"}"#),
        );

        transition_row(&conn, MutationState::Failed, MutationState::Applied, None).unwrap();

        assert_eq!(
            read_row(&conn),
            (MutationState::Applied, None, LATER.to_string())
        );
    }

    #[test]
    fn promotion_cancellation_withdraws_untried_and_rejected_intent_alike() {
        for (from, seeded_failure) in [
            (MutationState::Pending, None),
            (MutationState::Failed, Some(r#"{"detail":"boom"}"#)),
        ] {
            let conn = open_seeded();
            seed_row(&conn, MutationType::PromoteTicket, from, seeded_failure);

            transition_row(&conn, from, MutationState::Cancelled, None).unwrap();

            let (state, stored, _) = read_row(&conn);
            assert_eq!(state, MutationState::Cancelled);
            assert_eq!(
                stored.as_deref(),
                seeded_failure,
                "a withdrawal keeps the evidence of the rejection that motivated it"
            );
        }
    }

    #[test]
    fn withdrawing_an_indeterminate_creation_abandons_it_and_keeps_its_evidence() {
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::PromoteTicket,
            MutationState::Applying,
            Some(r#"{"detail":"?"}"#),
        );

        transition_row(
            &conn,
            MutationState::Applying,
            MutationState::Abandoned,
            None,
        )
        .unwrap();

        assert_eq!(
            read_row(&conn),
            (
                MutationState::Abandoned,
                Some(r#"{"detail":"?"}"#.to_string()),
                LATER.to_string()
            ),
            "the indeterminate diagnostic is why the withdrawal happened"
        );
    }

    #[test]
    fn an_indeterminate_creation_is_abandoned_rather_than_cancelled() {
        // `cancelled` means nothing was created. An unobserved outcome has to
        // land in its own state or that meaning stops holding (ADR-0039).
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::PromoteTicket,
            MutationState::Applying,
            None,
        );

        let err = transition_row(
            &conn,
            MutationState::Applying,
            MutationState::Cancelled,
            None,
        )
        .unwrap_err();

        assert!(
            matches!(
                err,
                TransitionError::Illegal(IllegalTransition::Edge { .. })
            ),
            "got {err:?}"
        );
        assert_eq!(read_row(&conn).0, MutationState::Applying);
    }

    #[test]
    fn an_edge_outside_the_transition_table_is_refused() {
        // Terminal states have no exit, and no workflow reaches `skipped`
        // from anything but a certified rejection.
        for (from, to) in [
            (MutationState::Applied, MutationState::Pending),
            (MutationState::Skipped, MutationState::Applying),
            (MutationState::Pending, MutationState::Skipped),
            (MutationState::Cancelled, MutationState::Pending),
            (MutationState::Applied, MutationState::Applied),
        ] {
            let conn = open_seeded();
            seed_row(&conn, MutationType::UpdateTicket, from, None);

            let err = transition_row(&conn, from, to, Some(&failure())).unwrap_err();
            assert!(
                matches!(
                    err,
                    TransitionError::Illegal(IllegalTransition::Edge { .. })
                ),
                "{from} -> {to} is not a legal edge, got {err:?}"
            );
            assert_eq!(read_row(&conn).0, from, "a refused edge writes nothing");
        }
    }

    #[test]
    fn only_an_unobserved_creation_is_abandoned_and_nothing_leaves_it() {
        // Seed a Promotion: the `mutations` CHECK admits `abandoned` for
        // nothing else.
        for (from, to) in [
            (MutationState::Pending, MutationState::Abandoned),
            (MutationState::Failed, MutationState::Abandoned),
            (MutationState::Applied, MutationState::Abandoned),
            (MutationState::Abandoned, MutationState::Pending),
            (MutationState::Abandoned, MutationState::Applied),
        ] {
            let conn = open_seeded();
            // A `failed` row must carry evidence to satisfy the CHECK.
            let seeded = match from {
                MutationState::Failed => Some(r#"{"detail":"boom"}"#),
                _ => None,
            };
            seed_row(&conn, MutationType::PromoteTicket, from, seeded);

            let err = transition_row(&conn, from, to, Some(&failure())).unwrap_err();
            assert!(
                matches!(
                    err,
                    TransitionError::Illegal(IllegalTransition::Edge { .. })
                ),
                "{from} -> {to} is not a legal edge, got {err:?}"
            );
            assert_eq!(read_row(&conn).0, from, "a refused edge writes nothing");
        }
    }

    #[test]
    fn a_row_that_left_the_expected_state_is_refused() {
        // The caller reads the state inside its own transaction, so this
        // guards a Store-layer contract break rather than a live race.
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::UpdateTicket,
            MutationState::Pending,
            None,
        );

        let err = transition_row(&conn, MutationState::Applying, MutationState::Pending, None)
            .unwrap_err();

        assert!(
            matches!(
                err,
                TransitionError::Illegal(IllegalTransition::Edge { .. })
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn an_edge_that_records_evidence_refuses_to_run_without_it() {
        let conn = open_seeded();
        seed_row(
            &conn,
            MutationType::UpdateTicket,
            MutationState::Pending,
            None,
        );

        let err =
            transition_row(&conn, MutationState::Pending, MutationState::Failed, None).unwrap_err();

        assert!(
            matches!(
                err,
                TransitionError::Illegal(IllegalTransition::MissingEvidence { .. })
            ),
            "got {err:?}"
        );
        assert_eq!(read_row(&conn).0, MutationState::Pending);
    }

    #[test]
    fn marking_applied_advances_the_sync_cursor_monotonically() {
        let mut conn = open_seeded();
        crate::store::sync::set_remote(&mut conn, BackendKind::Github, "{}", LATER).unwrap();
        seed_backend_ticket(&conn, "t1", "tk-1", 1);
        for sequence in [4, 9] {
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence,
                    mutation_type: "update_ticket",
                    item_id: "t1",
                    payload_json: r#"{"title":"T","body":""}"#,
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
        }

        mark_applied(&conn, 9, MutationState::Pending, LATER).unwrap();
        mark_applied(&conn, 4, MutationState::Pending, LATER).unwrap();

        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 9, "an out-of-order apply never rewinds the cursor");
    }
}
