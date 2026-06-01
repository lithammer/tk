//! Sync-side store helpers: Pull merge, Mutation Log decode + state
//! transitions, Mark-skipped curation, Remote read, and pending/failed count.
//!
//! Every operation here is SQL on the `items` / `mutations` / `item_ids` /
//! `remotes` / `sync_cursors` tables, so it lives under [`crate::store`]. The
//! sync engine ([`crate::sync`]) and the `tk sync` command compose these with
//! the backend-blind [`crate::remote::adapter::Adapter`] trait.
//!
//! Write helpers open their own transaction and take `&mut Connection`; read
//! helpers take `&Connection`. `advance_sync_cursor` is the exception — it runs
//! inside `apply_mutation_outcome`'s transaction and so takes a borrowed
//! connection without managing one.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

use crate::domain::apply_outcome::ApplyOutcome;
use crate::domain::backend_item_snapshot::BackendItemSnapshot;
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{
    DependencyRef, EpicRef, MutationPayload, StatusChange, TitleBody,
};
use crate::domain::mutation_state::MutationState;
use crate::domain::mutation_type::MutationType;
use crate::domain::mutation_view::MutationView;
use crate::store::repository::create::generate_internal_id;
use crate::store::sequences::{self, SequenceError};

// ──────────────────────────────────────────────────────────────────────────
// Pull merge
// ──────────────────────────────────────────────────────────────────────────

/// Error returned by [`merge_backend_snapshots`].
#[derive(Debug, Error)]
pub enum MergeError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error(transparent)]
    Sequence(#[from] SequenceError),
    /// A snapshot's `display_id` collided with an existing `item_ids.value`
    /// (a Display ID or Alias already claimed by another Item). Carries the
    /// colliding Display ID for the command's verbatim diagnostic.
    #[error("Display ID '{0}' already claimed by an existing Item")]
    DisplayIdCollision(String),
}

