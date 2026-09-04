//! `tk sync` and `tk sync log` — Mutation outbox replay and inspection.
//!
//! `tk sync` opens the configured Backend Adapter via
//! [`crate::remote::factory::open_configured`] and drives the backend-blind
//! engine ([`crate::sync::run_sync`]). The engine derives the Adopted working
//! set's active Backend keys and refreshes each through the Adapter before it
//! applies queued Mutations (ADR-0034); unsupported Backend
//! kinds fail while opening the Adapter.
//!
//! `tk sync --skip <id>` curates a failed Mutation under the repository's
//! Remote workflow guard. The skip commits BEFORE the adapter is opened so a
//! broken / unimplemented Remote cannot block an operator from bypassing a
//! Mutation the backend already rejected.
//!
//! `tk sync log` reads the Mutation Log through [`crate::store::sync`]; it
//! needs no adapter and is exercised end-to-end here.

use std::io::Write;

use clap::{Args as ClapArgs, Subcommand};

use crate::cli::{Deps, Exit};
use crate::commands::resolver;
use crate::domain::backend_outcome::FailureClass;
use crate::remote::factory::{self, OpenError as FactoryOpenError};
use crate::render::palette;
use crate::render::sanitize;
use crate::render::styler::SubStyler;
use crate::store::sync::{
    self as store_sync, LogDetailRow, LogError, LogListFilter, LogListRow, MarkSkippedError,
    SkipOutcome,
};
use crate::sync::{
    self, CreatedIdentityNotStoredCause, RunSyncError, RunSyncErrorCategory, SyncReport,
};

const COMMAND: &str = "sync";
const LOG_COMMAND: &str = "sync log";

/// Flags for `tk sync`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub subcommand: Option<Sub>,
    /// Mark one failed Mutation skipped before running sync.
    ///
    /// Skipping a failed close relinquishes it rather than bypassing it: the
    /// Item returns to open and loses its Closing Reason, Dependencies it
    /// resolved as their Blocking Item become unresolved, and an accepted
    /// Ticket becomes selectable by `tk next` again.
    #[arg(long, value_name = "MUTATION-ID")]
    pub skip: Option<i64>,
}

#[derive(Debug, Subcommand)]
pub enum Sub {
    /// Inspect pending, failed, applying, skipped, cancelled, and abandoned
    /// Mutations.
    Log(LogArgs),
}

