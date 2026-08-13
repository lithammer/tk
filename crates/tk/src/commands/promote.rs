//! `tk promote` — convert a Local Ticket or Local Epic into a backend-backed
//! object through the configured Remote (CONTEXT.md Promotion).
//!
//! One invocation is one Promotion Operation. The whole operation is
//! preflighted against a Repository Store snapshot before a byte is written
//! (ADR-0035): a refused Promotion leaves the outbox empty and calls no
//! Backend. What survives preflight commits to the Mutation Log in a single
//! local transaction and is then drained by the same [`crate::sync::run_sync`]
//! engine `tk sync` drives, so a Promotion applies behind whatever the outbox
//! already held.
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

use clap::Args as ClapArgs;

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
use crate::remote::adapter::Adapter;
use crate::remote::factory::{self, OpenError as FactoryOpenError};
use crate::store::mutations::AppendError;
use crate::store::promotion::{
    self as store_promotion, CommitPlanError, MutationSummary, ReadGraphError,
};
use crate::store::repository::RemoteWorkflowGuard;
use crate::store::repository::{ResolvedItemRefWithDisplay, Store};
use crate::store::sync::BackendCohortError;
use crate::sync::{self, RunSyncError};

/// Flags for `tk promote`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket or Epic to promote.
    #[arg(value_name = "ID")]
    pub id: String,
    /// Also promote the Epic's directly contained Local Tickets — its
    /// Promotion Children. Epics only.
    #[arg(long)]
    pub children: bool,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();
    let workflow = store
        .lock_remote_workflow()
        .map_err(CommandError::failure)?;

    let target = match resolver::resolve_with_display(&store, &args.id) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "'{id}' is not a known Display ID or Alias",
                id = args.id
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };

    // Only an Epic contains Promotion Children, so `--children` elsewhere is a
    // malformed invocation rather than an operation to refuse.
    if args.children && target.item_class != ItemClass::Epic {
        return Err(CommandError::usage(format!(
            "'{id}' is not an Epic; --children promotes the Promotion Children of an Epic",
            id = args.id
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
                // Never attempted, so there is nothing recorded against it and
                // nothing to resolve — see `recovery_guidance`.
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
            "Inspect it with 'tk sync log {}'; do not run 'tk sync' while it remains applying.",
            mutation.sequence
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::domain::backend_operation::BackendEdit;
    use crate::domain::mutation_state::MutationState;
    use crate::domain::promotion_capability::PromotionCapabilities;
    use crate::domain::ticket_kind::TicketKind;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::promotion::plan::ItemRef;
    use crate::remote::fake::{CreateResponse, EditResponse, FakeAdapter, RefreshResponse};
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
            id: id.into(),
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
             Inspect it with 'tk sync log 1'; do not run 'tk sync' while it remains applying.\n"
        );
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