/// Merge a Pull's `BackendItemSnapshot` slice into the Repository Store
/// (ADR-0010 Pull-merge-skips-pending).
///
/// Runs inside its own transaction. Per snapshot:
///   - A: no `(backend_kind, backend_key)` match → INSERT a new `items` row
///     with `origin='backend'`, a fresh internal ID, and a `created_seq` from
///     `item_created_seq`, plus the `item_ids` resolver row. A unique
///     violation against an existing Display ID returns
///     [`MergeError::DisplayIdCollision`].
///   - B: match exists AND a pending/failed Mutation references it → SKIP the
///     UPDATE. The Mutation Log is the source of truth for in-flight edits.
///   - C: match exists with no in-flight Mutations → UPDATE title, body,
///     status, updated_at; the Display ID is preserved.
///   - D: snapshot list shorter than local backend rows → v1 no-op (deletion
///     detection is deferred).
///
/// `BackendItemSnapshot` carries no Priority; backend Tickets default to `P2`
/// so the `items` Ticket-Priority constraint holds. `rng` supplies entropy for
/// the internal stable `items.id`.
pub fn merge_backend_snapshots(
    conn: &mut Connection,
    rng: &mut dyn rand::Rng,
    snapshots: &[BackendItemSnapshot],
    now: &str,
) -> Result<(), MergeError> {
    let tx = conn.transaction()?;

    for snap in snapshots {
        let existing: Option<String> = tx
            .query_row(
                "select id from items where backend_kind = ?1 and backend_key = ?2",
                params![snap.backend_kind, snap.backend_key],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(item_id) = existing {
            // Scenario B: skip if a pending/failed Mutation targets this Item.
            let in_flight: Option<i64> = tx
                .query_row(
                    "select 1 from mutations \
                     where item_id = ?1 and item_class = ?2 \
                       and state in ('pending','failed') limit 1",
                    params![item_id, snap.item_class],
                    |r| r.get(0),
                )
                .optional()?;
            if in_flight.is_some() {
                continue;
            }

            // Scenario C: overwrite title/body/status/updated_at; Display ID
            // is left alone.
            tx.execute(
                "update items \
                    set title = ?2, body = ?3, status = ?4, updated_at = ?5 \
                  where id = ?1",
                params![item_id, snap.title, snap.body, snap.status.text(), now],
            )?;
            continue;
        }

        // Scenario A: INSERT a new backend-origin Item.
        let id = generate_internal_id(rng);
        let created_seq = sequences::next(&tx, "item_created_seq")?;
        let ticket_kind_text = snap
            .ticket_kind
            .map(super::super::domain::ticket_kind::TicketKind::text);
        let priority_text = if snap.item_class == ItemClass::Ticket {
            Some("P2")
        } else {
            None
        };

        tx.execute(
            "insert into items(\
                id, display_value, item_class, ticket_kind, priority, title, body, \
                origin, backend_kind, backend_key, status, \
                created_seq, created_at, updated_at\
             ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'backend', ?8, ?9, ?10, ?11, ?12, ?12)",
            params![
                id,
                snap.display_id,
                snap.item_class.text(),
                ticket_kind_text,
                priority_text,
                snap.title,
                snap.body,
                snap.backend_kind,
                snap.backend_key,
                snap.status.text(),
                created_seq,
                now,
            ],
        )?;

        // Display ID resolver row. A unique/PK violation means the Display ID
        // is already claimed by another Item → DisplayIdCollision. Dropping
        // `tx` on the early return rolls back the orphaned `items` insert.
        match tx.execute(
            "insert into item_ids(value, source, item_id, created_at) \
             values (?1, 'display', ?2, ?3)",
            params![snap.display_id, id, now],
        ) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    || e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
            {
                return Err(MergeError::DisplayIdCollision(snap.display_id.clone()));
            }
            Err(other) => return Err(other.into()),
        }
    }

    tx.commit()?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Mutation Log decode (the engine's applicable-set load)
// ──────────────────────────────────────────────────────────────────────────

/// Error returned by [`load_applicable_mutations`].
#[derive(Debug, Error)]
pub enum LoadApplicableError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// SQL CHECK accepted a `mutation_type` text value the [`MutationType`]
    /// enum does not decode — fires when schema and enum drift.
    #[error("unrecognised mutation_type: {0}")]
    UnknownMutationType(String),
    /// `mutation_type` decoded, but [`MutationPayload`] has no matching variant
    /// (today: `promote_*`, `*_external_blocker`). A forward-compatibility
    /// guard so a future Mutation kind cannot be silently skipped.
    #[error("no payload projection for mutation kind: {0}")]
    PayloadVariantMissing(MutationType),
    /// `payload_json` parsed as JSON (the column's CHECK guarantees that) but
    /// did not match the variant's shape — Repository Store corruption.
    #[error("malformed payload_json: {0}")]
    PayloadJson(#[from] serde_json::Error),
}

/// Decode the typed Mutation Log entries the engine should (re)apply.
///
/// Returns `state in ('pending','failed')` rows in `sequence` order, each
/// joined to `items` so backend identifiers reach the adapter without a second
/// query.
pub fn load_applicable_mutations(
    conn: &Connection,
) -> Result<Vec<MutationView>, LoadApplicableError> {
    let mut stmt = conn.prepare(
        "select m.sequence, m.mutation_type, m.item_id, m.item_class, \
                m.payload_json, i.backend_kind, i.backend_key \
           from mutations m \
           join items i on i.id = m.item_id and i.item_class = m.item_class \
          where m.state in ('pending','failed') \
          order by m.sequence asc",
    )?;
    let mut rows = stmt.query([])?;

    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let sequence: i64 = row.get(0)?;
        let type_text: String = row.get(1)?;
        let item_id: String = row.get(2)?;
        let item_class: ItemClass = row.get(3)?;
        let payload_text: String = row.get(4)?;
        let backend_kind: Option<String> = row.get(5)?;
        let backend_key: Option<String> = row.get(6)?;

        let mutation_type = MutationType::from_str(&type_text)
            .map_err(|_| LoadApplicableError::UnknownMutationType(type_text))?;
        let payload = decode_mutation_payload(mutation_type, &payload_text)?;

        out.push(MutationView {
            sequence,
            mutation_type,
            item_id,
            item_class,
            payload,
            backend_kind,
            backend_key,
        });
    }

    Ok(out)
}

/// Decode a `payload_json` text column into the [`MutationPayload`] variant the
/// [`MutationType`] selects.
fn decode_mutation_payload(
    mutation_type: MutationType,
    payload_text: &str,
) -> Result<MutationPayload, LoadApplicableError> {
    use MutationType as Mt;
    Ok(match mutation_type {
        Mt::UpdateTicket | Mt::UpdateEpic => {
            MutationPayload::UpdateTitleBody(serde_json::from_str::<TitleBody>(payload_text)?)
        }
        Mt::AddTicketToEpic | Mt::RemoveTicketFromEpic => {
            MutationPayload::EpicRef(serde_json::from_str::<EpicRef>(payload_text)?)
        }
        Mt::SetItemStatus => {
            MutationPayload::ItemStatus(serde_json::from_str::<StatusChange>(payload_text)?)
        }
        Mt::AddDependency | Mt::RemoveDependency => {
            MutationPayload::DependencyRef(serde_json::from_str::<DependencyRef>(payload_text)?)
        }
        Mt::PromoteTicket
        | Mt::PromoteEpic
        | Mt::AddExternalBlocker
        | Mt::ResolveExternalBlocker => {
            return Err(LoadApplicableError::PayloadVariantMissing(mutation_type));
        }
    })
}

// ──────────────────────────────────────────────────────────────────────────
// Apply outcome / mark skipped / cursor
// ──────────────────────────────────────────────────────────────────────────

/// On-disk wire shape for `mutations.failure_json`. Encoder and decoder share
/// this one type so the two sides cannot drift. `serde_json` always emits
/// valid JSON (control bytes escaped as `\uXXXX`), satisfying the column's
/// `json_valid()` CHECK.
#[derive(Debug, Serialize, Deserialize)]
struct FailureJsonWrapper {
    detail: String,
}

/// Error returned by [`apply_mutation_outcome`].
#[derive(Debug, Error)]
pub enum ApplyMutationOutcomeError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// No `mutations` row matches `sequence`.
    #[error("mutation {0} not found")]
    MutationNotFound(i64),
    /// The matched row's prior state is `applied` or `skipped` — the engine
    /// must never request a transition out of a terminal state.
    #[error("mutation {0} is not in an applicable state")]
    MutationNotApplicable(i64),
}

