//! Sync engine orchestration.
//!
//! [`run_sync`] is the engine entry point shared by `tk sync` and `tk promote`.
//! It composes the backend-blind [`Adapter`] trait with the
//! SQL helpers in [`crate::store::sync`]:
//!
//! 1. Pull. The engine derives the Adopted working set's active backend keys
//!    ([`active_backend_keys`]) and refreshes each through [`Adapter::refresh_item`].
//!    It collects every result before the single Store merge transaction.
//!    The merge transaction is skipped when the Pull is empty so an idle sync
//!    takes no write lock.
//! 2. Apply loop. [`load_applicable_mutations`] decodes the pending+failed
//!    rows in sequence order; [`resolve_mutation_view`] binds each one's
//!    backend identity in turn, immediately before it is handed to
//!    [`Adapter::apply_mutation`] and persisted via [`apply_mutation_outcome`].
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
use crate::remote::adapter::{Adapter, AdapterReadError, ApplyError};
use crate::store::sync::{
    ApplyMutationOutcomeError, LoadApplicableError, RefreshStoreError, active_backend_keys,
    apply_mutation_outcome, load_applicable_mutations, merge_backend_refreshes,
    resolve_mutation_view,
};

/// Summary of one sync run for the calling command to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Number of Backend Items refreshed during Pull.
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
    /// Pull failed: adapter unavailable ([`AdapterReadError::Env`]) or backend
    /// rejection ([`AdapterReadError::Failed`] carrying captured stderr).
    #[error(transparent)]
    Pull(#[from] AdapterReadError),
    /// Apply hit an environment failure (backend CLI missing / spawn failed);
    /// the in-flight Mutation row is left `pending`.
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error(transparent)]
    Refresh(#[from] RefreshStoreError),
    #[error(transparent)]
    Load(#[from] LoadApplicableError),
    #[error(transparent)]
    Outcome(#[from] ApplyMutationOutcomeError),
}

/// Run one sync against a configured Adapter.
///
/// `now` is the injected timestamp written to every row this run touches.
pub fn run_sync(
    conn: &mut Connection,
    adapter: &mut dyn Adapter,
    now: &str,
) -> Result<SyncReport, RunSyncError> {
    let mut report = SyncReport {
        pulled_count: 0,
        applied_count: 0,
        stopped_at_sequence: None,
    };

    // Pull and merge. The engine derives the Adopted working set's active keys
    // and the adapter fetches exactly those (ADR-0034 opt-in refresh-by-key);
    // an empty set means no backend call. A storage fault deriving the keys is
    // a pull-side store error, surfaced through the merge boundary.
    let kind = adapter.backend_kind();
    let keys = active_backend_keys(conn, kind)?;
    let mut refreshes = Vec::with_capacity(keys.len());
    for key in keys {
        let refresh = adapter.refresh_item(&key)?;
        refreshes.push((key, refresh));
    }
    report.pulled_count = refreshes.len();
    if !refreshes.is_empty() {
        merge_backend_refreshes(conn, kind, &refreshes, now)?;
    }

    // Apply loop. An environment failure from `apply_mutation` bubbles via `?`
    // and leaves the row `pending` (no outcome persisted); a per-Mutation
    // rejection is persisted and stops the loop.
    //
    // Decode is batched (one undecodable row fails the run before any backend
    // write); backend identity is resolved per Mutation, immediately before
    // Apply, so a Promotion receipt committed earlier in this run is visible to
    // the Mutations ordered behind it (ADR-0036).
    let rows = load_applicable_mutations(conn)?;
    for row in &rows {
        let view = resolve_mutation_view(conn, row)?;
        let outcome = adapter.apply_mutation(&view, now)?;
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
    use crate::domain::backend_kind::BackendKind;
    use crate::domain::backend_operation::BackendItemRefresh;
    use crate::domain::status::ItemStatus;
    use crate::domain::ticket_kind::TicketKind;
    use crate::proc::ProcError;
    use crate::remote::fake::{ApplyResponse, FakeAdapter, RefreshResponse};
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, insert_fixture_item, insert_fixture_mutation,
        insert_fixture_remote,
    };

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

    fn refresh(title: &str) -> RefreshResponse {
        RefreshResponse::Item(BackendItemRefresh {
            ticket_kind: Some(TicketKind::Task),
            title: title.into(),
            body: String::new(),
            status: ItemStatus::Open,
        })
    }

    fn fake(refreshes: Vec<RefreshResponse>, applies: Vec<ApplyResponse>) -> FakeAdapter {
        FakeAdapter::directional(vec![], refreshes, applies)
    }

    fn run(conn: &mut Connection, fake: &mut FakeAdapter) -> Result<SyncReport, RunSyncError> {
        run_sync(conn, fake, NOW)
    }

    #[test]
    fn empty_queue_and_empty_pull_is_a_noop() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        let mut fake = fake(vec![], vec![]);

        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.pulled_count, 0);
        assert_eq!(report.applied_count, 0);
        assert_eq!(report.stopped_at_sequence, None);
    }

    #[test]
    fn mixed_backend_cohort_is_rejected_before_any_adapter_call() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t2",
                display: "tk-2",
                title: "Local",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_remote(&conn);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t2",
                payload_json: r#"{"title":"T","body":"","backend_kind":"jira"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let mut fake = fake(vec![], vec![]);

        let error = run(&mut conn, &mut fake).unwrap_err();

        assert!(matches!(
            error,
            RunSyncError::Refresh(RefreshStoreError::BackendCohort(
                crate::store::sync::BackendCohortError::MultipleBackendKinds
            ))
        ));
        assert!(fake.captured_refresh_keys.is_empty());
        assert!(fake.captured_applies.is_empty());
    }

    #[test]
    fn retained_promotion_for_another_backend_is_rejected_before_any_adapter_call() {
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_remote(&conn);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"T","body":"","backend_kind":"jira"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let mut fake = fake(vec![], vec![]);

        let error = run(&mut conn, &mut fake).unwrap_err();

        assert!(matches!(
            error,
            RunSyncError::Refresh(RefreshStoreError::BackendCohort(
                crate::store::sync::BackendCohortError::BackendKindMismatch {
                    expected: BackendKind::Github,
                    retained: BackendKind::Jira,
                }
            ))
        ));
        assert!(fake.captured_refresh_keys.is_empty());
        assert!(fake.captured_applies.is_empty());
    }

    #[test]
    fn pull_refreshes_an_adopted_backend_item() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-42", "42", 1);
        seed_remote(&conn);
        let mut fake = fake(vec![refresh("Refreshed")], vec![]);

        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.pulled_count, 1);

        // The engine derived the active Adopted key set and asked for exactly it.
        assert_eq!(fake.captured_refresh_keys, vec!["42".to_string()]);

        // The known row was refreshed in place.
        let title: String = conn
            .query_row(
                "select title from items where backend_key = '42'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "Refreshed");
    }

    #[test]
    fn pull_requests_only_active_adopted_keys() {
        let mut conn = open_seeded();
        // Adopted and open -> in the refresh set.
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        // Adopted but done -> terminal, excluded (ADR-0034).
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t2",
                display: "gh-2",
                title: "Closed",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("2"),
                status: "done",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        // Local item -> no backend key, excluded.
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t3",
                display: "tk-3",
                title: "Local",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_remote(&conn);

        let mut fake = fake(vec![refresh("Old")], vec![]);
        run(&mut conn, &mut fake).unwrap();

        assert_eq!(fake.captured_refresh_keys, vec!["1".to_string()]);
    }

    #[test]
    fn apply_success_transitions_and_advances_cursor() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 5, "t1", "New");

        let mut fake = fake(vec![refresh("Old")], vec![ApplyResponse::Success]);
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

        let mut fake = fake(
            vec![refresh("Old 1"), refresh("Old 2")],
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
        assert_eq!(fake.captured_applies.len(), 1);
    }

    #[test]
    fn apply_env_failure_propagates_and_leaves_row_pending() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "A");

        let mut fake = fake(
            vec![refresh("Old")],
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
    fn later_refresh_failure_prevents_all_refreshes_from_merging_and_skips_apply() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        backend_ticket(&conn, "t2", "gh-2", "2", 2);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "A");

        let mut fake = fake(
            vec![
                refresh("Should Not Merge"),
                RefreshResponse::RecordedFailure("gh: HTTP 502".into()),
            ],
            vec![],
        );
        let err = run(&mut conn, &mut fake).unwrap_err();
        match err {
            RunSyncError::Pull(AdapterReadError::Failed(detail)) => {
                assert!(detail.contains("HTTP 502"));
            }
            other => panic!("expected Pull(Failed), got {other:?}"),
        }

        // Apply never invoked; row still pending.
        assert!(fake.captured_applies.is_empty());
        assert_eq!(fake.captured_refresh_keys, ["1", "2"]);
        let title: String = conn
            .query_row("select title from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Old", "the earlier refresh was not merged");
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

        let mut fake = fake(vec![refresh("Old")], vec![ApplyResponse::Success]);
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
    fn pull_refresh_for_item_with_pending_mutation_is_skipped() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "Local Edit");
        conn.execute("update items set title = 'Local Edit' where id = 't1'", [])
            .unwrap();

        // Pull returns a stale backend view; apply the in-flight mutation.
        let mut fake = fake(
            vec![refresh("Stale Backend View")],
            vec![ApplyResponse::Success],
        );
        run(&mut conn, &mut fake).unwrap();

        // The pending Mutation shielded the local edit from the stale Pull.
        let title: String = conn
            .query_row("select title from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Local Edit");
    }

    #[test]
    fn a_receipt_supplies_the_identity_a_later_mutation_in_the_same_run_needs() {
        // The point of resolving identity per Mutation (ADR-0036): the
        // `set_item_status` behind the Promotion targets an Item that was still
        // Local when the run started, and reaches the backend only because the
        // Promotion's receipt landed first.
        let mut conn = open_seeded();
        seed_remote(&conn);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "pending",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let mut fake = fake(
            vec![],
            vec![
                ApplyResponse::PromotionSuccess {
                    backend_key: "42".into(),
                    display_id: "gh-42".into(),
                },
                ApplyResponse::Success,
            ],
        );
        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.applied_count, 2);

        assert_eq!(
            fake.captured_applies[0].backend_key, None,
            "the Promotion itself targets a Local Item"
        );
        assert_eq!(
            fake.captured_applies[1].backend_key.as_deref(),
            Some("42"),
            "the following Mutation saw the identity the receipt assigned"
        );

        let (display, origin, key): (String, String, String) = conn
            .query_row(
                "select display_value, origin, backend_key from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(display, "gh-42");
        assert_eq!(origin, "backend");
        assert_eq!(key, "42");
    }

    #[test]
    fn an_undecodable_applicable_row_fails_the_run_before_any_apply() {
        // Decode stays batched so a Mutation Log row this build cannot project
        // stops the run before any backend write, including the applicable rows
        // ordered ahead of it.
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "A");
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "add_external_blocker",
                item_id: "t1",
                payload_json: "{}",
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let mut fake = fake(vec![refresh("Old")], vec![]);
        let err = run(&mut conn, &mut fake).unwrap_err();
        assert!(
            matches!(
                err,
                RunSyncError::Load(LoadApplicableError::PayloadVariantMissing(
                    crate::domain::mutation_type::MutationType::AddExternalBlocker
                ))
            ),
            "expected the decode to fail the run, got {err:?}"
        );

        assert!(
            fake.captured_applies.is_empty(),
            "no backend write happened"
        );
        let pending: i64 = conn
            .query_row(
                "select count(*) from mutations where state = 'pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 2, "both rows still applicable");
    }

    #[test]
    fn multiple_apply_successes_advance_cursor_to_last() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        backend_ticket(&conn, "t2", "gh-2", "2", 2);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "A");
        update_ticket_mutation(&conn, 2, "t2", "B");

        let mut fake = fake(
            vec![refresh("Old 1"), refresh("Old 2")],
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
        assert_eq!(fake.captured_applies.len(), 2);
    }
}
