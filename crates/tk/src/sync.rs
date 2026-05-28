//! Sync engine orchestration.
//!
//! [`run_sync`] is the single entry point for `tk sync` (and, later, the
//! Promote flow). It composes the backend-blind [`Adapter`] trait with the
//! SQL helpers in [`crate::store::sync`]:
//!
//! 1. Pull. [`Adapter::pull_backend_items`] returns a snapshot slice (or
//!    [`PullError::Failed`] carrying captured stderr), which the engine merges
//!    via [`merge_backend_snapshots`]. The merge transaction is skipped when
//!    the Pull is empty so an idle sync takes no write lock.
//! 2. Apply loop. [`load_applicable_mutations`] returns pending+failed
//!    [`MutationView`]s in sequence order. Each is handed to
//!    [`Adapter::apply_mutation`], then persisted via [`apply_mutation_outcome`].
//!    The loop stops at the first [`ApplyOutcome::Rejected`].
//!
//! `tk sync --skip <id>` does NOT pass through the engine — the command calls
//! [`crate::store::sync::mark_mutation_skipped`] directly, before opening the
//! adapter, so a skip persists even when the Remote's adapter is unavailable.
//!
//! The engine is backend-blind: the Adapter trait is its only seam.

use rusqlite::Connection;
use thiserror::Error;

use crate::domain::apply_outcome::ApplyOutcome;
use crate::remote::adapter::{Adapter, ApplyError, PullError};
use crate::store::sync::{
    ApplyMutationOutcomeError, LoadApplicableError, MergeError, apply_mutation_outcome,
    load_applicable_mutations, merge_backend_snapshots,
};

/// Summary of one sync run for the calling command to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Number of `BackendItemSnapshot`s the adapter returned from Pull.
    pub pulled_count: usize,
    /// Number of Mutations that transitioned to `applied` during this run.
    pub applied_count: usize,
    /// When `Some`, the sync stopped because this Mutation's Apply returned
    /// [`ApplyOutcome::Rejected`]; the `mutations.failure_json` row records the
    /// detail and the caller renders the sequence.
    pub stopped_at_sequence: Option<i64>,
}

/// Error returned by [`run_sync`].
///
/// One enum unioning the Adapter trait's error sets, the merge/load SQL
/// boundary, and the outcome-persistence boundary. Catastrophic environment
/// failures and Pull failures bubble out unchanged so `tk sync` can dispatch
/// on the variant for its stderr rendering. The per-Mutation rejection that
/// stops the loop is NOT an error here — it surfaces through
/// [`SyncReport::stopped_at_sequence`].
#[derive(Debug, Error)]
pub enum RunSyncError {
    /// Pull failed: adapter unavailable ([`PullError::Env`]) or backend
    /// rejection ([`PullError::Failed`] carrying captured stderr).
    #[error(transparent)]
    Pull(#[from] PullError),
    /// Apply hit an environment failure (backend CLI missing / spawn failed);
    /// the in-flight Mutation row is left `pending`.
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error(transparent)]
    Merge(#[from] MergeError),
    #[error(transparent)]
    Load(#[from] LoadApplicableError),
    #[error(transparent)]
    Outcome(#[from] ApplyMutationOutcomeError),
}

/// Run one sync against a configured Adapter.
///
/// `rng` supplies entropy for the internal `items.id` of any backend Item the
/// Pull discovers (see [`merge_backend_snapshots`]); `now` is the injected
/// timestamp written to every row this run touches.
pub fn run_sync(
    conn: &mut Connection,
    adapter: &mut dyn Adapter,
    now: &str,
    rng: &mut dyn rand::Rng,
) -> Result<SyncReport, RunSyncError> {
    let mut report = SyncReport {
        pulled_count: 0,
        applied_count: 0,
        stopped_at_sequence: None,
    };

    // Pull and merge.
    let snapshots = adapter.pull_backend_items()?;
    report.pulled_count = snapshots.len();
    if !snapshots.is_empty() {
        merge_backend_snapshots(conn, rng, &snapshots, now)?;
    }

    // Apply loop. An environment failure from `apply_mutation` bubbles via `?`
    // and leaves the row `pending` (no outcome persisted); a per-Mutation
    // rejection is persisted and stops the loop.
    let views = load_applicable_mutations(conn)?;
    for view in &views {
        let outcome = adapter.apply_mutation(view, now)?;
        apply_mutation_outcome(conn, view.sequence, &outcome, now)?;
        match outcome {
            ApplyOutcome::Accepted(_) => report.applied_count += 1,
            ApplyOutcome::Rejected(_) => {
                report.stopped_at_sequence = Some(view.sequence);
                return Ok(report);
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_item_snapshot::BackendItemSnapshot;
    use crate::domain::item_class::ItemClass;
    use crate::domain::status::ItemStatus;
    use crate::domain::ticket_kind::TicketKind;
    use crate::proc::ProcError;
    use crate::remote::fake::{ApplyResponse, FakeAdapter, PullResponse};
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, insert_fixture_item, insert_fixture_mutation,
        insert_fixture_remote,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    const NOW: &str = "2026-05-19T00:00:00Z";

    fn open_seeded() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn
    }

    fn seed_remote(conn: &Connection) {
        insert_fixture_remote(conn, FixtureRemote::default()).unwrap();
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

    fn update_ticket_mutation(conn: &Connection, sequence: i64, item_id: &str, title: &str) {
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence,
                mutation_type: "update_ticket",
                item_id,
                payload_json: &format!(r#"{{"title":"{title}","body":""}}"#),
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    fn snapshot(backend_key: &str, display_id: &str, title: &str) -> BackendItemSnapshot {
        BackendItemSnapshot {
            backend_kind: "github".into(),
            backend_key: backend_key.into(),
            display_id: display_id.into(),
            item_class: ItemClass::Ticket,
            ticket_kind: Some(TicketKind::Task),
            title: title.into(),
            body: String::new(),
            status: ItemStatus::Open,
            backend_updated_at: NOW.into(),
        }
    }

    fn run(conn: &mut Connection, fake: &mut FakeAdapter) -> Result<SyncReport, RunSyncError> {
        let mut rng = StdRng::seed_from_u64(0);
        run_sync(conn, fake, NOW, &mut rng)
    }

    #[test]
    fn empty_queue_and_empty_pull_is_a_noop() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        let mut fake = FakeAdapter::new(vec![PullResponse::Snapshots(vec![])], vec![]);

        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.pulled_count, 0);
        assert_eq!(report.applied_count, 0);
        assert_eq!(report.stopped_at_sequence, None);
    }

    #[test]
    fn pull_inserts_a_discovered_backend_item() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        let mut fake = FakeAdapter::new(
            vec![PullResponse::Snapshots(vec![snapshot(
                "42",
                "gh-42",
                "Discovered",
            )])],
            vec![],
        );

        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.pulled_count, 1);

        let title: String = conn
            .query_row(
                "select title from items where backend_kind = 'github' and backend_key = '42'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "Discovered");
    }

    #[test]
    fn apply_success_transitions_and_advances_cursor() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 5, "t1", "New");

        let mut fake = FakeAdapter::new(
            vec![PullResponse::Snapshots(vec![])],
            vec![ApplyResponse::Success],
        );
        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.applied_count, 1);
        assert_eq!(report.stopped_at_sequence, None);

        let state: String = conn
            .query_row("select state from mutations where sequence = 5", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "applied");

        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 5);

        // Fake saw the decoded payload.
        assert_eq!(fake.captured_applies.len(), 1);
        assert_eq!(fake.captured_applies[0].sequence, 5);
        assert!(
            fake.captured_applies[0]
                .payload_text
                .contains(r#""title":"New""#)
        );
    }

    #[test]
    fn apply_recorded_failure_transitions_to_failed_and_stops() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        backend_ticket(&conn, "t2", "gh-2", "2", 2);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "A");
        update_ticket_mutation(&conn, 2, "t2", "B");

        let mut fake = FakeAdapter::new(
            vec![PullResponse::Snapshots(vec![])],
            vec![ApplyResponse::RecordedFailure(
                "HTTP 422: title required".into(),
            )],
        );
        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.applied_count, 0);
        assert_eq!(report.stopped_at_sequence, Some(1));