/// Persist the effect of an adapter [`ApplyOutcome`] against the Mutation Log row at
/// `sequence`, inside its own transaction:
///   - `pending`/`failed` + `Success` → `applied`, clear `failure_json`,
///     advance the Sync Cursor.
///   - `pending` + `Failure` → `failed`, record `{"detail":"…"}`.
///   - `failed` + `Failure` → state unchanged, refresh `failure_json`.
///   - any other prior state → [`ApplyMutationOutcomeError::MutationNotApplicable`].
///
/// A missing row returns [`ApplyMutationOutcomeError::MutationNotFound`].
pub fn apply_mutation_outcome(
    conn: &mut Connection,
    sequence: i64,
    outcome: &ApplyOutcome,
    now: &str,
) -> Result<(), ApplyMutationOutcomeError> {
    let tx = conn.transaction()?;

    let prior: Option<MutationState> = tx
        .query_row(
            "select state from mutations where sequence = ?1",
            params![sequence],
            |r| r.get(0),
        )
        .optional()?;
    let prior = prior.ok_or(ApplyMutationOutcomeError::MutationNotFound(sequence))?;
    // Apply is only defined for entries the engine has not yet resolved; an
    // already-`applied` or curated-`skipped` row is not re-applicable.
    if !matches!(prior, MutationState::Pending | MutationState::Failed) {
        return Err(ApplyMutationOutcomeError::MutationNotApplicable(sequence));
    }

    match outcome {
        ApplyOutcome::Accepted(_) => {
            tx.execute(
                "update mutations \
                    set state = 'applied', failure_json = null, state_changed_at = ?2 \
                  where sequence = ?1",
                params![sequence, now],
            )?;
            advance_sync_cursor(&tx, sequence, now)?;
        }
        ApplyOutcome::Rejected(failure) => {
            let failure_json = serde_json::to_string(&FailureJsonWrapper {
                detail: failure.detail.clone(),
            })
            .expect("FailureJsonWrapper serializes infallibly");
            if prior == MutationState::Pending {
                tx.execute(
                    "update mutations \
                        set state = 'failed', failure_json = ?2, state_changed_at = ?3 \
                      where sequence = ?1",
                    params![sequence, failure_json, now],
                )?;
            } else {
                // prior was 'failed' — keep state, refresh failure_json.
                tx.execute(
                    "update mutations \
                        set failure_json = ?2, state_changed_at = ?3 \
                      where sequence = ?1",
                    params![sequence, failure_json, now],
                )?;
            }
        }
    }

    tx.commit()?;
    Ok(())
}

