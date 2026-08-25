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
//!    rows in sequence order; [`resolve_backend_operation`] classifies and
//!    binds each one immediately before delivery. Edits and creation use
//!    distinct Adapter and Store contracts.
//!
//! `tk sync --skip <id>` does NOT pass through the engine — the command calls
//! [`crate::store::sync::mark_mutation_skipped`] directly under the Remote
//! workflow guard, before opening the adapter, so a skip persists even when
//! the Remote's adapter is unavailable.
//!
//! The engine is backend-blind: the Adapter trait is its only seam.

use rusqlite::Connection;
use thiserror::Error;

use crate::domain::backend_operation::{BackendItemIdentity, BackendOperation};
use crate::domain::backend_outcome::{BackendCreateOutcome, BackendEditOutcome};
use crate::remote::adapter::{Adapter, AdapterReadError, ApplyError};
use crate::store::repository::RemoteWorkflowGuard;
use crate::store::sync::{
    BackendCohortError, LoadApplicableError, PersistMutationOutcomeError, RefreshStoreError,
    active_backend_keys, applying_mutation_sequence, begin_create, load_applicable_mutations,
    merge_backend_refreshes, persist_create_outcome, persist_edit_outcome,
    resolve_backend_operation,
};

/// Summary of one sync run for the calling command to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Number of Backend Items refreshed during Pull.
    pub pulled_count: usize,
    /// Number of Mutations that transitioned to `applied` during this run.
    pub applied_count: usize,
    /// When `Some`, the sync stopped because this Mutation's Apply returned a
    /// certified Backend rejection; `failure_json` records the detail and the
    /// caller renders the sequence. Indeterminate creation surfaces through
    /// [`RunSyncError::ApplyingMutation`].
    pub stopped_at_sequence: Option<i64>,
}

