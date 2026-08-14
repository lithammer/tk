//! `tk promote` — convert a Local Ticket or Local Epic into a backend-backed
//! object through the configured Remote (CONTEXT.md Promotion).
//!
//! One `tk promote <id>` invocation is one Promotion Operation. The whole
//! operation is preflighted against a Repository Store snapshot before a byte
//! is written (ADR-0035): a refused Promotion leaves the outbox empty and calls
//! no Backend. What survives preflight commits to the Mutation Log in a single
//! local transaction and is then drained by the same [`crate::sync::run_sync`]
//! engine `tk sync` drives, so a Promotion applies behind whatever the outbox
//! already held. The ADR-0037 `reconcile` and `retry` subcommands recover an
//! existing Promotion Operation instead of minting a new one.
//!
//! Preflight reports every finding at once rather than the first (ADR-0036);
//! [`crate::promotion::plan`] carries them as typed parts and this module owns
//! their wording. Reporting reads persisted state only: a receipt replaces the
//! Display ID in place and keeps the outgoing one as an Alias, so re-resolving
//! what was captured before sync is what yields the old-to-new mapping.
//!
//! Per ADR-0032, [`run`] returns `Result<Exit, CommandError>` and the dispatch
//! seam frames failures as `tk promote: <body>`.

use std::collections::HashSet;
use std::io::Write;

use clap::{Args as ClapArgs, Subcommand};

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::domain::backend_binding::BackendBinding;
use crate::domain::backend_kind::BackendKind;
use crate::domain::dependency_rule::DependencyRejection;
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_state::MutationState;
use crate::domain::promotion_graph::{GraphItem, PromotionGraph};
use crate::domain::promotion_plan::PromotionPlan;
use crate::promotion::plan::{PromotionFinding, plan_promotion};
use crate::remote::adapter::{Adapter, AdapterReadError};
use crate::remote::factory::{self, OpenError as FactoryOpenError};
use crate::store::mutations::AppendError;
use crate::store::promotion::{
    self as store_promotion, CancelPromotionError, CancellationReport, CommitPlanError,
    MutationSummary, ReadGraphError, RecoveryPromotion, RecoveryPromotionError,
    RecoveryPromotionMapping, UnrepresentableDependency,
};
use crate::store::repository::RemoteWorkflowGuard;
use crate::store::repository::{ResolvedItemRefWithDisplay, Store};
use crate::store::sync::{
    BackendCohortError, LoadApplicableError, PersistMutationOutcomeError, RefreshStoreError,
};
use crate::sync::{self, RunSyncError};

/// Flags for `tk promote`.
#[derive(Debug, ClapArgs)]
#[command(subcommand_negates_reqs = true, args_conflicts_with_subcommands = true)]
pub struct Args {
    #[command(subcommand)]
    pub subcommand: Option<Sub>,
    /// Display ID or Alias of the Ticket or Epic to promote.
    #[arg(value_name = "ID", required = true)]
    pub id: Option<String>,
    /// Also promote the Epic's directly contained Local Tickets — its
    /// Promotion Children. Epics only.
    #[arg(long)]
    pub children: bool,
}

/// Explicit recovery workflows for a nonterminal Promotion.
#[derive(Debug, Subcommand)]
pub enum Sub {
    /// Attach a confirmed Backend object to an indeterminate Promotion.
    Reconcile(ReconcileArgs),
    /// Explicitly risk creating the Backend object again.
    Retry(RetryArgs),
    /// Withdraw the whole Promotion Operation without reaching the Backend.
    Cancel(CancelArgs),
}

/// Arguments for `tk promote reconcile`.
#[derive(Debug, ClapArgs)]
pub struct ReconcileArgs {
    /// Display ID or Alias of the Pending Promotion.
    #[arg(value_name = "ID")]
    pub id: String,
    /// Backend-native key or URL of the object to inspect.
    #[arg(value_name = "BACKEND-KEY")]
    pub backend_key: String,
    /// Accept a content mismatch and converge the Backend to current local content.
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `tk promote retry`.
#[derive(Debug, ClapArgs)]
pub struct RetryArgs {
    /// Display ID or Alias of the Pending Promotion.
    #[arg(value_name = "ID")]
    pub id: String,
}

/// Arguments for `tk promote cancel`.
#[derive(Debug, ClapArgs)]
pub struct CancelArgs {
    /// Display ID or Alias of any item in the Promotion Operation to withdraw.
    #[arg(value_name = "ID")]
    pub id: String,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    match args.subcommand {
        Some(Sub::Reconcile(args)) => return run_reconcile(deps, args),
        Some(Sub::Retry(args)) => return run_retry(deps, args),
        Some(Sub::Cancel(args)) => return run_cancel(deps, args),
        None => {}
    }
    let id = args
        .id
        .expect("clap requires ID when no promote subcommand is present");
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();
    let workflow = store
        .lock_remote_workflow()
        .map_err(CommandError::failure)?;

    let target = match resolver::resolve_with_display(&store, &id) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "'{id}' is not a known Display ID or Alias"
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };

    // Only an Epic contains Promotion Children, so `--children` elsewhere is a
    // malformed invocation rather than an operation to refuse.
    if args.children && target.item_class != ItemClass::Epic {
        return Err(CommandError::usage(format!(
            "'{id}' is not an Epic; --children promotes the Promotion Children of an Epic"
        )));
    }

    let adapter_opt = match factory::open_configured(store.conn(), deps.runner, deps.cwd) {
        Ok(adapter) => adapter,
        Err(err @ FactoryOpenError::NotImplemented) => return Err(CommandError::failure(err)),
        Err(FactoryOpenError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };
    let Some(mut adapter) = adapter_opt else {
        return Err(no_remote());
    };
    promote(
        deps,
        &mut store,
        &mut *adapter,
        &workflow,
        &target,
        args.children,
        &now,
    )
}

fn run_reconcile(deps: &mut Deps<'_>, args: ReconcileArgs) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();
    let workflow = store
        .lock_remote_workflow()
        .map_err(CommandError::failure)?;
    let target = resolve_recovery_target(&store, &args.id)?;
    let recovery = store_promotion::recoverable_promotion(store.conn(), &target.id)
        .map_err(|err| recovery_error(err, &target.display_id))?;
    let mut adapter = open_recovery_adapter(deps.runner, deps.cwd, &store)?;
    reconcile(
        deps,
        &mut store,
        &mut *adapter,
        &workflow,
        &recovery,
        &args,
        &now,
    )
}

fn run_retry(deps: &mut Deps<'_>, args: RetryArgs) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();
    let workflow = store
        .lock_remote_workflow()
        .map_err(CommandError::failure)?;
    let target = resolve_recovery_target(&store, &args.id)?;
    let recovery = store_promotion::recoverable_promotion(store.conn(), &target.id)
        .map_err(|err| recovery_error(err, &target.display_id))?;
    let mut adapter = open_recovery_adapter(deps.runner, deps.cwd, &store)?;
    retry(deps, &mut store, &mut *adapter, &workflow, &recovery, &now)
}

/// Withdraw a Promotion Operation.
///
/// Unlike reconcile and retry this opens no Backend Adapter and runs no nested
/// sync (ADR-0038), so it works with a broken, unimplemented, or already-cleared
/// Remote — which is the point of an exit of last resort.
fn run_cancel(deps: &mut Deps<'_>, args: CancelArgs) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();
    let workflow = store
        .lock_remote_workflow()
        .map_err(CommandError::failure)?;
    let target = resolve_recovery_target(&store, &args.id)?;
    let report = store_promotion::cancel_promotion(store.conn_mut(), &workflow, &target.id, &now)
        .map_err(|err| cancel_error(err, &target.display_id))?;
    render_cancellation(deps.stdout, &report);
    Ok(Exit::Ok)
}

fn resolve_recovery_target(
    store: &Store,
    id: &str,
) -> Result<ResolvedItemRefWithDisplay, CommandError> {
    match resolver::resolve_with_display(store, id) {
        Ok(target) => Ok(target),
        Err(resolver::ResolveError::NotFound) => Err(CommandError::failure(format!(
            "'{id}' is not a known Display ID or Alias"
        ))),
        Err(resolver::ResolveError::Storage(err)) => Err(resolver::storage_error(&err)),
    }
}

fn open_recovery_adapter<'a>(
    runner: &'a dyn crate::proc::ProcRunner,
    cwd: &'a std::path::Path,
    store: &Store,
) -> Result<Box<dyn Adapter + 'a>, CommandError> {
    match factory::open_configured(store.conn(), runner, cwd) {
        Ok(Some(adapter)) => Ok(adapter),
        Ok(None) => Err(no_remote()),
        Err(err @ FactoryOpenError::NotImplemented) => Err(CommandError::failure(err)),
        Err(FactoryOpenError::Storage(err)) => Err(resolver::storage_error(&err)),
    }
}

fn reconcile(
    deps: &mut Deps<'_>,
    store: &mut Store,
    adapter: &mut dyn Adapter,
    workflow: &RemoteWorkflowGuard,
    recovery: &RecoveryPromotion,
    args: &ReconcileArgs,
    now: &str,
) -> Result<Exit, CommandError> {
    ensure_recovery_backend(adapter, recovery)?;
    // Ordering is a local precondition, so refuse before spending a Backend
    // round trip — and before a content mismatch sends the operator after
    // '--force' for a refusal that would not have applied anyway.
    store_promotion::ensure_no_earlier_nonterminal(store.conn(), recovery.sequence)
        .map_err(|err| recovery_error(err, &recovery.outgoing_display_id))?;
    let inspection = adapter
        .inspect_item(&args.backend_key)
        .map_err(|err| inspection_error(&args.backend_key, err))?;
    let title_matches = inspection.title == recovery.promotion.title;
    let body_matches = inspection.body == recovery.promotion.body;
    let exact = title_matches && body_matches;
    if !exact && !args.force {
        let mut fields = Vec::new();
        if !title_matches {
            fields.push("title");
        }
        if !body_matches {
            fields.push("body");
        }
        return Err(CommandError::failure(format!(
            "Backend item {} does not match the retained Promotion snapshot ({})\nRe-run with '--force' only after confirming it is the object this Promotion created.",
            inspection.identity.display_id,
            fields.join(" and ")
        )));
    }

    let mappings = store_promotion::capture_recovery_mappings(store.conn())
        .map_err(|err| recovery_error(err, &recovery.outgoing_display_id))?;
    store_promotion::reconcile_promotion(
        store.conn_mut(),
        workflow,
        recovery,
        &inspection.identity,
        !exact,
        now,
    )
    .map_err(|err| recovery_error(err, &recovery.outgoing_display_id))?;
    let sync_result = sync::run_sync(store.conn_mut(), adapter, workflow, now);
    let mapping_result = render_recovery_mappings(deps.stdout, store, &mappings);
    finish_recovery(sync_result, mapping_result, RecoveryAction::Reconcile)
}

fn retry(
    deps: &mut Deps<'_>,
    store: &mut Store,
    adapter: &mut dyn Adapter,
    workflow: &RemoteWorkflowGuard,
    recovery: &RecoveryPromotion,
    now: &str,
) -> Result<Exit, CommandError> {
    ensure_recovery_backend(adapter, recovery)?;
    let mappings = store_promotion::capture_recovery_mappings(store.conn())
        .map_err(|err| recovery_error(err, &recovery.outgoing_display_id))?;
    store_promotion::retry_promotion(store.conn_mut(), workflow, recovery, now)
        .map_err(|err| recovery_error(err, &recovery.outgoing_display_id))?;
    let sync_result = sync::run_sync(store.conn_mut(), adapter, workflow, now);
    let mapping_result = render_recovery_mappings(deps.stdout, store, &mappings);
    finish_recovery(sync_result, mapping_result, RecoveryAction::Retry)
}