/// Advance the v1 single Remote's Sync Cursor to `sequence`. Runs inside the
/// caller's open transaction (so it takes a borrowed connection rather than
/// opening one). The v1 schema has exactly one cursor — the `primary` row.
fn advance_sync_cursor(conn: &Connection, sequence: i64, now: &str) -> rusqlite::Result<()> {
    conn.execute(
        "update sync_cursors \
            set last_applied_sequence = ?1, updated_at = ?2 \
          where remote_name = 'primary'",
        params![sequence, now],
    )?;
    Ok(())
}

/// Error returned by [`mark_mutation_skipped`].
#[derive(Debug, Error)]
pub enum MarkSkippedError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// No `mutations` row matches `sequence`.
    #[error("mutation {0} not found")]
    MutationNotFound(i64),
    /// The matched row's prior state is not `failed`. Skipping is only a
    /// curation tool for Mutations the backend has already rejected.
    #[error("mutation {0} is not in the failed state")]
    MutationNotFailed(i64),
}

/// Transition a `failed` Mutation Log entry into `skipped`, inside its own
/// transaction. Refuses a Mutation that is not `failed`. Clears no metadata —
/// the latest `failure_json` is preserved so `tk sync log` can show why the
/// Mutation was abandoned.
pub fn mark_mutation_skipped(
    conn: &mut Connection,
    sequence: i64,
    now: &str,
) -> Result<(), MarkSkippedError> {
    let tx = conn.transaction()?;

    let prior: Option<MutationState> = tx
        .query_row(
            "select state from mutations where sequence = ?1",
            params![sequence],
            |r| r.get(0),
        )
        .optional()?;
    let prior = prior.ok_or(MarkSkippedError::MutationNotFound(sequence))?;
    if prior != MutationState::Failed {
        return Err(MarkSkippedError::MutationNotFailed(sequence));
    }

    tx.execute(
        "update mutations \
            set state = 'skipped', state_changed_at = ?2 \
          where sequence = ?1",
        params![sequence, now],
    )?;

    tx.commit()?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Remote read + pending/failed count
// ──────────────────────────────────────────────────────────────────────────

/// Loaded copy of the singleton Remote configuration plus its Sync Cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRow {
    pub backend_kind: String,
    pub config_json: String,
    pub last_applied_sequence: i64,
}

/// Read the v1 singleton Remote configuration plus its Sync Cursor. Returns
/// `None` when no Remote is configured.
pub fn get_remote(conn: &Connection) -> rusqlite::Result<Option<RemoteRow>> {
    conn.query_row(
        "select r.backend_kind, r.config_json, c.last_applied_sequence \
           from remotes r \
           join sync_cursors c on c.remote_name = r.name \
          where r.name = 'primary'",
        [],
        |row| {
            Ok(RemoteRow {
                backend_kind: row.get(0)?,
                config_json: row.get(1)?,
                last_applied_sequence: row.get(2)?,
            })
        },
    )
    .optional()
}

/// Count Mutation Log entries in `pending` or `failed` state.
pub fn pending_or_failed_mutation_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "select count(*) from mutations where state in ('pending','failed')",
        [],
        |r| r.get(0),
    )
}

// ──────────────────────────────────────────────────────────────────────────
// Mutation Log read (tk sync log)
// ──────────────────────────────────────────────────────────────────────────

/// Filter for the `tk sync log` list view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogListFilter {
    /// Pending + failed + skipped (the default).
    Default,
    Pending,
    Failed,
    Skipped,
}

/// One row of the `tk sync log` list view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogListRow {
    pub sequence: i64,
    pub state: MutationState,
    pub mutation_type: MutationType,
    pub target_display_id: String,
    pub created_at: String,
    /// Decoded `failure.detail`; set only for `failed` rows.
    pub failure_detail: Option<String>,
}