        let (state1, failure1): (String, String) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state1, "failed");
        assert!(failure1.contains("title required"));

        let state2: String = conn
            .query_row("select state from mutations where sequence = 2", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state2, "pending", "loop stopped before sequence 2");

        // Only one apply consumed.
        assert_eq!(fake.apply_index, 1);
    }

    #[test]
    fn apply_env_failure_propagates_and_leaves_row_pending() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "A");

        let mut fake = FakeAdapter::new(
            vec![PullResponse::Snapshots(vec![])],
            vec![ApplyResponse::EnvFailure(ProcError::ExecutableNotFound)],
        );
        let err = run(&mut conn, &mut fake).unwrap_err();
        assert!(matches!(
            err,
            RunSyncError::Apply(ProcError::ExecutableNotFound)
        ));

        let state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "pending", "engine wrote no outcome");
    }

    #[test]
    fn pull_recorded_failure_propagates_and_skips_apply() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "A");

        let mut fake = FakeAdapter::new(
            vec![PullResponse::RecordedFailure("gh: HTTP 502".into())],
            vec![],
        );
        let err = run(&mut conn, &mut fake).unwrap_err();
        match err {
            RunSyncError::Pull(PullError::Failed(detail)) => assert!(detail.contains("HTTP 502")),
            other => panic!("expected Pull(Failed), got {other:?}"),
        }

        // Apply never invoked; row still pending.
        assert!(fake.captured_applies.is_empty());
        let state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "pending");
    }

    #[test]
    fn failed_mutation_retried_successfully_transitions_to_applied() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
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

        let mut fake = FakeAdapter::new(
            vec![PullResponse::Snapshots(vec![])],
            vec![ApplyResponse::Success],
        );
        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.applied_count, 1);

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
    fn pull_snapshot_for_item_with_pending_mutation_is_skipped() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "Local Edit");
        conn.execute("update items set title = 'Local Edit' where id = 't1'", [])
            .unwrap();

        // Pull returns a stale backend view; apply the in-flight mutation.
        let mut fake = FakeAdapter::new(
            vec![PullResponse::Snapshots(vec![snapshot(
                "1",
                "gh-1",
                "Stale Backend View",
            )])],
            vec![ApplyResponse::Success],
        );
        run(&mut conn, &mut fake).unwrap();

        // Merge's scenario B shielded the local edit from the stale Pull.
        let title: String = conn
            .query_row("select title from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Local Edit");
    }

    #[test]
    fn multiple_apply_successes_advance_cursor_to_last() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        backend_ticket(&conn, "t2", "gh-2", "2", 2);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "A");
        update_ticket_mutation(&conn, 2, "t2", "B");

        let mut fake = FakeAdapter::new(
            vec![PullResponse::Snapshots(vec![])],
            vec![ApplyResponse::Success, ApplyResponse::Success],
        );
        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.applied_count, 2);
        assert_eq!(report.stopped_at_sequence, None);

        let applied: i64 = conn
            .query_row(
                "select count(*) from mutations where state = 'applied'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(applied, 2);

        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 2);
        assert_eq!(fake.apply_index, 2);
    }
}