fn ensure_recovery_backend(
    adapter: &dyn Adapter,
    recovery: &RecoveryPromotion,
) -> Result<(), CommandError> {
    if adapter.backend_kind() == recovery.backend_kind {
        return Ok(());
    }
    Err(CommandError::failure(format!(
        "the configured Remote no longer matches the Promotion for {}",
        recovery.outgoing_display_id
    )))
}

fn inspection_error(backend_key: &str, err: AdapterReadError) -> CommandError {
    CommandError::failure(format!(
        "could not inspect Backend item '{backend_key}': {err}"
    ))
}

/// Recovery result vocabulary whose rendering stays verbatim under ADR-0017.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryAction {
    Reconcile,
    Retry,
}

impl RecoveryAction {
    fn stopped_at_mutation(self, sequence: i64) -> String {
        match self {
            Self::Reconcile => format!(
                "the Promotion was reconciled, but sync stopped at Mutation {sequence}\nInspect it with 'tk sync log {sequence}'; fix the cause, then run 'tk sync' to continue the queue."
            ),
            Self::Retry => format!(
                "the Promotion was retried, but sync stopped at Mutation {sequence}\nInspect it with 'tk sync log {sequence}'; fix the cause, then run 'tk sync' to continue the queue."
            ),
        }
    }

    fn indeterminate_creation(self, sequence: i64) -> String {
        match self {
            Self::Reconcile => format!(
                "the Promotion was reconciled, but Mutation {sequence} has an indeterminate Backend creation outcome\nUse 'tk promote reconcile <id> <backend-key>' if the object exists, or 'tk promote retry <id>' only when creating it again is safe."
            ),
            Self::Retry => format!(
                "the Promotion was retried, but Mutation {sequence} has an indeterminate Backend creation outcome\nUse 'tk promote reconcile <id> <backend-key>' if the object exists, or 'tk promote retry <id>' only when creating it again is safe."
            ),
        }
    }

    fn created_identity_not_stored(
        self,
        sequence: i64,
        error: &RunSyncError,
        detail: Option<&str>,
    ) -> String {
        match (self, detail) {
            (Self::Reconcile, Some(detail)) => format!(
                "the Promotion was reconciled, but sync could not save a created Backend identity\n{error}\n{detail}\nMutation {sequence} remains applying; use 'tk promote reconcile <id> <backend-key>' after confirming the created object."
            ),
            (Self::Retry, Some(detail)) => format!(
                "the Promotion was retried, but sync could not save a created Backend identity\n{error}\n{detail}\nMutation {sequence} remains applying; use 'tk promote reconcile <id> <backend-key>' after confirming the created object."
            ),
            (Self::Reconcile, None) => format!(
                "the Promotion was reconciled, but sync could not save a created Backend identity\n{error}\nMutation {sequence} remains applying; use 'tk promote reconcile <id> <backend-key>' after confirming the created object."
            ),
            (Self::Retry, None) => format!(
                "the Promotion was retried, but sync could not save a created Backend identity\n{error}\nMutation {sequence} remains applying; use 'tk promote reconcile <id> <backend-key>' after confirming the created object."
            ),
        }
    }

    fn sync_did_not_finish(self, detail: &str) -> String {
        match self {
            Self::Reconcile => {
                format!("the Promotion was reconciled, but sync did not finish\n{detail}")
            }
            Self::Retry => format!("the Promotion was retried, but sync did not finish\n{detail}"),
        }
    }

    fn remote_changed(self) -> String {
        match self {
            Self::Reconcile => "the Promotion was reconciled, but the configured Remote changed while contacting the Backend; retry 'tk sync'".into(),
            Self::Retry => "the Promotion was retried, but the configured Remote changed while contacting the Backend; retry 'tk sync'".into(),
        }
    }

    fn reporting_failed(self, body: &str) -> String {
        match self {
            Self::Reconcile => format!(
                "the Promotion was reconciled, but could not report every Display ID replacement\n{body}"
            ),
            Self::Retry => format!(
                "the Promotion was retried, but could not report every Display ID replacement\n{body}"
            ),
        }
    }
}

fn finish_recovery_sync(
    result: Result<sync::SyncReport, RunSyncError>,
    action: RecoveryAction,
) -> Result<Exit, CommandError> {
    match result {
        Ok(report) if report.stopped_at_sequence.is_none() => Ok(Exit::Ok),
        Ok(report) => Err(CommandError::failure(
            action.stopped_at_mutation(
                report
                    .stopped_at_sequence
                    .expect("the guarded branch has a stopping Mutation"),
            ),
        )),
        Err(
            RunSyncError::ApplyingMutation(sequence)
            | RunSyncError::Refresh(RefreshStoreError::ApplyingMutation(sequence))
            | RunSyncError::Outcome(PersistMutationOutcomeError::ApplyingMutation(sequence)),
        ) => Err(CommandError::failure(
            action.indeterminate_creation(sequence),
        )),
        Err(
            err @ RunSyncError::CreatedIdentityNotStored {
                sequence,
                source: PersistMutationOutcomeError::TargetNotLocal { .. },
                ..
            },
        ) => Err(CommandError::failure(action.created_identity_not_stored(
            sequence,
            &err,
            Some("This is Repository Store corruption or a Ticket bug — please report it."),
        ))),
        Err(
            err @ RunSyncError::CreatedIdentityNotStored {
                sequence,
                source: PersistMutationOutcomeError::Storage(_),
                ..
            },
        ) => {
            let RunSyncError::CreatedIdentityNotStored { source, .. } = &err else {
                unreachable!("the match arm fixes the error variant")
            };
            let PersistMutationOutcomeError::Storage(storage) = source else {
                unreachable!("the match arm fixes the source variant")
            };
            let detail = created_identity_storage_detail(storage);
            Err(CommandError::failure(action.created_identity_not_stored(
                sequence,
                &err,
                Some(&detail),
            )))
        }
        Err(err @ RunSyncError::CreatedIdentityNotStored { sequence, .. }) => Err(
            CommandError::failure(action.created_identity_not_stored(sequence, &err, None)),
        ),
        Err(
            RunSyncError::Refresh(
                RefreshStoreError::Storage(storage)
                | RefreshStoreError::BackendCohort(BackendCohortError::Storage(storage)),
            )
            | RunSyncError::Load(LoadApplicableError::Storage(storage))
            | RunSyncError::Outcome(PersistMutationOutcomeError::Storage(storage)),
        ) => Err(recovery_storage_error(action, &storage)),
        Err(
            err @ (RunSyncError::Load(
                LoadApplicableError::UnknownMutationType(_)
                | LoadApplicableError::PayloadVariantMissing(_)
                | LoadApplicableError::PayloadJson(_)
                | LoadApplicableError::OperationShapeMismatch { .. }
                | LoadApplicableError::MissingBackendIdentity { .. }
                | LoadApplicableError::CounterpartClassMismatch { .. },
            )
            | RunSyncError::Outcome(
                PersistMutationOutcomeError::PayloadJson(_)
                | PersistMutationOutcomeError::OperationShapeMismatch { .. }
                | PersistMutationOutcomeError::TargetNotLocal { .. }
                | PersistMutationOutcomeError::Transition(_),
            )),
        ) => Err(CommandError::failure(action.sync_did_not_finish(&format!(
            "{err}; this is a Ticket bug — please report it"
        )))),
        Err(
            err @ RunSyncError::Refresh(RefreshStoreError::BackendCohort(
                BackendCohortError::MultipleBackendKinds
                | BackendCohortError::UnknownBackendKind(_)
                | BackendCohortError::BackendKindMismatch { .. },
            )),
        ) => Err(CommandError::failure(action.sync_did_not_finish(&format!(
            "{err}; this is a Repository Store invariant failure"
        )))),
        Err(RunSyncError::Refresh(RefreshStoreError::RemoteChanged { .. })) => {
            Err(CommandError::failure(action.remote_changed()))
        }
        Err(RunSyncError::Pull(AdapterReadError::Failed(detail))) => {
            Err(CommandError::failure(action.sync_did_not_finish(&detail)))
        }
        Err(
            err @ (RunSyncError::Pull(AdapterReadError::Env(_))
            | RunSyncError::Apply(_)
            | RunSyncError::Outcome(
                PersistMutationOutcomeError::MutationNotFound(_)
                | PersistMutationOutcomeError::MutationNotApplicable(_),
            )),
        ) => Err(CommandError::failure(
            action.sync_did_not_finish(&err.to_string()),
        )),
    }
}

fn finish_recovery(
    sync_result: Result<sync::SyncReport, RunSyncError>,
    mapping_result: Result<(), CommandError>,
    action: RecoveryAction,
) -> Result<Exit, CommandError> {
    let sync_result = finish_recovery_sync(sync_result, action);
    match (sync_result, mapping_result) {
        (Ok(exit), Ok(())) => Ok(exit),
        (Err(sync_error), Ok(())) => Err(sync_error),
        (Ok(_), Err(mapping_error)) => Err(with_partial_recovery_context(action, mapping_error)),
        (Err(sync_error), Err(mapping_error)) => {
            Err(append_secondary_error(sync_error, mapping_error))
        }
    }
}

fn with_partial_recovery_context(action: RecoveryAction, error: CommandError) -> CommandError {
    let CommandError::Failure { body, tail } = error else {
        unreachable!("recovery mapping errors are operation failures")
    };
    CommandError::Failure {
        body: action.reporting_failed(&body),
        tail,
    }
}

fn append_secondary_error(primary: CommandError, secondary: CommandError) -> CommandError {
    let CommandError::Failure {
        body: primary_body,
        tail,
    } = primary
    else {
        unreachable!("recovery sync errors are operation failures")
    };
    let CommandError::Failure {
        body: secondary_body,
        ..
    } = secondary
    else {
        unreachable!("recovery mapping errors are operation failures")
    };
    CommandError::Failure {
        body: format!(
            "{primary_body}\nAdditionally, tk could not report every Display ID replacement: {secondary_body}"
        ),
        tail,
    }
}

fn created_identity_storage_detail(storage: &rusqlite::Error) -> String {
    if resolver::is_busy_error(storage) {
        "The Repository Store was busy while saving the created identity.".into()
    } else {
        format!("The created identity could not be saved in the Repository Store: {storage}")
    }
}

fn recovery_storage_error(action: RecoveryAction, storage: &rusqlite::Error) -> CommandError {
    let classified = resolver::storage_error(storage);
    let CommandError::Failure { body, tail } = classified else {
        unreachable!("storage errors are operation failures")
    };
    CommandError::Failure {
        body: action.sync_did_not_finish(&body),
        tail,
    }
}