/// One row of the `tk sync log <sequence>` detail view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogDetailRow {
    pub sequence: i64,
    pub state: MutationState,
    pub mutation_type: MutationType,
    pub target_display_id: String,
    pub item_class: ItemClass,
    pub payload_json: String,
    pub failure_detail: Option<String>,
    pub created_at: String,
    pub state_changed_at: String,
}

/// Error returned by [`list_mutation_log`] and [`show_mutation_log`].
#[derive(Debug, Error)]
pub enum LogError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// No `mutations` row matches the supplied sequence.
    #[error("mutation {0} not found")]
    MutationNotFound(i64),
    /// Persisted `failure_json` did not match [`FailureJsonWrapper`].
    #[error("malformed failure_json: {0}")]
    FailureJson(#[from] serde_json::Error),
}

/// Return Mutation Log rows matching `filter` in ascending sequence order.
pub fn list_mutation_log(
    conn: &Connection,
    filter: LogListFilter,
) -> Result<Vec<LogListRow>, LogError> {
    let where_clause = match filter {
        LogListFilter::Default => "where m.state in ('pending', 'failed', 'skipped')",
        LogListFilter::Pending => "where m.state = 'pending'",
        LogListFilter::Failed => "where m.state = 'failed'",
        LogListFilter::Skipped => "where m.state = 'skipped'",
    };
    let sql = format!(
        "select m.sequence, m.state, m.mutation_type, i.display_value, m.created_at, m.failure_json \
           from mutations m \
           join items i on i.id = m.item_id and i.item_class = m.item_class \
          {where_clause} \
          order by m.sequence asc"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;

    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let raw_failure: Option<String> = row.get(5)?;
        let failure_detail = raw_failure
            .map(|raw| decode_failure_detail(&raw))
            .transpose()?;
        out.push(LogListRow {
            sequence: row.get(0)?,
            state: row.get(1)?,
            mutation_type: row.get(2)?,
            target_display_id: row.get(3)?,
            created_at: row.get(4)?,
            failure_detail,
        });
    }
    Ok(out)
}

/// Look up one Mutation Log entry by sequence and return its full detail.
pub fn show_mutation_log(conn: &Connection, sequence: i64) -> Result<LogDetailRow, LogError> {
    // The closure populates `failure_detail` with the raw `failure_json`
    // wrapper; it is decoded to the bare detail after the query because the
    // rusqlite row closure can only surface `rusqlite::Error`, not `LogError`.
    let mut detail = conn
        .query_row(
            "select m.sequence, m.state, m.mutation_type, i.display_value, \
                    m.item_class, m.payload_json, m.failure_json, \
                    m.created_at, m.state_changed_at \
               from mutations m \
               join items i on i.id = m.item_id and i.item_class = m.item_class \
              where m.sequence = ?1",
            params![sequence],
            |r| {
                Ok(LogDetailRow {
                    sequence: r.get(0)?,
                    state: r.get(1)?,
                    mutation_type: r.get(2)?,
                    target_display_id: r.get(3)?,
                    item_class: r.get(4)?,
                    payload_json: r.get(5)?,
                    failure_detail: r.get(6)?,
                    created_at: r.get(7)?,
                    state_changed_at: r.get(8)?,
                })
            },
        )
        .optional()?
        .ok_or(LogError::MutationNotFound(sequence))?;

    if let Some(raw) = detail.failure_detail.take() {
        detail.failure_detail = Some(decode_failure_detail(&raw)?);
    }
    Ok(detail)
}