/// Flags for `tk sync log`. The state flags are a filter; if more than one is
/// given, precedence is pending → failed → skipped → cancelled → abandoned.
/// Applying Mutations appear in the default view.
#[derive(Debug, ClapArgs)]
// One bool per CLI flag at the parser layer; `run_log` collapses them into
// `LogListFilter` before anything reasons over them.
#[allow(clippy::struct_excessive_bools)]
pub struct LogArgs {
    /// Only pending Mutations.
    #[arg(long)]
    pub pending: bool,
    /// Only failed Mutations.
    #[arg(long)]
    pub failed: bool,
    /// Only skipped Mutations.
    #[arg(long)]
    pub skipped: bool,
    /// Only cancelled Mutations.
    #[arg(long)]
    pub cancelled: bool,
    /// Only abandoned Mutations.
    #[arg(long)]
    pub abandoned: bool,
    /// Show one Mutation in detail (Mutation Sequence).
    pub id: Option<i64>,
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> Exit {
    match args.subcommand {
        Some(Sub::Log(log_args)) => run_log(deps, log_args),
        None => run_sync(deps, args.skip),
    }
}

fn run_sync(deps: Deps<'_>, skip: Option<i64>) -> Exit {
    let Deps {
        stdout,
        stderr,
        runner,
        clock,
        cwd,
        ..
    } = deps;

    let mut store = match resolver::open_for_command(runner, cwd, clock) {
        Ok(s) => s,
        Err(err) => {
            resolver::open_error(&err).render(stderr, COMMAND);
            return Exit::Failure;
        }
    };
    let now = clock.now_iso();

    let workflow = match store.lock_remote_workflow() {
        Ok(guard) => guard,
        Err(err) => {
            let _ = writeln!(stderr, "tk sync: {err}");
            return Exit::Failure;
        }
    };

    // Commit the skip before opening the adapter: a broken or unimplemented
    // Remote must not block an operator from bypassing a failed Mutation, and
    // the committed local outcome is reported before Backend work begins
    // (ADR-0046).
    if let Some(seq) = skip {
        match store_sync::mark_mutation_skipped(store.conn_mut(), &workflow, seq, &now) {
            Ok(outcome) => render_skip_outcome(stdout, seq, &outcome),
            Err(err) => {
                render_skip_error(stderr, &err);
                return Exit::Failure;
            }
        }
    }

    let adapter_opt = match factory::open_configured(store.conn(), runner, cwd) {
        Ok(a) => a,
        Err(FactoryOpenError::NotImplemented) => {
            let _ = writeln!(
                stderr,
                "tk sync: the configured Remote's adapter is not implemented in this build"
            );
            return Exit::Failure;
        }
        Err(FactoryOpenError::Storage(err)) => {
            resolver::storage_error(&err).render(stderr, COMMAND);
            return Exit::Failure;
        }
    };
    let Some(mut adapter) = adapter_opt else {
        let _ = writeln!(
            stderr,
            "tk sync: no Remote configured; run 'tk remote set <kind>' first"
        );
        return Exit::Failure;
    };

    let report = match sync::run_sync(store.conn_mut(), &mut *adapter, &workflow, &now) {
        Ok(report) => report,
        Err(err) => {
            render_run_sync_error(stderr, &err);
            return Exit::Failure;
        }
    };
    render_sync_report(stdout, &report);
    if report.stopped_at_sequence.is_some() {
        Exit::Failure
    } else {
        Exit::Ok
    }
}

fn run_log(deps: Deps<'_>, args: LogArgs) -> Exit {
    let Deps {
        stdout,
        stderr,
        runner,
        clock,
        cwd,
        styler,
        ..
    } = deps;
    let styler = styler.for_stdout();

    let store = match resolver::open_for_command(runner, cwd, clock) {
        Ok(s) => s,
        Err(err) => {
            resolver::open_error(&err).render(stderr, LOG_COMMAND);
            return Exit::Failure;
        }
    };

    if let Some(seq) = args.id {
        return match store_sync::show_mutation_log(store.conn(), seq) {
            Ok(detail) => {
                render_log_detail(stdout, &detail, styler);
                Exit::Ok
            }
            Err(LogError::MutationNotFound(seq)) => {
                let _ = writeln!(stderr, "tk sync log: Mutation {seq} not found");
                Exit::Failure
            }
            Err(err) => {
                render_log_error(stderr, &err);
                Exit::Failure
            }
        };
    }

    let filter = if args.pending {
        LogListFilter::Pending
    } else if args.failed {
        LogListFilter::Failed
    } else if args.skipped {
        LogListFilter::Skipped
    } else if args.cancelled {
        LogListFilter::Cancelled
    } else if args.abandoned {
        LogListFilter::Abandoned
    } else {
        LogListFilter::Default
    };

    let rows = match store_sync::list_mutation_log(store.conn(), filter) {
        Ok(rows) => rows,
        Err(err) => {
            render_log_error(stderr, &err);
            return Exit::Failure;
        }
    };

    if rows.is_empty() {
        let message = match filter {
            // The default list is the only filter that leaves a state out, so
            // it is the only one whose empty result can still sit on a log
            // that holds rows. Every other filter names the state it looked
            // for, so its own empty line already says everything.
            LogListFilter::Default => match store_sync::mutation_log_is_empty(store.conn()) {
                Ok(true) => "No Mutations recorded.",
                Ok(false) => "All Mutations applied.",
                Err(err) => {
                    render_log_error(stderr, &err);
                    return Exit::Failure;
                }
            },
            LogListFilter::Pending => "No pending Mutations.",
            LogListFilter::Failed => "No failed Mutations.",
            LogListFilter::Skipped => "No skipped Mutations.",
            LogListFilter::Cancelled => "No cancelled Mutations.",
            LogListFilter::Abandoned => "No abandoned Mutations.",
        };
        let _ = writeln!(stdout, "{message}");
        return Exit::Ok;
    }
    for row in &rows {
        render_log_row(stdout, row, styler);
    }
    Exit::Ok
}

/// Render the one-line sync summary: `Sync complete: <p> pulled, <a> applied`
/// with an optional `, stopped at <seq>` clause.
fn render_sync_report<W: Write + ?Sized>(stdout: &mut W, report: &SyncReport) {
    let _ = write!(
        stdout,
        "Sync complete: {} pulled, {} applied",
        report.pulled_count, report.applied_count
    );
    if let Some(seq) = report.stopped_at_sequence {
        let _ = write!(stdout, ", stopped at {seq}");
    }
    let _ = writeln!(stdout, ".");
}

/// Render the ADR-0046 pre-adapter skip line: the committed local outcome of
/// `--skip`, printed before the adapter is opened so it survives even when
/// the Remote is broken, unconfigured, or unimplemented.
fn render_skip_outcome<W: Write + ?Sized>(stdout: &mut W, seq: i64, outcome: &SkipOutcome) {
    match outcome {
        SkipOutcome::Bypassed => {
            let _ = writeln!(stdout, "Skipped Mutation {seq}.");
        }
        SkipOutcome::RelinquishedClose { display_id } => {
            let _ = writeln!(
                stdout,
                "Skipped Mutation {seq}; restored {display_id} to open."
            );
        }
    }
}

fn render_skip_error<W: Write + ?Sized>(stderr: &mut W, err: &MarkSkippedError) {
    match err {
        MarkSkippedError::MutationNotFailed(seq) => {
            let _ = writeln!(
                stderr,
                "tk sync --skip: Mutation {seq} is not in the failed state; --skip only bypasses failed Mutations"
            );
        }
        MarkSkippedError::MutationNotFound(seq) => {
            let _ = writeln!(stderr, "tk sync --skip: Mutation {seq} not found");
        }
        MarkSkippedError::CannotSkipPromotion(seq) => {
            let _ = writeln!(
                stderr,
                "tk sync --skip: Mutation {seq} is a Promotion; skipping it would leave every Mutation queued behind it with no backend identity to apply against. Use 'tk promote cancel <id>' to withdraw the whole Promotion Operation."
            );
        }
        MarkSkippedError::Transition(_)
        | MarkSkippedError::ReopenMatchedNothing(_)
        | MarkSkippedError::ReopenRefusedByTrigger(_) => {
            let _ = writeln!(
                stderr,
                "tk sync --skip: {err}; this is a Ticket bug — please report it"
            );
        }
        MarkSkippedError::Storage(err) => resolver::storage_error(err).render(stderr, COMMAND),
    }
}

/// Render a [`LogError`] from any `tk sync log` read. Only the SQLite arm is an
/// ordinary storage fault; the rest fall through to the generic frame.
fn render_log_error<W: Write + ?Sized>(stderr: &mut W, err: &LogError) {
    match err {
        LogError::Storage(err) => resolver::storage_error(err).render(stderr, LOG_COMMAND),
        LogError::MutationNotFound(_) | LogError::FailureJson(_) => {
            let _ = writeln!(
                stderr,
                "tk sync log: failed to read Repository Store\n{err}"
            );
        }
    }
}

/// Dispatch a [`RunSyncError`] to its verbatim stderr line. Storage-class and
/// environment failures fall through to the generic frame.
fn render_run_sync_error<W: Write + ?Sized>(stderr: &mut W, err: &RunSyncError) {
    match err.category() {
        RunSyncErrorCategory::BackendDetail(detail) => {
            let _ = writeln!(stderr, "tk sync: {detail}");
        }
        RunSyncErrorCategory::MutationSchemaDrift(_) => {
            let _ = writeln!(
                stderr,
                "tk sync: Mutation Log row has an unrecognised mutation kind; this is a Ticket bug — please report it"
            );
        }
        RunSyncErrorCategory::TicketBug(error) => {
            let _ = writeln!(
                stderr,
                "tk sync: {error}; this is a Ticket bug — please report it"
            );
        }
        RunSyncErrorCategory::Storage(error) => {
            resolver::storage_error(error).render(stderr, COMMAND);
        }
        RunSyncErrorCategory::CreatedIdentityNotStored {
            error,
            sequence,
            cause,
        } => {
            let _ = writeln!(stderr, "tk sync: {error}");
            match cause {
                CreatedIdentityNotStoredCause::TargetNotLocal => {
                    let _ = writeln!(
                        stderr,
                        "This is Repository Store corruption or a Ticket bug — please report it"
                    );
                }
                CreatedIdentityNotStoredCause::Storage(_)
                | CreatedIdentityNotStoredCause::Direct => {}
            }
            let _ = writeln!(
                stderr,
                "Mutation {sequence} remains applying; use 'tk promote reconcile <id> <backend-key>' after confirming the created Backend object"
            );
        }
        RunSyncErrorCategory::Direct(error) => {
            let _ = writeln!(stderr, "tk sync: {error}");
        }
        RunSyncErrorCategory::IndeterminateCreation(sequence) => {
            let _ = writeln!(
                stderr,
                "tk sync: Mutation {sequence} has an indeterminate Backend creation outcome; use 'tk promote reconcile <id> <backend-key>' if the object exists, 'tk promote retry <id>' only when creating it again is safe, or 'tk promote cancel <id>' to withdraw the Promotion Operation, leaving any object it created untracked"
            );
        }
        RunSyncErrorCategory::RemoteChanged => {
            let _ = writeln!(
                stderr,
                "tk sync: the configured Remote changed while contacting the Backend; retry 'tk sync'"
            );
        }
        RunSyncErrorCategory::RepositoryInvariant(error) => {
            let _ = writeln!(
                stderr,
                "tk sync: {error}; this is a Repository Store invariant failure"
            );
        }
    }
}

/// Render one Mutation Log row.
///
/// The state token and the target Display ID carry palette entries; the
/// sequence, the Mutation Type, the timestamp, the `└─` continuation, and
/// the `[class]` tag stay plain. The continuation is plain to match
/// `tk list`, which passes its own tree prefix unstyled, and the `[class]`
/// tag because a Failure Class has no colour meaning anywhere else.
///
/// Writes are unchecked (`let _ =`) like the rest of `tk sync`, where the
/// styled read commands instead propagate `io::Result` and convert once
/// through `cli::write_error`. gh-95 owns unifying the two. Every span here
/// is a self-contained `wrap`, so no dropped write can leave an `open` span
/// unclosed and bleed style into the rest of the terminal.
fn render_log_row<W: Write + ?Sized>(stdout: &mut W, row: &LogListRow, styler: SubStyler) {
    let _ = writeln!(
        stdout,
        "{} {} {} {} {}",
        row.sequence,
        styler.wrap(palette::mutation_state_style(row.state), row.state.text()),
        row.mutation_type,
        styler.wrap(palette::id_style(row.item_class), &row.target_display_id),
        row.created_at
    );
    if let Some(detail) = &row.failure_detail {
        // The class is shown only when the adapter actually classified the
        // failure; an `unknown` row carries no signal, so it renders bare.
        match row.failure_class.filter(|c| *c != FailureClass::Unknown) {
            Some(class) => {
                let _ = write!(stdout, "  └─ [{class}] ");
            }
            None => {
                let _ = write!(stdout, "  └─ ");
            }
        }
        // Backend-controlled text: `failure_detail` decodes from
        // `failure_json`, which an Adapter fills from the Remote's own error
        // output. Sanitised as a line, so CR / LF fold to spaces and a
        // multi-line Remote error cannot rewrite the row layout below it.
        let _ = sanitize::write_sanitized_line(stdout, detail.as_bytes());
        let _ = stdout.write_all(b"\n");
    }
}

/// Render the `tk sync log <sequence>` detail view.
///
/// Styles the same two tokens the list row does — the state, inside its
/// brackets, and the Display ID. The aligned field labels stay plain: the
/// alignment already guides the eye, and `tk show` bolds section headers but
/// has no field labels of its own to match. See [`render_log_row`] on the
/// unchecked writes.
fn render_log_detail<W: Write + ?Sized>(stdout: &mut W, detail: &LogDetailRow, styler: SubStyler) {
    let _ = writeln!(
        stdout,
        "Mutation {}  [{}]",
        detail.sequence,
        styler.wrap(
            palette::mutation_state_style(detail.state),
            detail.state.text()
        )
    );
    let _ = writeln!(stdout, "Type:       {}", detail.mutation_type);
    let _ = writeln!(
        stdout,
        "Target:     {} ({})",
        styler.wrap(
            palette::id_style(detail.item_class),
            &detail.target_display_id
        ),
        detail.item_class
    );
    let _ = writeln!(stdout, "Created:    {}", detail.created_at);
    let _ = writeln!(stdout, "Updated:    {}", detail.state_changed_at);
    // `payload_json` needs no sanitising: it is `serde_json` output, and JSON
    // string escaping already renders every control byte as `\uNNNN` text
    // before it reaches the Repository Store.
    let _ = writeln!(stdout, "Payload:    {}", detail.payload_json);
    if let Some(d) = &detail.failure_detail {
        if let Some(class) = detail.failure_class.filter(|c| *c != FailureClass::Unknown) {
            let _ = writeln!(stdout, "Class:      {class}");
        }
        // Backend-controlled text, as in [`render_log_row`], but sanitised as
        // a body: this block is the one place a Remote's multi-line error is
        // shown whole, so LF stays layout and only control bytes go inert.
        let _ = write!(stdout, "Failure:\n  ");
        let _ = sanitize::write_sanitized_body(stdout, d.as_bytes());
        let _ = stdout.write_all(b"\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::commands::testing::{Harness, cwd, expect_git, expect_github_pull, seed_store};
    use crate::domain::backend_kind::BackendKind;
    use crate::domain::backend_operation::BackendItemIdentity;
    use crate::domain::lifecycle::Lifecycle;
    use crate::domain::mutation_payload::Promotion;
    use crate::domain::mutation_type::MutationType;
    use crate::proc::{FakeRunner, ProcError, RunOutput};
    use crate::remote::adapter::AdapterReadError;
    use crate::render::Styler;
    use crate::store::sync::{
        BackendCohortError, LoadApplicableError, PersistMutationOutcomeError, RefreshStoreError,
    };
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, TmpStore, insert_fixture_item,
        insert_fixture_mutation, insert_fixture_remote,
    };
    use rusqlite::Connection;

    fn backend_ticket(conn: &Connection, id: &str, display: &str, key: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "T",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some(key),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    /// Seed the two-row Mutation Log the list-view tests read: a pending
    /// `update_ticket` on tk-1, then a failed `set_item_status` on tk-2
    /// carrying `failure_json`. The caller supplies the failure so a test can
    /// choose whether the row renders a `[class]` tag.
    fn seed_list_view_log(store: &TmpStore, failure_json: &str) {
        let conn = seed_store(store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
        backend_ticket(&conn, "t2", "tk-2", "2", 2);
        insert_fixture_mutation(
            &conn,
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
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "set_item_status",
                item_id: "t2",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(failure_json),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    /// Seed the single failed Mutation the detail-view tests inspect:
    /// sequence 7, `set_item_status` on tk-1, with an unclassified failure.
    fn seed_detail_view_log(store: &TmpStore, failure_json: &str) {
        let conn = seed_store(store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 7,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(failure_json),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    /// A Failure whose detail carries an SGR escape and a bell, as a Remote
    /// error message relaying user or collaborator content can. `\u001b` is
    /// JSON's spelling of ESC, so the stored `failure_json` decodes to a
    /// detail holding the raw control bytes.
    const HOSTILE_FAILURE_JSON: &str =
        r#"{"detail":"HTTP 422: \u001b[31mred\u0007 title rejected"}"#;

    fn log_args(id: Option<i64>) -> Args {
        Args {
            subcommand: Some(Sub::Log(LogArgs {
                pending: false,
                failed: false,
                skipped: false,
                cancelled: false,
                abandoned: false,
                id,
            })),
            skip: None,
        }
    }

    // ---- tk sync (adapter-reachable paths) ------------------------------

    #[test]
    fn sync_no_remote_returns_1_with_diagnostic() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: None,
            },
        );
        assert_eq!(code, Exit::Failure);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("no Remote configured")
        );
    }

    #[test]
    fn sync_github_with_no_adopted_items_is_a_noop() {
        // github now resolves to a real adapter; with no Adopted items the
        // engine derives an empty key set and makes no gh call.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "github",
                config_json: "{}",
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store); // only git discovery; no gh call expected
        let code = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: None,
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("Sync complete: 0 pulled, 0 applied.")
        );
    }

    #[test]
    fn sync_exits_failure_when_creation_never_started() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
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
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        h.runner.expect_exact_error(
            &[
                "gh",
                "issue",
                "create",
                "--title",
                "Local work",
                "--body",
                "",
            ],
            ProcError::ExecutableNotFound,
        );

        let code = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: None,
            },
        );

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            String::from_utf8(h.stdout).unwrap(),
            "Sync complete: 0 pulled, 0 applied, stopped at 1.\n"
        );
        let state: String = Connection::open(store.db_path())
            .unwrap()
            .query_row(
                "select state from mutations where sequence = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "failed");
    }

    #[test]
    fn sync_github_drives_gh_through_the_factory() {
        // End-to-end wiring: command -> factory -> real GithubAdapter -> gh via
        // the same FakeRunner. An Adopted item with a pending update_ticket
        // refreshes without overwriting the pending edit, then applies through gh.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "github",
                config_json: "{}",
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        let backend_key = "https://github.com/o/r/issues/1";
        backend_ticket(&conn, "t1", "gh-1", backend_key, 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"New Title","body":""}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_github_pull(&h, "o", "r", 1, "Backend", "B", Lifecycle::Open);
        h.runner.expect(
            &["gh", "issue", "edit", backend_key],
            RunOutput {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
        let code = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: None,
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("Sync complete: 1 pulled, 1 applied.")
        );

        let state: String = Connection::open(store.db_path())
            .unwrap()
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "applied");
    }

    #[test]
    fn sync_skip_commits_before_adapter_open() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
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
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        // No Remote configured: sync still exits 1 on no-remote, but the skip
        // committed first.
        let code = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: Some(1),
            },
        );
        assert_eq!(code, Exit::Failure);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("Skipped Mutation 1."),
            "the pre-adapter skip line is reported even though sync then failed"
        );

        let conn = Connection::open(store.db_path()).unwrap();
        let state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "skipped", "skip committed before the no-remote exit");
    }

    #[test]
    fn sync_skip_reports_the_relinquished_close_before_adapter_work() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-1",
                title: "Needs its close relinquished",
                status: "done",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("1"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        // No Remote configured: the skip's line is what this test cares about,
        // reported before sync fails on the missing Remote.
        let code = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: Some(1),
            },
        );
        assert_eq!(code, Exit::Failure);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("Skipped Mutation 1; restored gh-1 to open.")
        );
    }

    #[test]
    fn sync_skip_relinquished_close_reenters_pull_and_reimports_done() {
        // ADR-0046 Consequences: "If the Backend was independently closed,
        // Pull imports `done` again." The reopen has to land before Pull reads
        // the working set, because `working_set_keys` selects `status = 'open'`.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "github",
                config_json: "{}",
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        let backend_key = "https://github.com/o/r/issues/1";
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-1",
                title: "Local title before the refresh",
                status: "done",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some(backend_key),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        // The reopen lands before this Pull call, so the Item is back in the
        // open-only working set; the Backend answers with the issue closed.
        expect_github_pull(
            &h,
            "o",
            "r",
            1,
            "Closed on the Backend",
            "Backend body",
            Lifecycle::Done,
        );
        let code = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: Some(1),
            },
        );
        assert_eq!(code, Exit::Ok);
        assert_eq!(
            String::from_utf8(h.stdout).unwrap(),
            "Skipped Mutation 1; restored gh-1 to open.\nSync complete: 1 pulled, 0 applied.\n"
        );

        // Lifecycle and content together: the reopen puts the Item back in the
        // working set, so this run's merge is free to overwrite title and body
        // from the snapshot. Seeding the two sides differently is what lets
        // that assertion fail if it ever stops holding.
        let (status, title, body): (String, String, String) = Connection::open(store.db_path())
            .unwrap()
            .query_row(
                "select status, title, body from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (status.as_str(), title.as_str(), body.as_str()),
            ("done", "Closed on the Backend", "Backend body"),
            "Pull re-imported the independently closed Item"
        );
    }

    #[test]
    fn sync_skip_reports_a_busy_remote_workflow_guard() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
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
        drop(conn);

        let cwd_path = cwd();
        let holder_runner = FakeRunner::new();
        holder_runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
        let clock = FakeClock::new(1_778_284_800_000);
        let first = resolver::open_for_command(&holder_runner, &cwd_path, &clock).unwrap();
        let first_guard = first.lock_remote_workflow().unwrap();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let exit = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: Some(1),
            },
        );
        assert_eq!(exit, Exit::Failure);
        assert_eq!(
            String::from_utf8(h.stderr).unwrap(),
            "tk sync: another remote-changing command is running; retry when it finishes\n"
        );
        let state: String = first
            .conn()
            .query_row(
                "select state from mutations where sequence = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "failed");
        drop(first_guard);
    }

    #[test]
    fn sync_skip_a_failed_promotion_reports_and_does_not_skip() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
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
                state: "failed",
                failure_json: Some(r#"{"detail":"boom"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: Some(1),
            },
        );
        assert_eq!(code, Exit::Failure);
        assert_eq!(
            String::from_utf8(h.stderr).unwrap(),
            "tk sync --skip: Mutation 1 is a Promotion; skipping it would leave every Mutation \
             queued behind it with no backend identity to apply against. Use 'tk promote cancel \
             <id>' to withdraw the whole Promotion Operation.\n"
        );

        let conn = Connection::open(store.db_path()).unwrap();
        let state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "failed", "the refusal must not commit the skip");
    }

    #[test]
    fn sync_skip_non_failed_reports_and_does_not_skip() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
        insert_fixture_mutation(
            &conn,
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
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                subcommand: None,
                skip: Some(1),
            },
        );
        assert_eq!(code, Exit::Failure);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("is not in the failed state")
        );
    }

    // ---- tk sync log ----------------------------------------------------

    #[test]
    fn sync_log_empty_prints_default_message() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run(h.deps(), log_args(None));
        assert_eq!(code, Exit::Ok);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("No Mutations recorded.")
        );
    }

    #[test]
    fn sync_log_drained_reports_all_applied() {
        // The default list leaves applied Mutations out, so an empty result
        // there does not mean an empty log. A failure here tells an agent its
        // work never reached the Backend when it had already synced.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                state: "applied",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run(h.deps(), log_args(None));

        assert_eq!(code, Exit::Ok);
        assert_eq!(
            String::from_utf8(h.stdout).unwrap(),
            "All Mutations applied.\n"
        );
    }

    /// Holds [`render_log_row`] to its contract under forced colour: state
    /// token and Display ID styled, continuation and `[class]` tag plain.
    #[test]
    fn sync_log_rows_style_the_state_token_and_the_display_id() {
        let store = TmpStore::new("repo");
        seed_list_view_log(
            &store,
            r#"{"detail":"HTTP 422: rejected","class":"validation"}"#,
        );

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run(h.deps_with(Styler::always()), log_args(None));

        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(
            out.contains("\u{1b}[90mpending\u{1b}[39m"),
            "pending should carry MUTATION_PENDING: {out:?}"
        );
        assert!(
            out.contains("\u{1b}[91mfailed\u{1b}[39m"),
            "failed should carry MUTATION_FAILED: {out:?}"
        );
        assert!(
            out.contains("\u{1b}[36mtk-1\u{1b}[39m"),
            "the Display ID should carry the cyan anchor: {out:?}"
        );
        assert!(
            out.contains("  └─ [validation] HTTP 422: rejected\n"),
            "the continuation and class tag stay plain: {out:?}"
        );
    }

    /// Backend text reaches the row's failure continuation inert.
    ///
    /// `failure_detail` is decoded from `failure_json`, which a Backend
    /// Adapter fills from the Remote's own error output. Left raw, an SGR
    /// escape in it would emit colour through no palette entry and under no
    /// `ColorChoice` — colour even under `NO_COLOR`, which is the one thing
    /// ADR-0014 exists to prevent.
    #[test]
    fn sync_log_row_renders_backend_failure_text_inert() {
        let store = TmpStore::new("repo");
        seed_list_view_log(&store, HOSTILE_FAILURE_JSON);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run(h.deps(), log_args(None));

        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(
            out.contains("  └─ HTTP 422: \\x1b[31mred\\x07 title rejected\n"),
            "control bytes should render as visible text: {out:?}"
        );
        assert!(
            !out.contains('\u{1b}'),
            "no escape byte may reach stdout from Backend text: {out:?}"
        );
    }

    #[test]
    fn sync_log_lists_rows_with_failure_continuation() {
        let store = TmpStore::new("repo");
        seed_list_view_log(&store, r#"{"detail":"HTTP 422: rejected"}"#);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), log_args(None));
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(out.contains("1 pending update_ticket tk-1"));
        assert!(out.contains("2 failed set_item_status tk-2"));
        assert!(out.contains("  └─ HTTP 422: rejected"));
    }

    /// The detail view's `Failure:` block is the same Backend text on more
    /// lines, so it sanitises as a body: LF stays layout, control bytes go
    /// inert. See [`sync_log_row_renders_backend_failure_text_inert`].
    #[test]
    fn sync_log_detail_renders_backend_failure_text_inert() {
        let store = TmpStore::new("repo");
        seed_detail_view_log(&store, HOSTILE_FAILURE_JSON);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run(h.deps(), log_args(Some(7)));

        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(
            out.contains("Failure:\n  HTTP 422: \\x1b[31mred\\x07 title rejected"),
            "control bytes should render as visible text: {out:?}"
        );
        assert!(
            !out.contains('\u{1b}'),
            "no escape byte may reach stdout from Backend text: {out:?}"
        );
    }

    #[test]
    fn sync_log_detail_renders_full_view() {
        let store = TmpStore::new("repo");
        seed_detail_view_log(&store, r#"{"detail":"backend said no"}"#);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), log_args(Some(7)));
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(out.contains("Mutation 7  [failed]"));
        assert!(out.contains("Type:       set_item_status"));
        assert!(out.contains("Target:     tk-1 (ticket)"));
        assert!(out.contains("Payload:    {\"status\":\"done\"}"));
        assert!(out.contains("Failure:\n  backend said no"));
    }

    /// Holds [`render_log_detail`] to its contract under forced colour: the
    /// bracketed state and the Display ID styled, field labels plain.
    #[test]
    fn sync_log_detail_styles_the_state_token_and_leaves_labels_plain() {
        let store = TmpStore::new("repo");
        seed_detail_view_log(&store, r#"{"detail":"backend said no"}"#);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run(h.deps_with(Styler::always()), log_args(Some(7)));

        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(
            out.contains("Mutation 7  [\u{1b}[91mfailed\u{1b}[39m]"),
            "the state token styles inside the brackets, not with them: {out:?}"
        );
        assert!(
            out.contains("Target:     \u{1b}[36mtk-1\u{1b}[39m (ticket)"),
            "the Target Display ID should carry the cyan anchor: {out:?}"
        );
        assert!(
            out.contains("Type:       set_item_status\n"),
            "field labels stay plain: {out:?}"
        );
        assert!(
            !out.contains("\u{1b}[1m"),
            "nothing in the detail view is bold: {out:?}"
        );
    }

    #[test]
    fn sync_log_lists_classified_failure_with_class_tag() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"HTTP 401: Bad credentials","class":"auth"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), log_args(None));
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(
            out.contains("  └─ [auth] HTTP 401: Bad credentials"),
            "{out}"
        );
    }

    #[test]
    fn sync_log_detail_renders_class_line_when_classified() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 3,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(
                    r#"{"detail":"HTTP 422: Validation Failed","class":"validation"}"#,
                ),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), log_args(Some(3)));
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(out.contains("Class:      validation"), "{out}");
        assert!(
            out.contains("Failure:\n  HTTP 422: Validation Failed"),
            "{out}"
        );
    }

    #[test]
    fn sync_log_detail_missing_returns_not_found() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), log_args(Some(99)));
        assert_eq!(code, Exit::Failure);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("Mutation 99 not found")
        );
    }

    // ---- report / error rendering ---------------------------------------

    #[test]
    fn render_report_includes_stopped_clause() {
        let mut out = Vec::new();
        render_sync_report(
            &mut out,
            &SyncReport {
                pulled_count: 3,
                applied_count: 2,
                stopped_at_sequence: Some(9),
            },
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Sync complete: 3 pulled, 2 applied, stopped at 9.\n"
        );
    }

    #[test]
    fn render_report_plain_when_no_stop() {
        let mut out = Vec::new();
        render_sync_report(
            &mut out,
            &SyncReport {
                pulled_count: 0,
                applied_count: 0,
                stopped_at_sequence: None,
            },
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Sync complete: 0 pulled, 0 applied.\n"
        );
    }

    #[test]
    fn render_skip_error_frames_a_failed_reopen_as_a_bug() {
        // ADR-0017 keeps these lines verbatim, and the store-side tests assert
        // the variant rather than the rendered bytes. Neither reopen failure is
        // reachable through a Store writer, so this is the only place their
        // wording is pinned.
        for err in [
            MarkSkippedError::ReopenMatchedNothing(4),
            MarkSkippedError::ReopenRefusedByTrigger(4),
        ] {
            let mut out = Vec::new();
            render_skip_error(&mut out, &err);
            let rendered = String::from_utf8(out).unwrap();
            assert!(
                rendered.starts_with("tk sync --skip: mutation 4's reopen "),
                "{rendered}"
            );
            assert!(
                rendered.ends_with("; this is a Ticket bug — please report it\n"),
                "{rendered}"
            );
        }
    }

    #[test]
    fn render_run_sync_error_renders_pull_failure_detail() {
        let mut err_out = Vec::new();
        render_run_sync_error(
            &mut err_out,
            &RunSyncError::Pull(AdapterReadError::Failed("gh: HTTP 502".into())),
        );
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: gh: HTTP 502\n"
        );
    }

    #[test]
    fn render_run_sync_error_renders_remote_change_retry_guidance() {
        let mut err_out = Vec::new();
        render_run_sync_error(
            &mut err_out,
            &RunSyncError::Refresh(RefreshStoreError::RemoteChanged {
                expected: BackendKind::Github,
                actual: Some(BackendKind::Jira),
            }),
        );
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: the configured Remote changed while contacting the Backend; \
             retry 'tk sync'\n"
        );
    }

    #[test]
    fn render_run_sync_error_renders_unknown_backend_cohort_as_an_invariant_failure() {
        let mut err_out = Vec::new();
        render_run_sync_error(
            &mut err_out,
            &RunSyncError::Refresh(RefreshStoreError::BackendCohort(
                BackendCohortError::UnknownBackendKind("gitlab".into()),
            )),
        );
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: Repository Store contains unknown Backend kind 'gitlab'; \
             this is a Repository Store invariant failure\n"
        );
    }

    #[test]
    fn render_run_sync_error_renders_schema_drift() {
        let mut err_out = Vec::new();
        render_run_sync_error(
            &mut err_out,
            &RunSyncError::Load(LoadApplicableError::UnknownMutationType("weird".into())),
        );
        assert!(
            String::from_utf8(err_out)
                .unwrap()
                .contains("unrecognised mutation kind")
        );
    }

    #[test]
    fn render_run_sync_error_names_an_outcome_boundary_mismatch_as_a_bug() {
        // An Adapter that answers a Promotion with a bare acknowledgement has
        // broken its contract: the Mutation stays applicable, so the user needs
        // to know retrying will not clear it.
        let mut err_out = Vec::new();
        render_run_sync_error(
            &mut err_out,
            &RunSyncError::Outcome(PersistMutationOutcomeError::OperationShapeMismatch {
                sequence: 4,
                mutation_type: MutationType::PromoteTicket,
            }),
        );
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: mutation 4 of type promote_ticket cannot carry this receipt; \
             this is a Ticket bug — please report it\n"
        );
    }

    #[test]
    fn render_run_sync_error_names_a_malformed_payload_as_a_bug() {
        let mut err_out = Vec::new();
        render_run_sync_error(
            &mut err_out,
            &RunSyncError::Outcome(PersistMutationOutcomeError::PayloadJson {
                sequence: 4,
                source: serde_json::from_str::<Promotion>("{}").unwrap_err(),
            }),
        );
        let rendered = String::from_utf8(err_out).unwrap();
        assert!(
            rendered.starts_with("tk sync: mutation 4 has malformed payload_json: ")
                && rendered.ends_with("; this is a Ticket bug — please report it\n"),
            "{rendered}"
        );
    }

    #[test]
    fn render_run_sync_error_blocks_retry_after_indeterminate_creation() {
        let mut stderr = Vec::new();

        render_run_sync_error(&mut stderr, &RunSyncError::ApplyingMutation(7));

        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "tk sync: Mutation 7 has an indeterminate Backend creation outcome; use 'tk promote reconcile <id> <backend-key>' if the object exists, 'tk promote retry <id>' only when creating it again is safe, or 'tk promote cancel <id>' to withdraw the Promotion Operation, leaving any object it created untracked\n"
        );
    }

    #[test]
    fn render_run_sync_error_preserves_an_unstored_created_identity() {
        let mut stderr = Vec::new();
        let error = RunSyncError::CreatedIdentityNotStored {
            sequence: 7,
            identity: BackendItemIdentity {
                display_id: "gh-42".into(),
                backend_key: "https://github.com/o/r/issues/42".into(),
            },
            source: PersistMutationOutcomeError::MutationNotFound(7),
        };

        render_run_sync_error(&mut stderr, &error);

        let rendered = String::from_utf8(stderr).unwrap();
        assert!(rendered.contains("gh-42"));
        assert!(rendered.contains("https://github.com/o/r/issues/42"));
        assert!(rendered.contains("remains applying"));
        assert!(rendered.contains("tk promote reconcile"));
    }

    #[test]
    fn render_run_sync_error_labels_post_create_origin_drift_as_corruption() {
        let mut stderr = Vec::new();
        let error = RunSyncError::CreatedIdentityNotStored {
            sequence: 7,
            identity: BackendItemIdentity {
                display_id: "gh-42".into(),
                backend_key: "https://github.com/o/r/issues/42".into(),
            },
            source: PersistMutationOutcomeError::TargetNotLocal {
                sequence: 7,
                item_id: "item-1".into(),
            },
        };

        render_run_sync_error(&mut stderr, &error);

        let rendered = String::from_utf8(stderr).unwrap();
        assert!(rendered.contains("Repository Store corruption or a Ticket bug"));
        assert!(rendered.contains("gh-42"));
        assert!(rendered.contains("remains applying"));
        assert!(rendered.contains("tk promote reconcile"));
    }

    #[test]
    fn render_run_sync_error_preserves_storage_classification() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        let mut stderr = Vec::new();

        render_run_sync_error(
            &mut stderr,
            &RunSyncError::Outcome(PersistMutationOutcomeError::Storage(busy)),
        );

        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "tk sync: Repository Store is busy; retry the command\n"
        );
    }

    #[test]
    fn render_run_sync_error_preserves_direct_technical_errors() {
        let mut stderr = Vec::new();

        render_run_sync_error(
            &mut stderr,
            &RunSyncError::Outcome(PersistMutationOutcomeError::MutationNotFound(8)),
        );

        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "tk sync: mutation 8 not found\n"
        );
    }
}