fn render_recovery_mappings<W: Write + ?Sized>(
    stdout: &mut W,
    store: &Store,
    captured: &[RecoveryPromotionMapping],
) -> Result<(), CommandError> {
    // Nested sync drains the whole queue, so a corruption finding on one
    // captured Promotion must not hide the replacements that did land. Report
    // every mapping first and surface the finding afterwards; only an
    // unreadable Store aborts, because the next row would fail the same way.
    let mut corruption: Option<CommandError> = None;
    for item in captured {
        let current = match resolver::resolve_with_display(store, &item.outgoing_display_id) {
            Ok(current) => current,
            Err(resolver::ResolveError::NotFound) => {
                corruption.get_or_insert_with(|| {
                    CommandError::failure(format!(
                        "Repository Store corruption: Promotion alias {} disappeared",
                        item.outgoing_display_id
                    ))
                });
                continue;
            }
            Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
        };
        if current.id != item.item_id {
            corruption.get_or_insert_with(|| {
                CommandError::failure(format!(
                    "Repository Store corruption: Promotion alias {} changed ownership",
                    item.outgoing_display_id
                ))
            });
            continue;
        }
        if current.display_id != item.outgoing_display_id {
            let _ = writeln!(
                stdout,
                "Promoted {}: {} -> {}",
                item.item_class.label(),
                item.outgoing_display_id,
                current.display_id
            );
        }
    }
    match corruption {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Preflight, commit, sync, report — the operation itself, once the Repository
/// Store and the Backend Adapter are open.
///
/// Split from [`run`] at the Adapter seam so the Promotion transaction and
/// recovery paths can be exercised with a scripted Adapter independently of
/// the concrete GitHub subprocess mapping.
fn promote(
    deps: &mut Deps<'_>,
    store: &mut Store,
    adapter: &mut dyn Adapter,
    workflow: &RemoteWorkflowGuard,
    target: &ResolvedItemRefWithDisplay,
    children: bool,
    now: &str,
) -> Result<Exit, CommandError> {
    let backend = adapter.backend_kind();
    let graph = store_promotion::read_graph(store.conn(), &target.id, children)
        .map_err(read_graph_error)?;
    let plan = plan_promotion(&graph, adapter.promotion_capabilities(), backend)
        .map_err(|findings| refusal(&target.display_id, &findings, backend))?;

    let captured = capture_display_ids(&graph, &plan);
    let operation_id = store_promotion::commit_plan(
        store.conn_mut(),
        workflow,
        &plan,
        backend,
        &mut *deps.rng,
        now,
    )
    .map_err(commit_error)?;
    if plan.is_empty() {
        render_nothing_to_promote(deps.stdout, target_item(&graph));
    }

    // Sync runs even when nothing was appended: an earlier invocation's
    // Promotion may still be pending, and this is the drain that applies it.
    let sync_result = sync::run_sync(store.conn_mut(), adapter, workflow, now);

    render_mappings(deps.stdout, store, &captured)?;

    // An empty plan owns no Mutations to resolve, so re-invoking on work that
    // is already Backend or already Pending Promotion stays an idempotent
    // success — but only if the drain this invocation ran actually finished.
    // Reporting Ok while sync failed would claim the Promotion landed while it
    // remains pending.
    let Some(operation_id) = operation_id else {
        return match sync_stop(&sync_result) {
            Some(SyncStop::Error(err)) => Err(CommandError::failure(format!(
                "nothing to promote, but the sync that followed did not finish\n{err}"
            ))),
            Some(SyncStop::RejectedMutation(sequence)) => Err(CommandError::failure(format!(
                "nothing to promote, but the sync that followed stopped at Mutation {sequence}"
            ))),
            None => Ok(Exit::Ok),
        };
    };
    let unresolved = store_promotion::unresolved_in_operation(store.conn(), &operation_id)
        .map_err(|e| resolver::storage_error(&e))?;
    let Some(first_unresolved) = unresolved.first() else {
        // Every Mutation of this Promotion Operation resolved; a failure later
        // in the same run belongs to the rest of the outbox, not to the
        // Promotion, but it still leaves the sync unfinished.
        return match sync_stop(&sync_result) {
            Some(SyncStop::Error(err)) => Err(CommandError::failure(format!(
                "the Promotion applied, but the sync that followed it did not finish\n{err}"
            ))),
            Some(SyncStop::RejectedMutation(sequence)) => Err(CommandError::failure(format!(
                "the Promotion applied, but the sync that followed stopped at Mutation {sequence}"
            ))),
            None => Ok(Exit::Ok),
        };
    };
    let blocker = store_promotion::earliest_applicable_mutation(store.conn())
        .map_err(|e| resolver::storage_error(&e))?;
    Err(unresolved_failure(
        blocker.as_ref(),
        first_unresolved,
        sync_result.as_ref().err(),
    ))
}

enum SyncStop<'a> {
    Error(&'a RunSyncError),
    RejectedMutation(i64),
}

fn sync_stop(result: &Result<sync::SyncReport, RunSyncError>) -> Option<SyncStop<'_>> {
    match result {
        Err(error) => Some(SyncStop::Error(error)),
        Ok(report) => report.stopped_at_sequence.map(SyncStop::RejectedMutation),
    }
}

/// An Item's Display ID as it stood before sync, with the Item Class the
/// mapping line names.
struct CapturedItem {
    display_id: String,
    item_class: ItemClass,
}

/// Capture the Display IDs a receipt could replace during this sync.
///
/// A Promotion receipt replaces the Display ID in place, so the old value has
/// to be held before the drain for the mapping to be renderable afterwards. The
/// target rides along even when the plan promotes nothing: an already Pending
/// Promotion can apply in this very invocation. Order follows `graph.items`,
/// which is creation order.
fn capture_display_ids(graph: &PromotionGraph, plan: &PromotionPlan) -> Vec<CapturedItem> {
    let promoted: HashSet<&str> = plan
        .mutations
        .iter()
        .filter(|m| m.mutation_type.is_promotion())
        .map(|m| m.item_id.as_str())
        .collect();
    graph
        .items
        .iter()
        .filter(|item| item.id == graph.target_id || promoted.contains(item.id.as_str()))
        .map(|item| CapturedItem {
            display_id: item.display_id.clone(),
            item_class: item.item_class,
        })
        .collect()
}

fn target_item(graph: &PromotionGraph) -> &GraphItem {
    graph
        .items
        .iter()
        .find(|item| item.id == graph.target_id)
        .expect("the Promotion graph carries the target it was read for")
}

/// Print one line per Item whose Display ID a Promotion receipt replaced.
///
/// The outgoing Display ID stays resolvable as an Alias (CONTEXT.md Promotion),
/// so re-resolving what was captured before sync reaches the same Item and
/// yields its current Display ID. An unchanged Display ID means no receipt
/// landed for it, and nothing speculative is printed.
fn render_mappings<W: Write + ?Sized>(
    stdout: &mut W,
    store: &Store,
    captured: &[CapturedItem],
) -> Result<(), CommandError> {
    for item in captured {
        let current = match resolver::resolve_with_display(store, &item.display_id) {
            Ok(current) => current,
            // Unreachable: Promotion preserves the outgoing Display ID as an
            // Alias, and an unpromoted Item still owns it.
            Err(resolver::ResolveError::NotFound) => continue,
            Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
        };
        if current.display_id != item.display_id {
            let _ = writeln!(
                stdout,
                "Promoted {}: {} -> {}",
                item.item_class.label(),
                item.display_id,
                current.display_id
            );
        }
    }
    Ok(())
}

/// Report the idempotent no-op an empty plan means.
fn render_nothing_to_promote<W: Write + ?Sized>(stdout: &mut W, target: &GraphItem) {
    match &target.backend_binding {
        BackendBinding::Backend { .. } => {
            let _ = writeln!(stdout, "Already promoted: {}", target.display_id);
        }
        // A Local target is always promoted, so an empty plan leaves only work
        // whose Promotion intent is already durable.
        BackendBinding::PendingPromotion { .. } | BackendBinding::Local => {
            let _ = writeln!(stdout, "Promotion already pending: {}", target.display_id);
        }
    }
}

/// Diagnose a Promotion Operation with Mutations left unresolved.
///
/// A Mutation ahead of the operation means the Promotion is durably queued and
/// can apply after that Mutation clears. An unresolved Mutation owned by the
/// operation means the Promotion itself did not land.
fn unresolved_failure(
    blocker: Option<&MutationSummary>,
    unresolved: &MutationSummary,
    sync_error: Option<&RunSyncError>,
) -> CommandError {
    let (headline, guidance) = match blocker {
        Some(blocker) if blocker.sequence < unresolved.sequence => (
            format!(
                "the Promotion is committed and remains pending behind Mutation {} ({}) for {}",
                blocker.sequence, blocker.state, blocker.target_display_id
            ),
            match blocker.state {
                MutationState::Failed => format!(
                    "Resolve that Mutation — 'tk sync log {}' shows why it stopped — then run 'tk sync' to apply the Promotion.",
                    blocker.sequence
                ),
                // Automatic sync can never clear the blocker, so naming it is
                // the only actionable guidance — see `recovery_guidance`.
                MutationState::Applying => format!(
                    "Inspect it with 'tk sync log {}'. Then use 'tk promote reconcile {} <backend-key>' if the Backend object exists, or 'tk promote retry {}' only when creating it again is safe.",
                    blocker.sequence, blocker.target_display_id, blocker.target_display_id
                ),
                // Still `pending`: never attempted, so there is nothing
                // recorded against it and nothing to resolve.
                _ => "Fix the cause above, then run 'tk sync' to apply the Promotion.".to_owned(),
            },
        ),
        _ => (
            format!(
                "the Promotion did not finish: Mutation {} ({}) for {} is unresolved",
                unresolved.sequence, unresolved.state, unresolved.target_display_id
            ),
            recovery_guidance(unresolved),
        ),
    };
    // A typed sync error says why the engine stopped; the Mutation Log says
    // what durable state the Promotion reached.
    let cause = match sync_error {
        Some(err) => format!("\nSync stopped: {err}"),
        None => String::new(),
    };
    CommandError::failure(format!("{headline}{cause}\n{guidance}"))
}

/// The next step for a Mutation that stopped the sync, keyed on whether it
/// carries a recorded failure to read.
///
/// A Mutation the engine never resolved is still `pending` with no
/// `failure_json`: an environment failure writes no outcome (ADR-0009), and a
/// Mutation ordered behind the stopping point was never attempted. `tk sync
/// log` on such a row renders no Failure block, so sending the reader there
/// answers nothing — the cause is the line above. Only a `failed` row has
/// something recorded for them to inspect. An `applying` row must not be sent
/// back through automatic sync because its creation may already have succeeded.
fn recovery_guidance(mutation: &MutationSummary) -> String {
    match mutation.state {
        MutationState::Failed => format!(
            "Inspect it with 'tk sync log {}', then run 'tk sync' to apply the rest of the Promotion.",
            mutation.sequence
        ),
        MutationState::Applying => format!(
            "Inspect it with 'tk sync log {}'. Then use 'tk promote reconcile <id> <backend-key>' if the Backend object exists, or 'tk promote retry <id>' only when creating it again is safe.",
            mutation.sequence,
        ),
        _ => {
            "Fix the cause above, then run 'tk sync' to apply the rest of the Promotion.".to_owned()
        }
    }
}

/// Refuse the operation with every preflight finding.
///
/// The seam frames only the first line (ADR-0032), so the headline leads and
/// each finding follows on its own line, in the order the planner collected
/// them: Item findings in creation order, then relationship findings in
/// endpoint order.
fn refusal(
    target_display_id: &str,
    findings: &[PromotionFinding],
    backend: BackendKind,
) -> CommandError {
    let mut body = format!("cannot promote {target_display_id}:");
    for finding in findings {
        body.push_str("\n  ");
        body.push_str(&render_finding(finding, backend));
    }
    CommandError::failure(body)
}

/// Word one preflight finding: what is wrong, which Items it is about, and —
/// where the planner computed one — what the user can do about it.
fn render_finding(finding: &PromotionFinding, backend: BackendKind) -> String {
    match finding {
        PromotionFinding::TriageTicket { item } => format!(
            "{id} is in triage; run 'tk accept {id} --priority P0..P4' before promoting it.",
            id = item.display_id
        ),
        PromotionFinding::ItemClassNotRepresentable { item, item_class } => format!(
            "{}: the {backend} Backend cannot create {}s under Promotion.",
            item.display_id,
            item_class.label()
        ),
        PromotionFinding::TicketKindNotRepresentable { item, ticket_kind } => format!(
            "{}: the {backend} Backend cannot create {} Tickets under Promotion.",
            item.display_id,
            ticket_kind.label()
        ),
        // ADR-0035 asks a rejected Dependency to name both endpoints, the
        // reason, and a remedy. The remedy follows from the reason, so both
        // come out of one match rather than being chosen twice.
        PromotionFinding::DependencyRejected {
            blocked,
            blocking,
            reason,
        } => match reason {
            DependencyRejection::BackendBlockedLocalBlocking => format!(
                "{blocked_id} would be backend-backed while its Blocking Item {blocking_id} stays local. \
                 Promote {blocking_id} in the same operation, or run 'tk unblock {blocked_id} {blocking_id}' to drop the Dependency.",
                blocked_id = blocked.display_id,
                blocking_id = blocking.display_id,
            ),
            DependencyRejection::BackendKindMismatch => format!(
                "{blocked_id} and {blocking_id} would be backed by different Backends. \
                 Run 'tk unblock {blocked_id} {blocking_id}' to drop the Dependency.",
                blocked_id = blocked.display_id,
                blocking_id = blocking.display_id,
            ),
        },
        PromotionFinding::DependencyNotRepresentable { blocked, blocking } => format!(
            "{} depends on {}, and the {backend} Backend cannot represent a Dependency under Promotion.",
            blocked.display_id, blocking.display_id
        ),
        PromotionFinding::EpicMembershipNotRepresentable { ticket, epic } => format!(
            "{} belongs to Epic {}, and the {backend} Backend cannot represent Epic membership under Promotion.",
            ticket.display_id, epic.display_id
        ),
    }
}

/// The no-Remote diagnostic for both configuration lookup paths.
fn no_remote() -> CommandError {
    CommandError::failure("no Remote configured; run 'tk remote set <kind>' first")
}

fn read_graph_error(err: ReadGraphError) -> CommandError {
    match err {
        ReadGraphError::Storage(e) => resolver::storage_error(&e),
        ReadGraphError::BackendBinding(e) => resolver::backend_binding_error(&e),
    }
}

fn commit_error(err: CommitPlanError) -> CommandError {
    match err {
        CommitPlanError::Storage(e)
        | CommitPlanError::Append(AppendError::Sqlite(e))
        | CommitPlanError::BackendCohort(BackendCohortError::Storage(e)) => {
            resolver::storage_error(&e)
        }
        CommitPlanError::Append(e @ AppendError::Sequence(_)) => {
            CommandError::failure(format!("Repository Store corruption: {e}"))
        }
        CommitPlanError::RemoteChanged { .. } => CommandError::failure(
            "the configured Remote changed while preparing the Promotion; retry 'tk promote'",
        ),
        CommitPlanError::BackendCohort(e) => {
            CommandError::failure(format!("Repository Store corruption: {e}"))
        }
    }
}

fn recovery_error(err: RecoveryPromotionError, display_id: &str) -> CommandError {
    match err {
        RecoveryPromotionError::Storage(err)
        | RecoveryPromotionError::Receipt(store_promotion::ApplyReceiptError::Storage(err))
        | RecoveryPromotionError::Append(AppendError::Sqlite(err))
        | RecoveryPromotionError::BackendCohort(BackendCohortError::Storage(err)) => {
            resolver::storage_error(&err)
        }
        RecoveryPromotionError::NoRecoverablePromotion(_)
        | RecoveryPromotionError::TerminalPromotion { .. } => CommandError::failure(format!(
            "'{display_id}' has no nonterminal Promotion to recover"
        )),
        RecoveryPromotionError::EarlierNonterminal { sequence, state } => {
            CommandError::failure(format!(
                "Mutation {sequence} ({state}) must resolve before recovering the Promotion for {display_id}"
            ))
        }
        RecoveryPromotionError::RemoteChanged { .. } => CommandError::failure(format!(
            "the configured Remote changed while recovering the Promotion for {display_id}; retry the command"
        )),
        RecoveryPromotionError::BackendIdentityTaken {
            display_id: backend_display_id,
        } => CommandError::failure(format!(
            "Backend object {backend_display_id} is already tracked by tk, so the Promotion for {display_id} was left unchanged\nConfirm the Backend object this Promotion created, or resolve the duplicate first."
        )),
        RecoveryPromotionError::RetryNotApplying { sequence, state } => {
            CommandError::failure(format!(
                "the Promotion for {display_id} is {state}, not an indeterminate creation; Mutation {sequence} is retried by 'tk sync'"
            ))
        }
        // Named exhaustively rather than caught by `_`, so a variant added
        // later has to be classified here instead of inheriting a corruption
        // diagnosis it may not deserve.
        RecoveryPromotionError::MultipleNonterminalPromotions { first, second, .. } => {
            CommandError::failure(format!(
                "Repository Store corruption: '{display_id}' has multiple nonterminal Promotion Mutations ({first} and {second})"
            ))
        }
        err @ (RecoveryPromotionError::MalformedPayload { .. }
        | RecoveryPromotionError::MalformedBackendKind { .. }
        | RecoveryPromotionError::MissingOperationId(_)
        | RecoveryPromotionError::WrongMutationShape { .. }
        | RecoveryPromotionError::TargetNotLocal { .. }
        | RecoveryPromotionError::BackendCohort(_)
        | RecoveryPromotionError::Receipt(_)
        | RecoveryPromotionError::Append(_)
        | RecoveryPromotionError::Transition(_)) => {
            CommandError::failure(format!("Repository Store corruption: {err}"))
        }
    }
}

fn cancel_error(err: CancelPromotionError, display_id: &str) -> CommandError {
    match err {
        CancelPromotionError::Storage(err) => resolver::storage_error(&err),
        CancelPromotionError::Recovery(err) => recovery_error(*err, display_id),
        CancelPromotionError::ApplyingPromotion {
            sequence,
            display_id: applying_display_id,
        } => CommandError::failure(format!(
            "the Promotion for {applying_display_id} has an indeterminate Backend creation outcome, so the Promotion Operation cannot be withdrawn\nInspect it with 'tk sync log {sequence}'. Then use 'tk promote reconcile {applying_display_id} <backend-key>' if the Backend object exists, or 'tk promote retry {applying_display_id}' only when creating it again is safe."
        )),
        // ADR-0035 asks a rejected Dependency to name both endpoints, the
        // reason, and a remedy; a withdrawal reaches the same graph from the
        // other direction and says the same thing.
        CancelPromotionError::UnrepresentableDependencies(edges) => {
            let mut body = format!("cannot cancel the Promotion Operation for {display_id}:");
            for edge in &edges {
                body.push_str("\n  ");
                body.push_str(&render_withdrawn_dependency(edge));
            }
            CommandError::failure(body)
        }
        CancelPromotionError::NothingToWithdraw(_) => CommandError::failure(format!(
            "the Promotion Operation for {display_id} has already resolved; there is no Promotion left to withdraw"
        )),
        CancelPromotionError::BackendBinding(err) => resolver::backend_binding_error(&err),
        err @ (CancelPromotionError::MalformedPayload { .. }
        | CancelPromotionError::Transition(_)) => {
            CommandError::failure(format!("Repository Store corruption: {err}"))
        }
    }
}

fn render_withdrawn_dependency(edge: &UnrepresentableDependency) -> String {
    match edge.rejection {
        DependencyRejection::BackendBlockedLocalBlocking => format!(
            "{blocked_id} is backend-backed and would be left waiting on {blocking_id}, which the withdrawal returns to local. \
             Run 'tk unblock {blocked_id} {blocking_id}' to drop the Dependency, then cancel again.",
            blocked_id = edge.blocked_display_id,
            blocking_id = edge.blocking_display_id,
        ),
        DependencyRejection::BackendKindMismatch => format!(
            "{blocked_id} and {blocking_id} would be backed by different Backends. \
             Run 'tk unblock {blocked_id} {blocking_id}' to drop the Dependency, then cancel again.",
            blocked_id = edge.blocked_display_id,
            blocking_id = edge.blocking_display_id,
        ),
    }
}

/// Report one withdrawal.
///
/// Enumerate what surprises, count what does not (ADR-0038): every withdrawn
/// Promotion, every Promotion the Backend already accepted, and every withdrawn
/// Mutation whose target survives — that last group is intent lost for an object
/// that really exists upstream. Mutations targeting a withdrawn item follow
/// from the withdrawal itself, so they are a count.
fn render_cancellation<W: Write + ?Sized>(stdout: &mut W, report: &CancellationReport) {
    for promotion in &report.cancelled_promotions {
        let _ = writeln!(
            stdout,
            "Cancelled Promotion: {} {}",
            promotion.item_class.label(),
            promotion.display_id
        );
    }
    for promotion in &report.applied_promotions {
        let _ = writeln!(
            stdout,
            "Already created upstream, left in place: {} {}",
            promotion.item_class.label(),
            promotion.display_id
        );
    }
    let mut on_cancelled_items = 0;
    for mutation in &report.withdrawn {
        if mutation.target_cancelled {
            on_cancelled_items += 1;
            continue;
        }
        let _ = writeln!(
            stdout,
            "Withdrew {} for {} (Mutation {})",
            mutation.mutation_type, mutation.target_display_id, mutation.sequence
        );
    }
    if on_cancelled_items > 0 {
        let _ = writeln!(
            stdout,
            "Withdrew {on_cancelled_items} further Mutation(s) targeting the cancelled items."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::domain::backend_operation::{
        BackendEdit, BackendItemIdentity, BackendItemInspection, BackendItemRefresh,
    };
    use crate::domain::mutation_state::MutationState;
    use crate::domain::promotion_capability::PromotionCapabilities;
    use crate::domain::status::ItemStatus;
    use crate::domain::ticket_kind::TicketKind;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::promotion::plan::ItemRef;
    use crate::remote::fake::{
        CreateResponse, EditResponse, FakeAdapter, InspectionResponse, RefreshResponse,
    };
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, TmpStore, commit_promotion, insert_dependency,
        insert_fixture_item, insert_fixture_mutation, insert_fixture_remote, mutation_count,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rusqlite::Connection;
    use std::path::Path;

    fn cwd() -> std::path::PathBuf {
        std::env::current_dir().unwrap()
    }

    fn seed_store(store: &TmpStore) -> Connection {
        std::fs::create_dir_all(store.tk_dir()).unwrap();
        let mut conn = Connection::open(store.db_path()).unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn.execute(
            "insert into store_config(key, value) values ('display_prefix', 'tk')",
            [],
        )
        .unwrap();
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        conn
    }

    struct Harness<'a> {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdin: std::io::Cursor<Vec<u8>>,
        runner: FakeRunner,
        clock: FakeClock,
        rng: StdRng,
        cwd: &'a Path,
    }

    impl<'a> Harness<'a> {
        fn new(cwd: &'a Path) -> Self {
            Self {
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdin: std::io::Cursor::new(Vec::new()),
                runner: FakeRunner::new(),
                clock: FakeClock::new(1_778_284_800_000),
                rng: StdRng::seed_from_u64(7),
                cwd,
            }
        }
        fn deps(&mut self) -> Deps<'_> {
            Deps {
                stdout: &mut self.stdout,
                stderr: &mut self.stderr,
                stdin: &mut self.stdin,
                runner: &self.runner,
                clock: &self.clock,
                rng: &mut self.rng,
                cwd: self.cwd,
                styler: Styler::plain(),
            }
        }
        fn out(&self) -> String {
            String::from_utf8(self.stdout.clone()).unwrap()
        }
        fn err(&self) -> String {
            String::from_utf8(self.stderr.clone()).unwrap()
        }
    }

    /// Queue the `git rev-parse` discovery call `open_for_command` makes.
    fn expect_git(h: &Harness<'_>, store: &TmpStore) {
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
    }

    fn local_ticket(conn: &Connection, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Local work",
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn local_epic(conn: &Connection, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Local epic",
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn adapter(edits: Vec<EditResponse>, creates: Vec<CreateResponse>) -> FakeAdapter {
        FakeAdapter::new()
            .with_edits(edits)
            .with_creates(creates)
            .with_capabilities(PromotionCapabilities::all())
    }

    fn adapter_with_refresh(edits: Vec<EditResponse>, creates: Vec<CreateResponse>) -> FakeAdapter {
        FakeAdapter::new()
            .with_refreshes(vec![RefreshResponse::Item(
                crate::domain::backend_operation::BackendItemRefresh {
                    title: "Adopted".into(),
                    body: String::new(),
                    status: crate::domain::status::ItemStatus::Open,
                    ticket_kind: Some(TicketKind::Task),
                },
            )])
            .with_edits(edits)
            .with_creates(creates)
            .with_capabilities(PromotionCapabilities::all())
    }

    /// Drive `run` and frame any error exactly as the dispatch seam does
    /// (ADR-0032: `tk promote: <body>`), so a test asserts the framed bytes.
    fn run_rendered(h: &mut Harness<'_>, id: &str, children: bool) -> Exit {
        let mut deps = h.deps();
        let args = Args {
            subcommand: None,
            id: Some(id.into()),
            children,
        };
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "promote");
                exit
            }
        }
    }

    fn run_subcommand_rendered(h: &mut Harness<'_>, subcommand: Sub) -> Exit {
        let mut deps = h.deps();
        let args = Args {
            subcommand: Some(subcommand),
            id: None,
            children: false,
        };
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "promote");
                exit
            }
        }
    }

    /// Open the Repository Store the way `run` does, so a test can drive
    /// [`promote`] against a scripted Adapter.
    fn open_store(h: &Harness<'_>, store: &TmpStore, cwd: &Path) -> Store {
        expect_git(h, store);
        resolver::open_for_command(&h.runner, cwd, &h.clock).expect("open the Repository Store")
    }

    /// Drive the Adapter-seam half of the command with a scripted Adapter,
    /// framing any error as the dispatch seam does.
    fn promote_rendered(
        h: &mut Harness<'_>,
        store: &mut Store,
        fake: &mut FakeAdapter,
        id: &str,
        children: bool,
    ) -> Exit {
        let target = resolver::resolve_with_display(store, id).expect("the target resolves");
        let workflow = store.lock_remote_workflow().unwrap();
        let mut deps = h.deps();
        let now = deps.clock.now_iso();
        match promote(&mut deps, store, fake, &workflow, &target, children, &now) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "promote");
                exit
            }
        }
    }

    fn item_state(conn: &Connection, id: &str) -> (String, String) {
        conn.query_row(
            "select display_value, origin from items where id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    fn inspection(display_id: &str, key: &str, title: &str, body: &str) -> InspectionResponse {
        InspectionResponse::Item(BackendItemInspection {
            identity: BackendItemIdentity {
                backend_key: key.into(),
                display_id: display_id.into(),
            },
            title: title.into(),
            body: body.into(),
        })
    }

    fn refresh(title: &str, body: &str, status: ItemStatus) -> RefreshResponse {
        RefreshResponse::Item(BackendItemRefresh {
            title: title.into(),
            body: body.into(),
            status,
            ticket_kind: Some(TicketKind::Task),
        })
    }

    fn reconcile_rendered(
        h: &mut Harness<'_>,
        store: &mut Store,
        fake: &mut FakeAdapter,
        id: &str,
        backend_key: &str,
        force: bool,
    ) -> Exit {
        let target = resolver::resolve_with_display(store, id).expect("the target resolves");
        let recovery = store_promotion::recoverable_promotion(store.conn(), &target.id)
            .expect("the Promotion is recoverable");
        let workflow = store.lock_remote_workflow().unwrap();
        let mut deps = h.deps();
        let now = deps.clock.now_iso();
        let args = ReconcileArgs {
            id: id.into(),
            backend_key: backend_key.into(),
            force,
        };
        match reconcile(&mut deps, store, fake, &workflow, &recovery, &args, &now) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "promote");
                exit
            }
        }
    }

    fn retry_rendered(
        h: &mut Harness<'_>,
        store: &mut Store,
        fake: &mut FakeAdapter,
        id: &str,
    ) -> Exit {
        let target = resolver::resolve_with_display(store, id).expect("the target resolves");
        let recovery = store_promotion::recoverable_promotion(store.conn(), &target.id)
            .expect("the Promotion is recoverable");
        let workflow = store.lock_remote_workflow().unwrap();
        let mut deps = h.deps();
        let now = deps.clock.now_iso();
        match retry(&mut deps, store, fake, &workflow, &recovery, &now) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "promote");
                exit
            }
        }
    }

    /// Drive `tk promote cancel` end to end, framing any error as the dispatch
    /// seam does. No Adapter is threaded through, because cancellation opens
    /// none (ADR-0038).
    fn cancel_rendered(h: &mut Harness<'_>, id: &str) -> Exit {
        let mut deps = h.deps();
        match run_cancel(&mut deps, CancelArgs { id: id.into() }) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "promote");
                exit
            }
        }
    }

    // ---- argument and Remote validation (no Adapter reached) -------------

    #[test]
    fn an_unknown_id_names_what_the_user_typed() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "tk-9999", false);

        assert_eq!(code, Exit::Failure);
        assert!(
            h.err()
                .contains("tk promote: 'tk-9999' is not a known Display ID or Alias"),
            "{}",
            h.err()
        );
    }

    #[test]
    fn children_on_a_ticket_is_a_usage_error() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_ticket(&conn, "t1", "tk-1", 1);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "tk-1", true);

        assert_eq!(
            code,
            Exit::Usage,
            "only an Epic contains Promotion Children"
        );
        assert!(
            h.err().contains(
                "tk promote: 'tk-1' is not an Epic; --children promotes the Promotion Children of an Epic"
            ),
            "{}",
            h.err()
        );
        assert_eq!(mutation_count(&conn).unwrap(), 0);
    }

    #[test]
    fn no_remote_configured_is_a_failure_with_the_sync_guidance() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        conn.execute("delete from sync_cursors", []).unwrap();
        conn.execute("delete from remotes", []).unwrap();
        local_ticket(&conn, "t1", "tk-1", 1);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "tk-1", false);

        assert_eq!(code, Exit::Failure);
        assert!(
            h.err()
                .contains("tk promote: no Remote configured; run 'tk remote set <kind>' first"),
            "{}",
            h.err()
        );
        assert_eq!(mutation_count(&conn).unwrap(), 0);
    }

    #[test]
    fn a_jira_remote_is_not_implemented() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        conn.execute("update remotes set backend_kind = 'jira'", [])
            .unwrap();
        conn.execute("update sync_cursors set backend_kind = 'jira'", [])
            .unwrap();
        local_ticket(&conn, "t1", "tk-1", 1);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "tk-1", false);

        assert_eq!(code, Exit::Failure);
        assert!(
            h.err().contains(
                "tk promote: the configured Remote's adapter is not implemented in this build"
            ),
            "{}",
            h.err()
        );
    }

    #[test]
    fn github_capabilities_leave_only_real_aggregate_preflight_findings() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_epic(&conn, "e1", "tk-1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "c1",
                display: "tk-2",
                title: "Build the child",
                container_id: Some("e1"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "c2",
                display: "tk-3",
                title: "Captured idea",
                priority: None,
                container_id: Some("e1"),
                selection_state: Some("triage"),
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        local_ticket(&conn, "outside", "tk-4", 4);
        insert_dependency(&conn, "outside", "c1").unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "tk-1", true);

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk promote: cannot promote tk-1:\n  \
             tk-3 is in triage; run 'tk accept tk-3 --priority P0..P4' before promoting it.\n  \
             tk-2 would be backend-backed while its Blocking Item tk-4 stays local. \
             Promote tk-4 in the same operation, or run 'tk unblock tk-2 tk-4' to drop the Dependency.\n"
        );
        assert_eq!(mutation_count(&conn).unwrap(), 0);
        h.runner.assert_all_consumed();
    }

    #[test]
    fn a_github_remote_promotes_a_task_through_the_real_adapter() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_ticket(&conn, "t1", "tk-1", 1);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        h.runner.expect_exact(
            &[
                "gh",
                "issue",
                "create",
                "--title",
                "Local work",
                "--body",
                "",
            ],
            RunOutput {
                exit_code: 0,
                stdout: b"https://github.com/o/r/issues/42\n".to_vec(),
                stderr: Vec::new(),
            },
        );

        let code = run_rendered(&mut h, "tk-1", false);

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(h.out(), "Promoted Ticket: tk-1 -> gh-42\n");
        assert_eq!(item_state(&conn, "t1"), ("gh-42".into(), "backend".into()));
        h.runner.assert_all_consumed();
    }

    #[test]
    fn a_github_remote_promotes_an_epic_and_child_with_membership() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_epic(&conn, "e1", "tk-1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "c1",
                display: "tk-2",
                title: "Child",
                container_id: Some("e1"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        h.runner.expect_exact(
            &[
                "gh",
                "issue",
                "create",
                "--title",
                "Local epic",
                "--body",
                "",
            ],
            RunOutput {
                exit_code: 0,
                stdout: b"https://github.com/o/r/issues/1\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        h.runner.expect_exact(
            &["gh", "issue", "create", "--title", "Child", "--body", ""],
            RunOutput {
                exit_code: 0,
                stdout: b"https://github.com/o/r/issues/2\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        h.runner.expect_exact(
            &[
                "gh",
                "issue",
                "edit",
                "https://github.com/o/r/issues/2",
                "--parent",
                "https://github.com/o/r/issues/1",
            ],
            RunOutput {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );

        let code = run_rendered(&mut h, "tk-1", true);

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(
            h.out(),
            "Promoted Epic: tk-1 -> gh-1\nPromoted Ticket: tk-2 -> gh-2\n"
        );
        assert_eq!(item_state(&conn, "e1"), ("gh-1".into(), "backend".into()));
        assert_eq!(item_state(&conn, "c1"), ("gh-2".into(), "backend".into()));
        h.runner.assert_all_consumed();
    }

    #[test]
    fn public_reconcile_dispatch_uses_the_configured_github_adapter() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        conn.execute("update mutations set state = 'applying'", [])
            .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);
        let issue = br#"{"number":42,"title":"Local work","body":"","state":"OPEN","issueType":null,"url":"https://github.com/o/r/issues/42"}"#;
        for key in ["42", "https://github.com/o/r/issues/42"] {
            h.runner.expect_exact(
                &[
                    "gh",
                    "issue",
                    "view",
                    key,
                    "--json",
                    "number,title,body,state,issueType,url",
                ],
                RunOutput {
                    exit_code: 0,
                    stdout: issue.to_vec(),
                    stderr: Vec::new(),
                },
            );
        }

        let code = run_subcommand_rendered(
            &mut h,
            Sub::Reconcile(ReconcileArgs {
                id: "tk-1".into(),
                backend_key: "42".into(),
                force: false,
            }),
        );

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(h.out(), "Promoted Ticket: tk-1 -> gh-42\n");
        assert_eq!(item_state(&conn, "t1"), ("gh-42".into(), "backend".into()));
        h.runner.assert_all_consumed();
    }

    #[test]
    fn public_retry_dispatch_uses_the_configured_github_adapter() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        conn.execute("update mutations set state = 'applying'", [])
            .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);
        h.runner.expect_exact(
            &[
                "gh",
                "issue",
                "create",
                "--title",
                "Local work",
                "--body",
                "",
            ],
            RunOutput {
                exit_code: 0,
                stdout: b"https://github.com/o/r/issues/42\n".to_vec(),
                stderr: Vec::new(),
            },
        );

        let code = run_subcommand_rendered(&mut h, Sub::Retry(RetryArgs { id: "tk-1".into() }));

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(h.out(), "Promoted Ticket: tk-1 -> gh-42\n");
        assert_eq!(item_state(&conn, "t1"), ("gh-42".into(), "backend".into()));
        h.runner.assert_all_consumed();
    }

    #[test]
    fn public_reconcile_stops_at_remote_workflow_contention() {
        let fixture = TmpStore::new("repo");
        let _conn = seed_store(&fixture);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let blocking_store = open_store(&h, &fixture, &cwd_path);
        let _guard = blocking_store.lock_remote_workflow().unwrap();
        expect_git(&h, &fixture);

        let code = run_subcommand_rendered(
            &mut h,
            Sub::Reconcile(ReconcileArgs {
                id: "tk-1".into(),
                backend_key: "42".into(),
                force: false,
            }),
        );

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk promote: another remote-changing command is running; retry when it finishes\n"
        );
        h.runner.assert_all_consumed();
    }

    #[test]
    fn public_retry_stops_at_remote_workflow_contention() {
        let fixture = TmpStore::new("repo");
        let _conn = seed_store(&fixture);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let blocking_store = open_store(&h, &fixture, &cwd_path);
        let _guard = blocking_store.lock_remote_workflow().unwrap();
        expect_git(&h, &fixture);

        let code = run_subcommand_rendered(&mut h, Sub::Retry(RetryArgs { id: "tk-1".into() }));

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk promote: another remote-changing command is running; retry when it finishes\n"
        );
        h.runner.assert_all_consumed();
    }

    #[test]
    fn public_retry_reports_a_known_item_without_recoverable_promotion() {
        let fixture = TmpStore::new("repo");
        let conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);

        let code = run_subcommand_rendered(&mut h, Sub::Retry(RetryArgs { id: "tk-1".into() }));

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk promote: 'tk-1' has no nonterminal Promotion to recover\n"
        );
        h.runner.assert_all_consumed();
    }

    #[test]
    fn exact_reconcile_attaches_then_imports_backend_status() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        conn.execute("update mutations set state = 'applying'", [])
            .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut store = open_store(&h, &fixture, &cwd_path);
        let mut fake = FakeAdapter::new()
            .with_inspections(vec![inspection(
                "gh-42",
                "https://github.com/o/r/issues/42",
                "Local work",
                "",
            )])
            .with_refreshes(vec![refresh("Local work", "", ItemStatus::Done)]);

        let code = reconcile_rendered(&mut h, &mut store, &mut fake, "tk-1", "42", false);

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(h.out(), "Promoted Ticket: tk-1 -> gh-42\n");
        let row: (String, String, String) = conn
            .query_row(
                "select display_value, origin, status from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, ("gh-42".into(), "backend".into(), "done".into()));
        assert_eq!(fake.captured_inspection_keys, vec!["42"]);
        assert_eq!(
            fake.captured_refresh_keys,
            vec!["https://github.com/o/r/issues/42"]
        );
    }

    #[test]
    fn reconcile_refuses_content_mismatch_without_writing() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        conn.execute("update mutations set state = 'applying'", [])
            .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut store = open_store(&h, &fixture, &cwd_path);
        let mut fake = FakeAdapter::new().with_inspections(vec![inspection(
            "gh-42",
            "42",
            "Other title",
            "Other body",
        )]);

        let code = reconcile_rendered(&mut h, &mut store, &mut fake, "tk-1", "42", false);

        assert_eq!(code, Exit::Failure);
        assert_eq!(h.out(), "");
        assert_eq!(
            h.err(),
            "tk promote: Backend item gh-42 does not match the retained Promotion snapshot (title and body)\n\
             Re-run with '--force' only after confirming it is the object this Promotion created.\n"
        );
        assert_eq!(item_state(&conn, "t1"), ("tk-1".into(), "local".into()));
        let state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "applying");
    }

    #[test]
    fn forced_reconcile_converges_current_local_content_with_the_same_operation() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        conn.execute_batch(
            "update mutations set state = 'applying'; update items set title = 'Current local', body = 'Current body' where id = 't1'",
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut store = open_store(&h, &fixture, &cwd_path);
        let mut fake = FakeAdapter::new()
            .with_inspections(vec![inspection(
                "gh-42",
                "42",
                "Different Backend title",
                "Different Backend body",
            )])
            .with_refreshes(vec![refresh(
                "Different Backend title",
                "Different Backend body",
                ItemStatus::Open,
            )])
            .with_edits(vec![EditResponse::Success]);

        let code = reconcile_rendered(&mut h, &mut store, &mut fake, "tk-1", "42", true);

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(h.out(), "Promoted Ticket: tk-1 -> gh-42\n");
        let BackendEdit::UpdateTicket { snapshot, .. } = &fake.captured_edits[0] else {
            panic!("forced convergence must append a Ticket update")
        };
        assert_eq!(snapshot.title, "Current local");
        assert_eq!(snapshot.body, "Current body");
        let operations: Vec<String> = {
            let mut stmt = conn
                .prepare("select promotion_operation_id from mutations order by sequence")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(operations.len(), 2);
        assert_eq!(operations[0], operations[1]);
    }

    #[test]
    fn reconcile_reports_every_promotion_that_nested_sync_lands() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        local_ticket(&conn, "t2", "tk-2", 2);
        commit_promotion(&mut conn, "t1");
        commit_promotion(&mut conn, "t2");
        conn.execute(
            "update mutations set state = 'applying' where sequence = 1",
            [],
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut store = open_store(&h, &fixture, &cwd_path);
        let mut fake = FakeAdapter::new()
            .with_inspections(vec![inspection("gh-1", "1", "Local work", "")])
            .with_refreshes(vec![refresh("Local work", "", ItemStatus::Open)])
            .with_creates(vec![CreateResponse::Created {
                backend_key: "2".into(),
                display_id: "gh-2".into(),
            }]);

        let code = reconcile_rendered(&mut h, &mut store, &mut fake, "tk-1", "1", false);

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(
            h.out(),
            "Promoted Ticket: tk-1 -> gh-1\nPromoted Ticket: tk-2 -> gh-2\n"
        );
    }

    #[test]
    fn reconcile_renders_landed_mapping_before_a_later_certified_rejection() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        local_ticket(&conn, "t2", "tk-2", 2);
        commit_promotion(&mut conn, "t1");
        commit_promotion(&mut conn, "t2");
        conn.execute(
            "update mutations set state = 'applying' where sequence = 1",
            [],
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut store = open_store(&h, &fixture, &cwd_path);
        let mut fake = FakeAdapter::new()
            .with_inspections(vec![inspection("gh-1", "1", "Local work", "")])
            .with_refreshes(vec![refresh("Local work", "", ItemStatus::Open)])
            .with_creates(vec![CreateResponse::Rejected(
                "Backend validation rejected tk-2".into(),
            )]);

        let code = reconcile_rendered(&mut h, &mut store, &mut fake, "tk-1", "1", false);

        assert_eq!(code, Exit::Failure);
        assert_eq!(h.out(), "Promoted Ticket: tk-1 -> gh-1\n");
        assert!(h.err().contains("sync stopped at Mutation 2"));
        assert!(h.err().contains("tk sync log 2"));
        assert!(h.err().contains("run 'tk sync' to continue the queue"));
    }

    #[test]
    fn retry_reenters_normal_sync_and_reports_the_mapping() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        conn.execute("update mutations set state = 'applying'", [])
            .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut store = open_store(&h, &fixture, &cwd_path);
        let mut fake = FakeAdapter::new().with_creates(vec![CreateResponse::Created {
            backend_key: "42".into(),
            display_id: "gh-42".into(),
        }]);

        let code = retry_rendered(&mut h, &mut store, &mut fake, "tk-1");

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(h.out(), "Promoted Ticket: tk-1 -> gh-42\n");
        assert_eq!(fake.captured_creates.len(), 1);
    }

    #[test]
    fn retry_that_is_still_indeterminate_restores_safe_recovery_guidance() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        conn.execute("update mutations set state = 'applying'", [])
            .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut store = open_store(&h, &fixture, &cwd_path);
        let mut fake = FakeAdapter::new().with_creates(vec![CreateResponse::Indeterminate(
            "request outcome unknown".into(),
        )]);

        let code = retry_rendered(&mut h, &mut store, &mut fake, "tk-1");

        assert_eq!(code, Exit::Failure);
        assert!(h.err().contains("indeterminate Backend creation outcome"));
        assert!(h.err().contains("tk promote reconcile"));
        let state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "applying");
    }

    // ---- cancel ----------------------------------------------------------

    #[test]
    fn cancel_withdraws_the_operation_and_returns_the_item_to_local() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);

        let code = cancel_rendered(&mut h, "tk-1");

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(h.out(), "Cancelled Promotion: Ticket tk-1\n");
        let conn = Connection::open(fixture.db_path()).unwrap();
        assert_eq!(
            crate::store::mutations::resolve_backend_binding(&conn, "t1").unwrap(),
            BackendBinding::Local
        );
    }

    #[test]
    fn cancel_needs_no_adapter_so_a_cleared_remote_still_lets_it_run() {
        // The exit of last resort must not depend on the Remote that produced
        // the stuck Promotion (ADR-0038).
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        conn.execute("delete from sync_cursors", []).unwrap();
        conn.execute("delete from remotes", []).unwrap();
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);

        let code = cancel_rendered(&mut h, "tk-1");

        assert_eq!(code, Exit::Ok, "{}", h.err());
    }

    #[test]
    fn cancel_reports_a_withdrawal_that_reaches_a_backend_backed_target() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_epic(&conn, "e1", "tk-1", 1);
        commit_promotion(&mut conn, "e1");
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "backend",
                display: "gh-9",
                title: "Backend Ticket",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 9,
                mutation_type: "add_ticket_to_epic",
                item_id: "backend",
                payload_json: r#"{"epic_id":"e1"}"#,
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);

        let code = cancel_rendered(&mut h, "tk-1");

        assert_eq!(code, Exit::Ok, "{}", h.err());
        assert_eq!(
            h.out(),
            "Cancelled Promotion: Epic tk-1\n\
             Withdrew add_ticket_to_epic for gh-9 (Mutation 9)\n"
        );
    }

    #[test]
    fn cancel_refuses_an_indeterminate_creation_and_names_both_recoveries() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        conn.execute("update mutations set state = 'applying'", [])
            .unwrap();
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);

        let code = cancel_rendered(&mut h, "tk-1");

        assert_eq!(code, Exit::Failure);
        assert!(h.err().contains("indeterminate Backend creation outcome"));
        assert!(h.err().contains("tk promote reconcile tk-1"));
        assert!(h.err().contains("tk promote retry tk-1"));
    }

    #[test]
    fn cancel_refuses_a_dependency_the_withdrawal_would_strand() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "backend",
                display: "gh-9",
                title: "Backend Ticket",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        commit_promotion(&mut conn, "t1");
        insert_dependency(&conn, "t1", "backend").unwrap();
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);

        let code = cancel_rendered(&mut h, "tk-1");

        assert_eq!(code, Exit::Failure);
        assert!(h.err().contains("tk unblock gh-9 tk-1"), "{}", h.err());
    }

    #[test]
    fn cancelling_an_already_withdrawn_operation_says_so() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        drop(conn);
        let cwd_path = cwd();
        {
            let mut h = Harness::new(&cwd_path);
            expect_git(&h, &fixture);
            assert_eq!(cancel_rendered(&mut h, "tk-1"), Exit::Ok);
        }

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);
        let code = cancel_rendered(&mut h, "tk-1");

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk promote: the Promotion Operation for tk-1 has already resolved; there is no Promotion left to withdraw\n"
        );
    }

    #[test]
    fn cancel_refuses_an_item_with_no_nonterminal_promotion() {
        let fixture = TmpStore::new("repo");
        let conn = seed_store(&fixture);
        local_ticket(&conn, "t1", "tk-1", 1);
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &fixture);

        let code = cancel_rendered(&mut h, "tk-1");

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk promote: 'tk-1' has no nonterminal Promotion to recover\n"
        );
    }

    #[test]
    fn recovery_sync_created_identity_failure_preserves_reconcile_only_guidance() {
        let error = RunSyncError::CreatedIdentityNotStored {
            sequence: 7,
            identity: BackendItemIdentity {
                backend_key: "https://github.com/o/r/issues/42".into(),
                display_id: "gh-42".into(),
            },
            source: PersistMutationOutcomeError::TargetNotLocal {
                sequence: 7,
                item_id: "item-1".into(),
            },
        };

        let failure = finish_recovery_sync(Err(error), RecoveryAction::Retry).unwrap_err();
        let mut rendered = Vec::new();
        failure.render(&mut rendered, "promote");
        let rendered = String::from_utf8(rendered).unwrap();

        assert!(rendered.contains("the Promotion was retried"));
        assert!(rendered.contains("Backend created gh-42"));
        assert!(rendered.contains("Repository Store corruption or a Ticket bug"));
        assert!(rendered.contains("Mutation 7 remains applying"));
        assert!(rendered.contains("tk promote reconcile"));
        assert!(!rendered.contains("tk promote retry <id>"));
    }

    #[test]
    fn created_identity_busy_failure_never_recommends_retrying_creation() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        let error = RunSyncError::CreatedIdentityNotStored {
            sequence: 7,
            identity: BackendItemIdentity {
                backend_key: "https://github.com/o/r/issues/42".into(),
                display_id: "gh-42".into(),
            },
            source: PersistMutationOutcomeError::Storage(busy),
        };

        let failure = finish_recovery_sync(Err(error), RecoveryAction::Retry).unwrap_err();
        let mut rendered = Vec::new();
        failure.render(&mut rendered, "promote");
        let rendered = String::from_utf8(rendered).unwrap();

        assert!(rendered.contains("Backend created gh-42"));
        assert!(rendered.contains("Repository Store was busy while saving"));
        assert!(rendered.contains("Mutation 7 remains applying"));
        assert!(rendered.contains("tk promote reconcile"));
        assert!(!rendered.contains("retry the command"));
        assert!(!rendered.contains("tk promote retry <id>"));
    }

    #[test]
    fn recovery_sync_storage_failure_preserves_busy_retry_classification() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );

        let failure = finish_recovery_sync(
            Err(RunSyncError::Outcome(PersistMutationOutcomeError::Storage(
                busy,
            ))),
            RecoveryAction::Reconcile,
        )
        .unwrap_err();
        let mut rendered = Vec::new();
        failure.render(&mut rendered, "promote");

        assert_eq!(
            String::from_utf8(rendered).unwrap(),
            "tk promote: the Promotion was reconciled, but sync did not finish\n\
             Repository Store is busy; retry the command\n"
        );
    }

    #[test]
    fn recovery_sync_generic_failure_keeps_the_partial_success_context() {
        let failure = finish_recovery_sync(
            Err(RunSyncError::Outcome(
                PersistMutationOutcomeError::MutationNotFound(8),
            )),
            RecoveryAction::Retry,
        )
        .unwrap_err();
        let mut rendered = Vec::new();
        failure.render(&mut rendered, "promote");
        assert_eq!(
            String::from_utf8(rendered).unwrap(),
            "tk promote: the Promotion was retried, but sync did not finish\n\
             mutation 8 not found\n"
        );
    }

    #[test]
    fn mapping_failure_cannot_hide_an_indeterminate_creation() {
        let mapping_error =
            CommandError::failure("Repository Store corruption: Promotion alias tk-2 disappeared");

        let failure = finish_recovery(
            Err(RunSyncError::ApplyingMutation(7)),
            Err(mapping_error),
            RecoveryAction::Retry,
        )
        .unwrap_err();
        let mut rendered = Vec::new();
        failure.render(&mut rendered, "promote");
        let rendered = String::from_utf8(rendered).unwrap();

        assert!(rendered.contains("indeterminate Backend creation outcome"));
        assert!(rendered.contains("tk promote reconcile"));
        assert!(rendered.contains("tk promote retry"));
        assert!(rendered.contains("Additionally"));
        assert!(rendered.contains("Promotion alias tk-2 disappeared"));
    }

    // ---- finding rendering ----------------------------------------------

    fn item_ref(display_id: &str) -> ItemRef {
        ItemRef {
            id: format!("internal-{display_id}"),
            display_id: display_id.to_owned(),
        }
    }

    fn rendered(finding: &PromotionFinding) -> String {
        render_finding(finding, BackendKind::Github)
    }

    #[test]
    fn a_triage_finding_points_at_tk_accept() {
        assert_eq!(
            rendered(&PromotionFinding::TriageTicket {
                item: item_ref("tk-1")
            }),
            "tk-1 is in triage; run 'tk accept tk-1 --priority P0..P4' before promoting it."
        );
    }

    #[test]
    fn an_item_class_finding_names_the_class_and_the_backend() {
        assert_eq!(
            rendered(&PromotionFinding::ItemClassNotRepresentable {
                item: item_ref("tk-1"),
                item_class: ItemClass::Epic,
            }),
            "tk-1: the github Backend cannot create Epics under Promotion."
        );
    }

    #[test]
    fn a_ticket_kind_finding_names_the_kind_and_the_backend() {
        assert_eq!(
            rendered(&PromotionFinding::TicketKindNotRepresentable {
                item: item_ref("tk-2"),
                ticket_kind: TicketKind::Bug,
            }),
            "tk-2: the github Backend cannot create Bug Tickets under Promotion."
        );
    }

    #[test]
    fn a_rejected_dependency_offers_promoting_the_blocking_item() {
        assert_eq!(
            rendered(&PromotionFinding::DependencyRejected {
                blocked: item_ref("tk-1"),
                blocking: item_ref("tk-2"),
                reason: DependencyRejection::BackendBlockedLocalBlocking,
            }),
            "tk-1 would be backend-backed while its Blocking Item tk-2 stays local. \
             Promote tk-2 in the same operation, or run 'tk unblock tk-1 tk-2' to drop the Dependency."
        );
    }

    #[test]
    fn a_cross_backend_dependency_offers_only_unblocking() {
        // No Promotion moves either endpoint onto the other's Backend, so
        // dropping the edge is the only remedy the planner can offer.
        assert_eq!(
            rendered(&PromotionFinding::DependencyRejected {
                blocked: item_ref("tk-1"),
                blocking: item_ref("jira-7"),
                reason: DependencyRejection::BackendKindMismatch,
            }),
            "tk-1 and jira-7 would be backed by different Backends. \
             Run 'tk unblock tk-1 jira-7' to drop the Dependency."
        );
    }

    #[test]
    fn an_unrepresentable_dependency_names_both_endpoints() {
        assert_eq!(
            rendered(&PromotionFinding::DependencyNotRepresentable {
                blocked: item_ref("tk-1"),
                blocking: item_ref("gh-9"),
            }),
            "tk-1 depends on gh-9, and the github Backend cannot represent a Dependency under Promotion."
        );
    }

    #[test]
    fn an_unrepresentable_membership_names_the_ticket_and_the_epic() {
        assert_eq!(
            rendered(&PromotionFinding::EpicMembershipNotRepresentable {
                ticket: item_ref("tk-2"),
                epic: item_ref("tk-1"),
            }),
            "tk-2 belongs to Epic tk-1, and the github Backend cannot represent Epic membership under Promotion."
        );
    }

    #[test]
    fn a_refusal_lists_every_finding_under_one_headline() {
        // The seam frames the headline only; findings ride after it, one per
        // line, in the order the planner collected them.
        let findings = vec![
            PromotionFinding::ItemClassNotRepresentable {
                item: item_ref("tk-1"),
                item_class: ItemClass::Epic,
            },
            PromotionFinding::TriageTicket {
                item: item_ref("tk-2"),
            },
            PromotionFinding::EpicMembershipNotRepresentable {
                ticket: item_ref("tk-2"),
                epic: item_ref("tk-1"),
            },
        ];

        let mut out = Vec::new();
        refusal("tk-1", &findings, BackendKind::Github).render(&mut out, "promote");

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "tk promote: cannot promote tk-1:\n  \
             tk-1: the github Backend cannot create Epics under Promotion.\n  \
             tk-2 is in triage; run 'tk accept tk-2 --priority P0..P4' before promoting it.\n  \
             tk-2 belongs to Epic tk-1, and the github Backend cannot represent Epic membership under Promotion.\n"
        );
    }

    // ---- the operation, against a scripted Adapter -----------------------

    #[test]
    fn a_local_ticket_promotes_and_reports_its_backend_display_id() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_ticket(&conn, "t1", "tk-1", 1);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        let mut fake = adapter(
            vec![],
            vec![CreateResponse::Created {
                backend_key: "42".into(),
                display_id: "gh-42".into(),
            }],
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", false);

        assert_eq!(code, Exit::Ok, "stderr={}", h.err());
        assert_eq!(h.out(), "Promoted Ticket: tk-1 -> gh-42\n");
        assert_eq!(item_state(&conn, "t1"), ("gh-42".into(), "backend".into()));
        let state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "applied");
    }

    #[test]
    fn children_promotes_the_epic_and_its_local_children() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_epic(&conn, "e1", "tk-1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "c1",
                display: "tk-2",
                title: "Child",
                container_id: Some("e1"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        // Promotions first, then the membership the operation makes intent.
        let mut fake = adapter(
            vec![EditResponse::Success],
            vec![
                CreateResponse::Created {
                    backend_key: "1".into(),
                    display_id: "gh-1".into(),
                },
                CreateResponse::Created {
                    backend_key: "2".into(),
                    display_id: "gh-2".into(),
                },
            ],
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", true);

        assert_eq!(code, Exit::Ok, "stderr={}", h.err());
        assert_eq!(
            h.out(),
            "Promoted Epic: tk-1 -> gh-1\nPromoted Ticket: tk-2 -> gh-2\n"
        );
        assert_eq!(item_state(&conn, "e1"), ("gh-1".into(), "backend".into()));
        assert_eq!(item_state(&conn, "c1"), ("gh-2".into(), "backend".into()));
    }

    #[test]
    fn a_dependency_reaches_the_backend_with_both_endpoints_resolved() {
        // ADR-0036 requires both Promotion receipts before relationship
        // delivery; otherwise the Dependency cannot be addressed.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_epic(&conn, "e1", "tk-1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "c1",
                display: "tk-2",
                title: "Blocking child",
                container_id: Some("e1"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "c2",
                display: "tk-3",
                title: "Blocked child",
                container_id: Some("e1"),
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "c1", "c2").unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        // Three Promotions, then the two memberships and the Dependency.
        let mut fake = adapter(
            vec![
                EditResponse::Success,
                EditResponse::Success,
                EditResponse::Success,
            ],
            vec![
                CreateResponse::Created {
                    backend_key: "1".into(),
                    display_id: "gh-1".into(),
                },
                CreateResponse::Created {
                    backend_key: "2".into(),
                    display_id: "gh-2".into(),
                },
                CreateResponse::Created {
                    backend_key: "3".into(),
                    display_id: "gh-3".into(),
                },
            ],
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", true);

        assert_eq!(code, Exit::Ok, "stderr={}", h.err());
        let dependency = fake
            .captured_edits
            .iter()
            .find(|call| matches!(call, BackendEdit::AddDependency { .. }))
            .expect("the plan queues the Dependency between the two Promotion Children");
        let BackendEdit::AddDependency {
            blocked, blocking, ..
        } = dependency
        else {
            unreachable!()
        };
        assert_eq!(
            (blocked.backend_key.as_str(), blocking.backend_key.as_str()),
            ("3", "2")
        );
    }

    #[test]
    fn an_already_backend_target_appends_nothing_and_succeeds() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-7",
                title: "Adopted",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("7"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        let mut fake = adapter_with_refresh(vec![], vec![]);

        let code = promote_rendered(&mut h, &mut st, &mut fake, "gh-7", false);

        assert_eq!(code, Exit::Ok, "stderr={}", h.err());
        assert_eq!(h.out(), "Already promoted: gh-7\n");
        assert_eq!(mutation_count(&conn).unwrap(), 0);
        // The sync still ran: the Adopted working set's key was pulled.
        assert_eq!(fake.captured_refresh_keys, vec!["7".to_string()]);
    }

    #[test]
    fn nothing_to_promote_still_reports_a_drain_that_did_not_finish() {
        // The empty plan is the re-invocation case, so this run's whole job was
        // to drain the earlier Promotion. Exiting 0 with an empty stderr would
        // tell an agent the Promotion landed while it is still pending.
        let store = TmpStore::new("repo");
        let mut conn = seed_store(&store);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        let mut fake = adapter(
            vec![],
            vec![CreateResponse::Rejected(
                "executable not found on PATH".into(),
            )],
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", false);

        assert_eq!(code, Exit::Failure);
        assert_eq!(h.out(), "Promotion already pending: tk-1\n");
        assert_eq!(
            h.err(),
            "tk promote: nothing to promote, but the sync that followed stopped at Mutation 1\n"
        );
    }

    #[test]
    fn an_already_pending_target_appends_nothing_and_still_syncs() {
        let store = TmpStore::new("repo");
        let mut conn = seed_store(&store);
        local_ticket(&conn, "t1", "tk-1", 1);
        commit_promotion(&mut conn, "t1");
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        let mut fake = adapter(
            vec![],
            vec![CreateResponse::Created {
                backend_key: "42".into(),
                display_id: "gh-42".into(),
            }],
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", false);

        assert_eq!(code, Exit::Ok, "stderr={}", h.err());
        assert_eq!(
            mutation_count(&conn).unwrap(),
            1,
            "re-invoking on a Pending Promotion appends nothing"
        );
        // The sync this invocation ran is what applied the earlier Promotion,
        // so the mapping is real and gets rendered.
        assert_eq!(
            h.out(),
            "Promotion already pending: tk-1\nPromoted Ticket: tk-1 -> gh-42\n"
        );
    }

    #[test]
    fn a_partial_batch_prints_what_landed_and_exits_failure() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_epic(&conn, "e1", "tk-1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "c1",
                display: "tk-2",
                title: "Child",
                container_id: Some("e1"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        let mut fake = adapter(
            vec![],
            vec![
                CreateResponse::Created {
                    backend_key: "1".into(),
                    display_id: "gh-1".into(),
                },
                CreateResponse::Rejected("HTTP 422: title required".into()),
            ],
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", true);

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.out(),
            "Promoted Epic: tk-1 -> gh-1\n",
            "only the mapping that persisted is rendered"
        );
        assert_eq!(
            h.err(),
            "tk promote: the Promotion did not finish: Mutation 2 (failed) for tk-2 is unresolved\n\
             Inspect it with 'tk sync log 2', then run 'tk sync' to apply the rest of the Promotion.\n"
        );
    }

    #[test]
    fn an_indeterminate_creation_warns_not_to_retry_sync() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_ticket(&conn, "t1", "tk-1", 1);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        let mut fake = adapter(
            vec![],
            vec![CreateResponse::Indeterminate(
                "request outcome unknown".into(),
            )],
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", false);

        assert_eq!(code, Exit::Failure);
        assert_eq!(h.out(), "");
        assert_eq!(
            h.err(),
            "tk promote: the Promotion did not finish: Mutation 1 (applying) for tk-1 is unresolved\n\
             Sync stopped: Mutation 1 has an indeterminate Backend creation outcome\n\
             Inspect it with 'tk sync log 1'. Then use 'tk promote reconcile <id> <backend-key>' if the Backend object exists, or 'tk promote retry <id>' only when creating it again is safe.\n"
        );
    }

    /// ADR-0037 lets a Promotion commit behind an unresolved creation, so this
    /// diagnostic has to name the recovery commands. Automatic sync can never
    /// clear an `applying` blocker, and recommending it would loop the operator.
    #[test]
    fn an_older_applying_mutation_points_at_promotion_recovery() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_ticket(&conn, "t0", "tk-9", 1);
        local_ticket(&conn, "t1", "tk-1", 2);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t0",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "applying",
                failure_json: Some(r#"{"detail":"gh timed out"}"#),
                promotion_operation_id: Some("op-old"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        conn.execute(
            "update sequences set value = 1 where name = 'mutation_seq'",
            [],
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        let mut fake = adapter_with_refresh(vec![], vec![]);

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", false);

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk promote: the Promotion is committed and remains pending behind Mutation 1 (applying) for tk-9\n\
             Sync stopped: Mutation 1 has an indeterminate Backend creation outcome\n\
             Inspect it with 'tk sync log 1'. Then use 'tk promote reconcile tk-9 <backend-key>' if the Backend object exists, or 'tk promote retry tk-9' only when creating it again is safe.\n"
        );
        let state: String = conn
            .query_row("select state from mutations where sequence = 2", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            state, "pending",
            "the Promotion is durable behind the barrier"
        );
    }

    /// The operator's most likely mistake is naming an object tk already
    /// tracks — often one Adopt imported while the Promotion hung. That must
    /// read as a collision, not as the uniqueness constraint underneath.
    #[test]
    fn reconcile_refuses_a_backend_object_tk_already_tracks() {
        let fixture = TmpStore::new("repo");
        let mut conn = seed_store(&fixture);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "adopted",
                display: "gh-42",
                title: "Already tracked",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("42"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        local_ticket(&conn, "t1", "tk-1", 2);
        commit_promotion(&mut conn, "t1");
        conn.execute("update mutations set state = 'applying'", [])
            .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut store = open_store(&h, &fixture, &cwd_path);
        let mut fake =
            FakeAdapter::new().with_inspections(vec![inspection("gh-42", "42", "Local work", "")]);

        let code = reconcile_rendered(&mut h, &mut store, &mut fake, "tk-1", "42", false);

        assert_eq!(code, Exit::Failure);
        assert_eq!(h.out(), "");
        assert_eq!(
            h.err(),
            "tk promote: Backend object gh-42 is already tracked by tk, so the Promotion for tk-1 was left unchanged\n\
             Confirm the Backend object this Promotion created, or resolve the duplicate first.\n"
        );
        let row: (String, String) = conn
            .query_row(
                "select i.origin, m.state from items i join mutations m on m.item_id = i.id \
                   where i.id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("local".into(), "applying".into()));
    }

    #[test]
    fn an_older_failed_mutation_leaves_the_promotion_pending_behind_it() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "adopted",
                display: "gh-9",
                title: "Adopted",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        local_ticket(&conn, "t1", "tk-1", 2);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "adopted",
                payload_json: r#"{"title":"Edited","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"HTTP 403"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        // The fixture insert bypasses the outbox writer, so the counter has to
        // be advanced by hand for the Promotion to land behind sequence 1.
        conn.execute(
            "update sequences set value = 1 where name = 'mutation_seq'",
            [],
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        let mut fake = adapter_with_refresh(
            vec![EditResponse::RecordedFailure("HTTP 403".into())],
            vec![],
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", false);

        assert_eq!(code, Exit::Failure);
        assert_eq!(h.out(), "", "no receipt landed, so no mapping is rendered");
        assert_eq!(
            h.err(),
            "tk promote: the Promotion is committed and remains pending behind Mutation 1 (failed) for gh-9\n\
             Resolve that Mutation — 'tk sync log 1' shows why it stopped — then run 'tk sync' to apply the Promotion.\n"
        );
        let (state, origin): (String, String) = conn
            .query_row(
                "select m.state, i.origin from mutations m join items i on i.id = m.item_id \
                   where m.sequence = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (state.as_str(), origin.as_str()),
            ("pending", "local"),
            "the Promotion is durable and still applicable"
        );
    }

    #[test]
    fn a_certified_creation_rejection_reports_where_the_promotion_stands() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_ticket(&conn, "t1", "tk-1", 1);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        let mut fake = adapter(
            vec![],
            vec![CreateResponse::Rejected(
                "executable not found on PATH".into(),
            )],
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", false);

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk promote: the Promotion did not finish: Mutation 1 (failed) for tk-1 is unresolved\n\
             Inspect it with 'tk sync log 1', then run 'tk sync' to apply the rest of the Promotion.\n"
        );
        assert_eq!(mutation_count(&conn).unwrap(), 1);
    }

    #[test]
    fn a_rejected_dependency_from_a_real_graph_refuses_before_any_backend_call() {
        // The planner judges the edge against the Origins the operation *will*
        // produce: the Promotion Child becomes backend-backed while the Item it
        // waits on stays local.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        local_epic(&conn, "e1", "tk-1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "c1",
                display: "tk-2",
                title: "Child",
                container_id: Some("e1"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        local_ticket(&conn, "outside", "tk-3", 3);
        insert_dependency(&conn, "outside", "c1").unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let mut st = open_store(&h, &store, &cwd_path);
        // Dependencies are the only facet this Backend cannot represent, so the
        // rejected edge is the finding, not a capability complaint.
        let mut fake = FakeAdapter::new().with_capabilities(
            PromotionCapabilities::none()
                .with_item_class(ItemClass::Ticket)
                .with_item_class(ItemClass::Epic)
                .with_ticket_kind(TicketKind::Task)
                .with_epic_membership(),
        );

        let code = promote_rendered(&mut h, &mut st, &mut fake, "tk-1", true);

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk promote: cannot promote tk-1:\n  \
             tk-2 would be backend-backed while its Blocking Item tk-3 stays local. \
             Promote tk-3 in the same operation, or run 'tk unblock tk-2 tk-3' to drop the Dependency.\n"
        );
        assert_eq!(
            mutation_count(&conn).unwrap(),
            0,
            "a refused preflight writes nothing"
        );
        assert!(
            fake.captured_adopt_inputs.is_empty()
                && fake.captured_refresh_keys.is_empty()
                && fake.captured_edits.is_empty()
                && fake.captured_creates.is_empty(),
            "a refused preflight calls no Backend"
        );
    }

    // ---- unresolved-failure dispatch -------------------------------------

    fn status(sequence: i64, state: MutationState, display: &str) -> MutationSummary {
        MutationSummary {
            sequence,
            state,
            target_display_id: display.to_owned(),
        }
    }

    #[test]
    fn an_operations_own_mutation_is_reported_as_the_promotion_not_finishing() {
        let unresolved = status(4, MutationState::Failed, "tk-2");
        let err = unresolved_failure(Some(&unresolved), &unresolved, None);

        let mut out = Vec::new();
        err.render(&mut out, "promote");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "tk promote: the Promotion did not finish: Mutation 4 (failed) for tk-2 is unresolved\n\
             Inspect it with 'tk sync log 4', then run 'tk sync' to apply the rest of the Promotion.\n"
        );
    }

    #[test]
    fn a_blocker_with_no_applicable_row_falls_back_to_the_operations_own_mutation() {
        // A skipped Mutation of the operation is unresolved but not applicable,
        // so there may be no blocker at all to compare against.
        let err = unresolved_failure(None, &status(4, MutationState::Skipped, "tk-2"), None);

        let mut out = Vec::new();
        err.render(&mut out, "promote");
        assert!(
            String::from_utf8(out)
                .unwrap()
                .starts_with("tk promote: the Promotion did not finish: Mutation 4 (skipped)"),
        );
    }
}