/// Decode the `failure_json` text column into the bare detail string — the
/// inverse of the [`FailureJsonWrapper`] encoder.
fn decode_failure_detail(raw: &str) -> Result<String, LogError> {
    let wrapper: FailureJsonWrapper = serde_json::from_str(raw)?;
    Ok(wrapper.detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::status::ItemStatus;
    use crate::domain::ticket_kind::TicketKind;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, insert_fixture_item, insert_fixture_mutation,
        insert_fixture_remote,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn open_seeded() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn
    }

    fn snapshot(
        backend_key: &str,
        display_id: &str,
        class: ItemClass,
        title: &str,
        status: ItemStatus,
    ) -> BackendItemSnapshot {
        BackendItemSnapshot {
            backend_kind: "github".into(),
            backend_key: backend_key.into(),
            display_id: display_id.into(),
            item_class: class,
            ticket_kind: if class == ItemClass::Ticket {
                Some(TicketKind::Task)
            } else {
                None
            },
            title: title.into(),
            body: "Body".into(),
            status,
            backend_updated_at: "2026-05-19T00:00:00Z".into(),
        }
    }

    fn backend_ticket(conn: &Connection, id: &str, display: &str, key: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Old",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some(key),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    // ---- merge ----------------------------------------------------------

    #[test]
    fn merge_scenario_a_inserts_new_backend_item() {
        let mut conn = open_seeded();
        let mut rng = StdRng::seed_from_u64(0);
        merge_backend_snapshots(
            &mut conn,
            &mut rng,
            &[snapshot(
                "1",
                "gh-1",
                ItemClass::Ticket,
                "First",
                ItemStatus::Open,
            )],
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (title, origin, kind, source): (String, String, String, String) = conn
            .query_row(
                "select i.title, i.origin, i.backend_kind, \
                        (select source from item_ids where value = i.display_value) \
                   from items i where i.display_value = 'gh-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(title, "First");
        assert_eq!(origin, "backend");
        assert_eq!(kind, "github");
        assert_eq!(source, "display");
    }

    #[test]
    fn merge_scenario_b_skips_item_with_pending_mutation() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local Edit","body":""}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        // Local title set to the in-flight edit.
        conn.execute("update items set title = 'Local Edit' where id = 't1'", [])
            .unwrap();

        let mut rng = StdRng::seed_from_u64(0);
        merge_backend_snapshots(
            &mut conn,
            &mut rng,
            &[snapshot(
                "1",
                "gh-1",
                ItemClass::Ticket,
                "Stale Backend View",
                ItemStatus::Open,
            )],
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let title: String = conn
            .query_row("select title from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            title, "Local Edit",
            "pending mutation must shield the local edit"
        );
    }

    #[test]
    fn merge_scenario_c_updates_item_without_in_flight_mutation() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);

        let mut rng = StdRng::seed_from_u64(0);
        merge_backend_snapshots(
            &mut conn,
            &mut rng,
            &[snapshot(
                "1",
                "gh-1",
                ItemClass::Ticket,
                "Backend Wins",
                ItemStatus::Active,
            )],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let (title, status, updated): (String, String, String) = conn
            .query_row(
                "select title, status, updated_at from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Backend Wins");
        assert_eq!(status, "active");
        assert_eq!(updated, "2026-05-20T00:00:00Z");
    }

    #[test]
    fn merge_display_id_collision_is_surfaced_and_rolls_back() {
        let mut conn = open_seeded();
        // A local item already owns Display ID "gh-1".
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "local1",
                display: "gh-1",
                title: "Local",
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let mut rng = StdRng::seed_from_u64(0);
        // A *different* backend item claims the same Display ID.
        let err = merge_backend_snapshots(
            &mut conn,
            &mut rng,
            &[snapshot(
                "99",
                "gh-1",
                ItemClass::Ticket,
                "Backend",
                ItemStatus::Open,
            )],
            "2026-05-19T00:00:00Z",
        )
        .unwrap_err();
        match err {
            MergeError::DisplayIdCollision(id) => assert_eq!(id, "gh-1"),
            other => panic!("expected DisplayIdCollision, got {other:?}"),
        }
        // Rollback: no orphaned backend item landed.
        let count: i64 = conn
            .query_row(
                "select count(*) from items where backend_key = '99'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    // ---- load_applicable_mutations --------------------------------------

    #[test]
    fn load_applicable_returns_pending_and_failed_in_sequence_order() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        for (seq, state) in [
            (3, "pending"),
            (1, "failed"),
            (2, "applied"),
            (4, "skipped"),
        ] {
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence: seq,
                    mutation_type: "update_ticket",
                    item_id: "t1",
                    payload_json: r#"{"title":"X","body":""}"#,
                    state,
                    failure_json: if state == "failed" {
                        Some(r#"{"detail":"prior"}"#)
                    } else {
                        None
                    },
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
        }

        let views = load_applicable_mutations(&conn).unwrap();
        let seqs: Vec<i64> = views.iter().map(|v| v.sequence).collect();
        assert_eq!(seqs, vec![1, 3], "only pending+failed, sequence order");
    }

    #[test]
    fn load_applicable_decodes_each_payload_variant() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let views = load_applicable_mutations(&conn).unwrap();
        assert_eq!(views.len(), 1);
        match &views[0].payload {
            MutationPayload::ItemStatus(s) => assert_eq!(s.status, "done"),
            other => panic!("expected ItemStatus, got {other:?}"),
        }
        assert_eq!(views[0].backend_kind.as_deref(), Some("github"));
        assert_eq!(views[0].backend_key.as_deref(), Some("1"));
    }

    #[test]
    fn load_applicable_rejects_payload_variant_missing() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: "{}",
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        match load_applicable_mutations(&conn).unwrap_err() {
            LoadApplicableError::PayloadVariantMissing(MutationType::PromoteTicket) => {}
            other => panic!("expected PayloadVariantMissing, got {other:?}"),
        }
    }

    // ---- apply_mutation_outcome -----------------------------------------

    fn seed_remote(conn: &Connection) {
        insert_fixture_remote(conn, FixtureRemote::default()).unwrap();
    }

    fn seed_pending(conn: &Connection, sequence: i64) {
        backend_ticket(conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"New","body":""}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn apply_outcome_pending_success_applies_and_advances_cursor() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending(&conn, 5);

        apply_mutation_outcome(
            &mut conn,
            5,
            &ApplyOutcome::accepted(),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure): (String, Option<String>) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 5",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "applied");
        assert_eq!(failure, None);

        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 5);
    }

    #[test]
    fn apply_outcome_pending_failure_records_detail() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending(&conn, 1);

        apply_mutation_outcome(
            &mut conn,
            1,
            &ApplyOutcome::rejected("HTTP 422: title required"),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure): (String, String) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "failed");
        assert!(failure.contains("title required"));
    }

    #[test]
    fn apply_outcome_failed_success_clears_failure_and_applies() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 3,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"prior"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        apply_mutation_outcome(
            &mut conn,
            3,
            &ApplyOutcome::accepted(),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure): (String, Option<String>) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "applied");
        assert_eq!(failure, None);
    }

    #[test]
    fn apply_outcome_failed_failure_keeps_state_refreshes_detail() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"old reason"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        apply_mutation_outcome(
            &mut conn,
            2,
            &ApplyOutcome::rejected("new reason"),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure): (String, String) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "failed");
        assert!(failure.contains("new reason"));
        assert!(!failure.contains("old reason"));
    }

    #[test]
    fn apply_outcome_missing_row_returns_not_found() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        match apply_mutation_outcome(
            &mut conn,
            999,
            &ApplyOutcome::accepted(),
            "2026-05-19T00:00:00Z",
        )
        .unwrap_err()
        {
            ApplyMutationOutcomeError::MutationNotFound(999) => {}
            other => panic!("expected MutationNotFound, got {other:?}"),
        }
    }

    #[test]
    fn apply_outcome_terminal_state_returns_not_applicable() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 7,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "applied",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        match apply_mutation_outcome(
            &mut conn,
            7,
            &ApplyOutcome::accepted(),
            "2026-05-19T00:00:00Z",
        )
        .unwrap_err()
        {
            ApplyMutationOutcomeError::MutationNotApplicable(7) => {}
            other => panic!("expected MutationNotApplicable, got {other:?}"),
        }
    }

    // ---- mark_mutation_skipped ------------------------------------------

    #[test]
    fn mark_skipped_transitions_failed_to_skipped() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        mark_mutation_skipped(&mut conn, 1, "2026-05-19T00:00:00Z").unwrap();

        let (state, failure): (String, String) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "skipped");
        assert!(failure.contains("rejected"), "audit trail preserved");
    }

    #[test]
    fn mark_skipped_refuses_non_failed() {
        let mut conn = open_seeded();
        seed_pending(&conn, 1);
        match mark_mutation_skipped(&mut conn, 1, "2026-05-19T00:00:00Z").unwrap_err() {
            MarkSkippedError::MutationNotFailed(1) => {}
            other => panic!("expected MutationNotFailed, got {other:?}"),
        }
    }

    #[test]
    fn mark_skipped_missing_row_returns_not_found() {
        let mut conn = open_seeded();
        match mark_mutation_skipped(&mut conn, 42, "2026-05-19T00:00:00Z").unwrap_err() {
            MarkSkippedError::MutationNotFound(42) => {}
            other => panic!("expected MutationNotFound, got {other:?}"),
        }
    }

    // ---- get_remote / count ---------------------------------------------

    #[test]
    fn get_remote_returns_none_when_unconfigured() {
        let conn = open_seeded();
        assert_eq!(get_remote(&conn).unwrap(), None);
    }

    #[test]
    fn get_remote_returns_configured_row_with_cursor() {
        let conn = open_seeded();
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "github",
                config_json: r#"{"repo":"o/r"}"#,
                last_applied_sequence: 9,
                ..FixtureRemote::default()
            },
        )
        .unwrap();

        let row = get_remote(&conn).unwrap().unwrap();
        assert_eq!(row.backend_kind, "github");
        assert_eq!(row.config_json, r#"{"repo":"o/r"}"#);
        assert_eq!(row.last_applied_sequence, 9);
    }

    #[test]
    fn pending_or_failed_count_counts_only_in_flight() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        for (seq, state) in [
            (1, "pending"),
            (2, "failed"),
            (3, "applied"),
            (4, "skipped"),
        ] {
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence: seq,
                    mutation_type: "update_ticket",
                    item_id: "t1",
                    payload_json: r#"{"title":"X","body":""}"#,
                    state,
                    failure_json: if state == "failed" {
                        Some(r#"{"detail":"x"}"#)
                    } else {
                        None
                    },
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
        }
        assert_eq!(pending_or_failed_mutation_count(&conn).unwrap(), 2);
    }

    // ---- log read -------------------------------------------------------

    fn seed_log_fixture(conn: &Connection) {
        backend_ticket(conn, "t1", "gh-1", "1", 1);
        backend_ticket(conn, "t2", "gh-2", "2", 2);
        backend_ticket(conn, "t3", "gh-3", "3", 3);
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "set_item_status",
                item_id: "t2",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"HTTP 422: rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 3,
                mutation_type: "update_ticket",
                item_id: "t3",
                payload_json: r#"{"title":"C","body":""}"#,
                state: "skipped",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn list_default_returns_pending_failed_skipped_with_failure_detail() {
        let conn = open_seeded();
        seed_log_fixture(&conn);

        let rows = list_mutation_log(&conn, LogListFilter::Default).unwrap();
        let seqs: Vec<i64> = rows.iter().map(|r| r.sequence).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert_eq!(rows[0].failure_detail, None);
        assert_eq!(
            rows[1].failure_detail.as_deref(),
            Some("HTTP 422: rejected")
        );
        assert_eq!(rows[1].target_display_id, "gh-2");
    }

    #[test]
    fn list_filters_by_state() {
        let conn = open_seeded();
        seed_log_fixture(&conn);

        assert_eq!(
            list_mutation_log(&conn, LogListFilter::Pending)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_mutation_log(&conn, LogListFilter::Failed)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_mutation_log(&conn, LogListFilter::Skipped)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn show_returns_detail_with_decoded_failure() {
        let conn = open_seeded();
        seed_log_fixture(&conn);

        let detail = show_mutation_log(&conn, 2).unwrap();
        assert_eq!(detail.sequence, 2);
        assert_eq!(detail.state, MutationState::Failed);
        assert_eq!(detail.mutation_type, MutationType::SetItemStatus);
        assert_eq!(detail.target_display_id, "gh-2");
        assert_eq!(detail.item_class, ItemClass::Ticket);
        assert_eq!(detail.payload_json, r#"{"status":"done"}"#);
        assert_eq!(detail.failure_detail.as_deref(), Some("HTTP 422: rejected"));
    }

    #[test]
    fn show_missing_returns_not_found() {
        let conn = open_seeded();
        match show_mutation_log(&conn, 999).unwrap_err() {
            LogError::MutationNotFound(999) => {}
            other => panic!("expected MutationNotFound, got {other:?}"),
        }
    }
}