/// Error returned by [`run_sync`].
///
/// One enum unioning the Adapter trait's error sets, the merge/load SQL
/// boundary, and the outcome-persistence boundary. Environment failures, Pull
/// failures, and indeterminate creation bubble out so `tk sync` can render safe
/// recovery guidance. A certified per-Mutation rejection is not an error here
/// — it surfaces through [`SyncReport::stopped_at_sequence`].
#[derive(Debug, Error)]
pub enum RunSyncError {
    /// Pull failed: adapter unavailable ([`AdapterReadError::Env`]) or backend
    /// rejection ([`AdapterReadError::Failed`] carrying captured stderr).
    #[error(transparent)]
    Pull(#[from] AdapterReadError),
    /// An edit hit a process environment or observation failure; the in-flight
    /// Mutation keeps its prior `pending` or `failed` state.
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error(transparent)]
    Refresh(#[from] RefreshStoreError),
    #[error(transparent)]
    Load(#[from] LoadApplicableError),
    #[error(transparent)]
    Outcome(#[from] PersistMutationOutcomeError),
    #[error(
        "Backend created {identity} for Mutation {sequence}, but tk could not save its identity: {source}"
    )]
    CreatedIdentityNotStored {
        sequence: i64,
        identity: BackendItemIdentity,
        #[source]
        source: PersistMutationOutcomeError,
    },
    #[error("Mutation {0} has an indeterminate Backend creation outcome")]
    ApplyingMutation(i64),
}

/// Command-independent meaning of a [`RunSyncError`].
///
/// The sync engine owns this exhaustive classification under ADR-0009. Command
/// modules retain their own message bodies and framing under ADR-0017 and
/// ADR-0032, so categories carry evidence rather than rendered prose.
#[derive(Debug)]
pub(crate) enum RunSyncErrorCategory<'a> {
    BackendDetail(&'a str),
    MutationSchemaDrift(&'a RunSyncError),
    TicketBug(&'a RunSyncError),
    Storage(&'a rusqlite::Error),
    CreatedIdentityNotStored {
        error: &'a RunSyncError,
        sequence: i64,
        cause: CreatedIdentityNotStoredCause<'a>,
    },
    Direct(&'a RunSyncError),
    IndeterminateCreation(i64),
    RemoteChanged,
    RepositoryInvariant(&'a RunSyncError),
}

/// Cause retained when Backend creation succeeded but its identity could not
/// be committed to the Repository Store.
#[derive(Debug)]
pub(crate) enum CreatedIdentityNotStoredCause<'a> {
    Storage(&'a rusqlite::Error),
    TargetNotLocal,
    Direct,
}

impl RunSyncError {
    /// Classify the error once for every command that renders sync failures.
    pub(crate) fn category(&self) -> RunSyncErrorCategory<'_> {
        match self {
            Self::Pull(AdapterReadError::Failed(detail)) => {
                RunSyncErrorCategory::BackendDetail(detail)
            }
            Self::Load(
                LoadApplicableError::UnknownMutationType(_)
                | LoadApplicableError::PayloadVariantMissing(_),
            ) => RunSyncErrorCategory::MutationSchemaDrift(self),
            Self::Load(
                LoadApplicableError::PayloadJson(_)
                | LoadApplicableError::OperationShapeMismatch { .. }
                | LoadApplicableError::MissingBackendIdentity { .. }
                | LoadApplicableError::CounterpartClassMismatch { .. }
                | LoadApplicableError::MissingTicketKind { .. },
            )
            | Self::Outcome(
                PersistMutationOutcomeError::PayloadJson(_)
                | PersistMutationOutcomeError::OperationShapeMismatch { .. }
                | PersistMutationOutcomeError::TargetNotLocal { .. }
                | PersistMutationOutcomeError::Transition(_),
            ) => RunSyncErrorCategory::TicketBug(self),
            Self::Refresh(
                RefreshStoreError::Storage(error)
                | RefreshStoreError::BackendCohort(BackendCohortError::Storage(error)),
            )
            | Self::Load(LoadApplicableError::Storage(error))
            | Self::Outcome(PersistMutationOutcomeError::Storage(error)) => {
                RunSyncErrorCategory::Storage(error)
            }
            Self::CreatedIdentityNotStored {
                sequence, source, ..
            } => {
                let cause = match source {
                    PersistMutationOutcomeError::Storage(error) => {
                        CreatedIdentityNotStoredCause::Storage(error)
                    }
                    PersistMutationOutcomeError::TargetNotLocal { .. } => {
                        CreatedIdentityNotStoredCause::TargetNotLocal
                    }
                    PersistMutationOutcomeError::MutationNotFound(_)
                    | PersistMutationOutcomeError::MutationNotApplicable(_)
                    | PersistMutationOutcomeError::ApplyingMutation(_)
                    | PersistMutationOutcomeError::OperationShapeMismatch { .. }
                    | PersistMutationOutcomeError::PayloadJson(_)
                    | PersistMutationOutcomeError::Transition(_) => {
                        CreatedIdentityNotStoredCause::Direct
                    }
                };
                RunSyncErrorCategory::CreatedIdentityNotStored {
                    error: self,
                    sequence: *sequence,
                    cause,
                }
            }
            Self::Pull(AdapterReadError::Env(_))
            | Self::Apply(_)
            | Self::Outcome(
                PersistMutationOutcomeError::MutationNotFound(_)
                | PersistMutationOutcomeError::MutationNotApplicable(_),
            ) => RunSyncErrorCategory::Direct(self),
            Self::ApplyingMutation(sequence)
            | Self::Refresh(RefreshStoreError::ApplyingMutation(sequence))
            | Self::Outcome(PersistMutationOutcomeError::ApplyingMutation(sequence)) => {
                RunSyncErrorCategory::IndeterminateCreation(*sequence)
            }
            Self::Refresh(RefreshStoreError::RemoteChanged { .. }) => {
                RunSyncErrorCategory::RemoteChanged
            }
            Self::Refresh(RefreshStoreError::BackendCohort(
                BackendCohortError::MultipleBackendKinds
                | BackendCohortError::UnknownBackendKind(_)
                | BackendCohortError::BackendKindMismatch { .. },
            )) => RunSyncErrorCategory::RepositoryInvariant(self),
        }
    }
}

/// Run one sync against a configured Adapter.
///
/// `now` is the injected timestamp written to every row this run touches.
pub fn run_sync(
    conn: &mut Connection,
    adapter: &mut dyn Adapter,
    _workflow: &RemoteWorkflowGuard,
    now: &str,
) -> Result<SyncReport, RunSyncError> {
    if let Some(sequence) = applying_mutation_sequence(conn)
        .map_err(|error| RunSyncError::Refresh(RefreshStoreError::Storage(error)))?
    {
        return Err(RunSyncError::ApplyingMutation(sequence));
    }
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

    // Apply loop. An environment failure from an Adapter write bubbles via `?`
    // without changing the row's prior `pending` or `failed` state; a
    // per-Mutation rejection is persisted and stops the loop.
    //
    // Decode is batched (one undecodable row fails the run before any backend
    // write); backend identity is resolved per Mutation, immediately before
    // Apply, so a Promotion receipt committed earlier in this run is visible to
    // the Mutations ordered behind it (ADR-0036).
    let rows = load_applicable_mutations(conn)?;
    for row in rows {
        let resolved = resolve_backend_operation(conn, row)?;
        let sequence = resolved.sequence;
        match resolved.operation {
            BackendOperation::Edit(edit) => {
                let outcome = adapter.apply_edit(&edit)?;
                persist_edit_outcome(conn, sequence, &outcome, now)?;
                match outcome {
                    BackendEditOutcome::Acknowledged => report.applied_count += 1,
                    BackendEditOutcome::Rejected(_) => {
                        report.stopped_at_sequence = Some(sequence);
                        return Ok(report);
                    }
                }
            }
            BackendOperation::Create(create) => {
                begin_create(conn, sequence, now)?;
                let outcome = adapter.create_item(&create);
                if let Err(source) = persist_create_outcome(conn, sequence, &outcome, now) {
                    if let BackendCreateOutcome::Created(identity) = &outcome {
                        return Err(RunSyncError::CreatedIdentityNotStored {
                            sequence,
                            identity: identity.clone(),
                            source,
                        });
                    }
                    return Err(source.into());
                }
                match outcome {
                    BackendCreateOutcome::Created(_) => report.applied_count += 1,
                    BackendCreateOutcome::Rejected(_) => {
                        report.stopped_at_sequence = Some(sequence);
                        return Ok(report);
                    }
                    BackendCreateOutcome::Indeterminate(_) => {
                        return Err(RunSyncError::ApplyingMutation(sequence));
                    }
                }
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_kind::BackendKind;
    use crate::domain::backend_operation::{BackendCreate, BackendEdit, BackendItemRefresh};
    use crate::domain::status::ItemStatus;
    use crate::domain::ticket_kind::TicketKind;
    use crate::proc::{FakeRunner, ProcError};
    use crate::remote::fake::{CreateResponse, EditResponse, FakeAdapter, RefreshResponse};
    use crate::remote::github::GithubAdapter;
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

    fn fake(refreshes: Vec<RefreshResponse>, edits: Vec<EditResponse>) -> FakeAdapter {
        FakeAdapter::new()
            .with_refreshes(refreshes)
            .with_edits(edits)
    }

    fn fake_with_create(
        refreshes: Vec<RefreshResponse>,
        edits: Vec<EditResponse>,
        creates: Vec<CreateResponse>,
    ) -> FakeAdapter {
        FakeAdapter::new()
            .with_refreshes(refreshes)
            .with_edits(edits)
            .with_creates(creates)
    }

    fn run(conn: &mut Connection, fake: &mut FakeAdapter) -> Result<SyncReport, RunSyncError> {
        let workflow = RemoteWorkflowGuard::for_test();
        run_sync(conn, fake, &workflow, NOW)
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
    fn applying_creation_blocks_pull_and_apply_before_any_adapter_call() {
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
                sequence: 7,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "applying",
                failure_json: Some(r#"{"detail":"unknown effect"}"#),
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let mut fake = fake(vec![], vec![]);

        assert!(matches!(
            run(&mut conn, &mut fake),
            Err(RunSyncError::ApplyingMutation(7))
        ));
        assert!(fake.captured_refresh_keys.is_empty());
        assert!(fake.captured_edits.is_empty());
        assert!(fake.captured_creates.is_empty());
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
        assert!(fake.captured_edits.is_empty());
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
        assert!(fake.captured_edits.is_empty());
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

        let mut fake = fake(vec![refresh("Old")], vec![EditResponse::Success]);
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
        assert_eq!(fake.captured_edits.len(), 1);
        let BackendEdit::UpdateTicket { snapshot, .. } = &fake.captured_edits[0] else {
            panic!("expected ticket update")
        };
        assert_eq!(snapshot.title, "New");
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
            vec![EditResponse::RecordedFailure(
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
        assert_eq!(fake.captured_edits.len(), 1);
    }

    #[test]
    fn apply_env_failure_propagates_and_leaves_row_pending() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        seed_remote(&conn);
        update_ticket_mutation(&conn, 1, "t1", "A");

        let mut fake = fake(
            vec![refresh("Old")],
            vec![EditResponse::EnvFailure(ProcError::ExecutableNotFound)],
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
    fn indeterminate_creation_is_persisted_as_applying_without_converting_the_item() {
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
        let mut fake = fake_with_create(
            vec![],
            vec![],
            vec![CreateResponse::Indeterminate("spawn failed".into())],
        );

        assert!(matches!(
            run(&mut conn, &mut fake),
            Err(RunSyncError::ApplyingMutation(1))
        ));
        let (state, failure, origin, backend_key): (
            String,
            Option<String>,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "select m.state, m.failure_json, i.origin, i.backend_key \
                   from mutations m join items i on i.id = m.item_id \
                  where m.sequence = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(state, "applying");
        assert!(failure.unwrap().contains("spawn failed"));
        assert_eq!((origin.as_str(), backend_key), ("local", None));
        assert_eq!(fake.captured_creates.len(), 1);
        assert!(fake.captured_edits.is_empty());
    }

    #[test]
    fn post_spawn_create_failure_stays_applying_and_is_not_replayed() {
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
        let runner = FakeRunner::new();
        runner.expect_exact_error(
            &[
                "gh",
                "issue",
                "create",
                "--title",
                "Local work",
                "--body",
                "",
            ],
            ProcError::OutcomeUnobserved,
        );
        let cwd = std::env::current_dir().unwrap();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let workflow = RemoteWorkflowGuard::for_test();

        assert!(matches!(
            run_sync(&mut conn, &mut adapter, &workflow, NOW),
            Err(RunSyncError::ApplyingMutation(1))
        ));
        let (state, failure): (String, String) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "applying");
        assert!(failure.contains("outcome is unknown"), "{failure}");
        runner.assert_all_consumed();

        assert!(matches!(
            run_sync(&mut conn, &mut adapter, &workflow, NOW),
            Err(RunSyncError::ApplyingMutation(1))
        ));
        runner.assert_all_consumed();
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
        assert!(fake.captured_edits.is_empty());
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

        let mut fake = fake(vec![refresh("Old")], vec![EditResponse::Success]);
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
            vec![EditResponse::Success],
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

        let mut fake = fake_with_create(
            vec![],
            vec![EditResponse::Success],
            vec![CreateResponse::Created {
                backend_key: "42".into(),
                display_id: "gh-42".into(),
            }],
        );
        let report = run(&mut conn, &mut fake).unwrap();
        assert_eq!(report.applied_count, 2);

        assert!(matches!(
            fake.captured_creates[0],
            BackendCreate::Ticket { .. }
        ));
        let BackendEdit::SetItemStatus { item, .. } = &fake.captured_edits[0] else {
            panic!("expected status edit")
        };
        assert_eq!(
            item.backend_key, "42",
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
    fn a_created_identity_that_cannot_be_stored_stays_applying_with_the_receipt() {
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
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t2",
                display: "gh-42",
                title: "Existing display",
                created_seq: 2,
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
        let mut fake = fake_with_create(
            vec![],
            vec![],
            vec![CreateResponse::Created {
                backend_key: "https://github.com/o/r/issues/42".into(),
                display_id: "gh-42".into(),
            }],
        );

        let error = run(&mut conn, &mut fake).unwrap_err();

        assert!(matches!(
            error,
            RunSyncError::CreatedIdentityNotStored {
                sequence: 1,
                ref identity,
                ..
            } if identity.display_id == "gh-42"
                && identity.backend_key == "https://github.com/o/r/issues/42"
        ));
        let (state, origin): (String, String) = conn
            .query_row(
                "select m.state, i.origin from mutations m \
                   join items i on i.id = m.item_id where m.sequence = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "applying");
        assert_eq!(origin, "local");
    }

    #[test]
    fn creation_failures_stop_without_inventing_an_identity() {
        for (response, detail, expected_state, indeterminate) in [
            (
                CreateResponse::Rejected("pre-create refusal".into()),
                "pre-create refusal",
                "failed",
                false,
            ),
            (
                CreateResponse::Indeterminate("request outcome unknown".into()),
                "request outcome unknown",
                "applying",
                true,
            ),
        ] {
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
            let mut fake = fake_with_create(vec![], vec![], vec![response]);

            let result = run(&mut conn, &mut fake);
            if indeterminate {
                assert!(matches!(result, Err(RunSyncError::ApplyingMutation(1))));
            } else {
                assert_eq!(result.unwrap().stopped_at_sequence, Some(1));
            }
            let (state, failure, origin, key): (String, String, String, Option<String>) = conn
                .query_row(
                    "select m.state, m.failure_json, i.origin, i.backend_key \
                       from mutations m join items i on i.id = m.item_id \
                      where m.sequence = 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(state, expected_state);
            assert!(failure.contains(detail));
            assert_eq!(origin, "local");
            assert_eq!(key, None);
            assert_eq!(fake.captured_creates.len(), 1);
            assert!(fake.captured_edits.is_empty());
        }
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

        assert!(fake.captured_edits.is_empty(), "no backend write happened");
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
            vec![EditResponse::Success, EditResponse::Success],
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
        assert_eq!(fake.captured_edits.len(), 2);
    }
}
