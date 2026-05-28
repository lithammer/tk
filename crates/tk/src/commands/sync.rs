//! `tk sync` and `tk sync log` — Mutation outbox replay and inspection.
//!
//! `tk sync` opens the configured Backend Adapter via
//! [`crate::remote::factory::open_configured`] and drives the backend-blind
//! engine ([`crate::sync::run_sync`]). Until real adapters land (tk-40) a
//! configured Remote returns `NotImplemented`, so the engine is reached only
//! through the engine's own tests; this command's report rendering is unit-
//! tested directly against a synthetic [`SyncReport`].
//!
//! `tk sync --skip <id>` curates a failed Mutation. The skip commits BEFORE the
//! adapter is opened so a broken / unimplemented Remote cannot block an
//! operator from abandoning a Mutation the backend already rejected.
//!
//! `tk sync log` reads the Mutation Log through [`crate::store::sync`]; it
//! needs no adapter and is exercised end-to-end here.

use std::io::Write;

use clap::{Args as ClapArgs, Subcommand};

use crate::cli::Deps;
use crate::commands::resolver;
use crate::remote::adapter::PullError;
use crate::remote::factory::{self, OpenError as FactoryOpenError};
use crate::store::sync::{
    self as store_sync, ApplyMutationOutcomeError, LoadApplicableError, LogDetailRow, LogError,
    LogListFilter, LogListRow, MarkSkippedError, MergeError,
};
use crate::sync::{self, RunSyncError, SyncReport};

const COMMAND: &str = "sync";
const LOG_COMMAND: &str = "sync log";

/// Flags for `tk sync`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub subcommand: Option<Sub>,
    /// Mark one failed Mutation skipped before running sync.
    #[arg(long, value_name = "MUTATION-ID")]
    pub skip: Option<i64>,
}

#[derive(Debug, Subcommand)]
pub enum Sub {
    /// Inspect pending, failed, and skipped Mutations.
    Log(LogArgs),
}

/// Flags for `tk sync log`. The three state flags are a filter; if more than
/// one is given, precedence is pending → failed → skipped.
#[derive(Debug, ClapArgs)]
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
    /// Show one Mutation in detail (Mutation Sequence).
    pub id: Option<i64>,
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> u8 {
    match args.subcommand {
        Some(Sub::Log(log_args)) => run_log(deps, log_args),
        None => run_sync(deps, args.skip),
    }
}

fn run_sync(deps: Deps<'_>, skip: Option<i64>) -> u8 {
    let Deps {
        stdout,
        stderr,
        runner,
        clock,
        cwd,
        rng,
        ..
    } = deps;

    let mut store = match resolver::open_for_command(runner, cwd) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, COMMAND, &err);
            return 1;
        }
    };
    let now = clock.now_iso();

    // Commit the skip before opening the adapter: a broken or unimplemented
    // Remote must not block an operator from abandoning a failed Mutation.
    if let Some(seq) = skip {
        if let Err(err) = store_sync::mark_mutation_skipped(store.conn_mut(), seq, &now) {
            render_skip_error(stderr, &err);
            return 1;
        }
    }

    let adapter_opt = match factory::open_configured(store.conn()) {
        Ok(a) => a,
        Err(FactoryOpenError::NotImplemented) => {
            let _ = writeln!(
                stderr,
                "tk sync: the configured Remote's adapter is not implemented in this build"
            );
            return 1;
        }
        Err(FactoryOpenError::Storage(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return 1;
        }
    };
    let Some(mut adapter) = adapter_opt else {
        let _ = writeln!(
            stderr,
            "tk sync: no Remote configured; run 'tk remote set <kind>' first"
        );
        return 1;
    };

    let report = match sync::run_sync(store.conn_mut(), &mut *adapter, &now, rng) {
        Ok(report) => report,
        Err(err) => {
            render_run_sync_error(stderr, &err);
            return 1;
        }
    };
    render_sync_report(stdout, &report, skip);
    0
}

fn run_log(deps: Deps<'_>, args: LogArgs) -> u8 {
    let Deps {
        stdout,
        stderr,
        runner,
        cwd,
        ..
    } = deps;

    let store = match resolver::open_for_command(runner, cwd) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, LOG_COMMAND, &err);
            return 1;
        }
    };

    if let Some(seq) = args.id {
        return match store_sync::show_mutation_log(store.conn(), seq) {
            Ok(detail) => {
                render_log_detail(stdout, &detail);
                0
            }
            Err(LogError::MutationNotFound(seq)) => {
                let _ = writeln!(stderr, "tk sync log: Mutation {seq} not found");
                1
            }
            Err(LogError::Storage(err)) => {
                resolver::render_storage_error(stderr, LOG_COMMAND, &err);
                1
            }
            Err(LogError::FailureJson(err)) => {
                let _ = writeln!(
                    stderr,
                    "tk sync log: failed to read Repository Store\n{err}"
                );
                1
            }
        };
    }

    let filter = if args.pending {
        LogListFilter::Pending
    } else if args.failed {
        LogListFilter::Failed
    } else if args.skipped {
        LogListFilter::Skipped
    } else {
        LogListFilter::Default
    };

    let rows = match store_sync::list_mutation_log(store.conn(), filter) {
        Ok(rows) => rows,
        Err(LogError::Storage(err)) => {
            resolver::render_storage_error(stderr, LOG_COMMAND, &err);
            return 1;
        }
        Err(err) => {
            let _ = writeln!(
                stderr,
                "tk sync log: failed to read Repository Store\n{err}"
            );
            return 1;
        }
    };

    if rows.is_empty() {
        let _ = writeln!(stdout, "{}", empty_log_message(filter));
        return 0;
    }
    for row in &rows {
        render_log_row(stdout, row);
    }
    0
}

fn empty_log_message(filter: LogListFilter) -> &'static str {
    match filter {
        LogListFilter::Default => "No Mutations recorded.",
        LogListFilter::Pending => "No pending Mutations.",
        LogListFilter::Failed => "No failed Mutations.",
        LogListFilter::Skipped => "No skipped Mutations.",
    }
}

/// Render the one-line sync summary: `Sync complete: <p> pulled, <a> applied`
/// with optional `, skipped <id>` and `, stopped at <seq>` clauses.
fn render_sync_report<W: Write + ?Sized>(stdout: &mut W, report: &SyncReport, skip: Option<i64>) {
    let _ = write!(
        stdout,
        "Sync complete: {} pulled, {} applied",
        report.pulled_count, report.applied_count
    );
    if let Some(seq) = skip {
        let _ = write!(stdout, ", skipped {seq}");
    }
    if let Some(seq) = report.stopped_at_sequence {
        let _ = write!(stdout, ", stopped at {seq}");
    }
    let _ = writeln!(stdout, ".");
}

fn render_skip_error<W: Write + ?Sized>(stderr: &mut W, err: &MarkSkippedError) {
    match err {
        MarkSkippedError::MutationNotFailed(seq) => {
            let _ = writeln!(
                stderr,
                "tk sync --skip: Mutation {seq} is not in the failed state; --skip only abandons failed Mutations"
            );
        }
        MarkSkippedError::MutationNotFound(seq) => {
            let _ = writeln!(stderr, "tk sync --skip: Mutation {seq} not found");
        }
        MarkSkippedError::Storage(err) => resolver::render_storage_error(stderr, COMMAND, err),
    }
}

/// Dispatch a [`RunSyncError`] to its verbatim stderr line. Storage-class and
/// environment failures fall through to the generic frame.
fn render_run_sync_error<W: Write + ?Sized>(stderr: &mut W, err: &RunSyncError) {
    match err {
        RunSyncError::Pull(PullError::Failed(detail)) => {
            let _ = writeln!(stderr, "tk sync: {detail}");
        }
        RunSyncError::Merge(MergeError::DisplayIdCollision(id)) => {
            let _ = writeln!(
                stderr,
                "tk sync: Display ID '{id}' already claimed by an existing Item"
            );
        }
        RunSyncError::Load(
            LoadApplicableError::UnknownMutationType(_)
            | LoadApplicableError::PayloadVariantMissing(_),
        ) => {
            let _ = writeln!(
                stderr,
                "tk sync: Mutation Log row has an unrecognised mutation kind; this is a Ticket bug — please report it"
            );
        }
        RunSyncError::Merge(MergeError::Storage(e))
        | RunSyncError::Load(LoadApplicableError::Storage(e))
        | RunSyncError::Outcome(ApplyMutationOutcomeError::Storage(e)) => {
            resolver::render_storage_error(stderr, COMMAND, e);
        }
        other => {
            let _ = writeln!(stderr, "tk sync: {other}");
        }
    }
}

fn render_log_row<W: Write + ?Sized>(stdout: &mut W, row: &LogListRow) {
    let _ = writeln!(
        stdout,
        "{} {} {} {} {}",
        row.sequence, row.state, row.mutation_type, row.target_display_id, row.created_at
    );
    if let Some(detail) = &row.failure_detail {
        let _ = writeln!(stdout, "  └─ {detail}");
    }
}

fn render_log_detail<W: Write + ?Sized>(stdout: &mut W, detail: &LogDetailRow) {
    let _ = writeln!(stdout, "Mutation {}  [{}]", detail.sequence, detail.state);
    let _ = writeln!(stdout, "Type:       {}", detail.mutation_type);
    let _ = writeln!(
        stdout,
        "Target:     {} ({})",
        detail.target_display_id, detail.item_class
    );
    let _ = writeln!(stdout, "Created:    {}", detail.created_at);
    let _ = writeln!(stdout, "Updated:    {}", detail.state_changed_at);
    let _ = writeln!(stdout, "Payload:    {}", detail.payload_json);
    if let Some(d) = &detail.failure_detail {
        let _ = writeln!(stdout, "Failure:\n  {d}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, TmpStore, insert_fixture_item,
        insert_fixture_mutation, insert_fixture_remote,
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
                rng: StdRng::seed_from_u64(0),
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
    }

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
        assert_eq!(code, 1);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("no Remote configured")
        );
    }

    #[test]
    fn sync_github_remote_returns_adapter_not_implemented() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "github",
                config_json: r#"{"repo":"o/r"}"#,
                ..FixtureRemote::default()
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
                skip: None,
            },
        );
        assert_eq!(code, 1);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("adapter is not implemented in this build")
        );
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
        assert_eq!(code, 1);

        let conn = Connection::open(store.db_path()).unwrap();
        let state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "skipped", "skip committed before the no-remote exit");
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
        assert_eq!(code, 1);
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

        let code = run(
            h.deps(),
            Args {
                subcommand: Some(Sub::Log(LogArgs {
                    pending: false,
                    failed: false,
                    skipped: false,
                    id: None,
                })),
                skip: None,
            },
        );
        assert_eq!(code, 0);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("No Mutations recorded.")
        );
    }

    #[test]
    fn sync_log_lists_rows_with_failure_continuation() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
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
                failure_json: Some(r#"{"detail":"HTTP 422: rejected"}"#),
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
                subcommand: Some(Sub::Log(LogArgs {
                    pending: false,
                    failed: false,
                    skipped: false,
                    id: None,
                })),
                skip: None,
            },
        );
        assert_eq!(code, 0);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(out.contains("1 pending update_ticket tk-1"));
        assert!(out.contains("2 failed set_item_status tk-2"));
        assert!(out.contains("  └─ HTTP 422: rejected"));
    }

    #[test]
    fn sync_log_detail_renders_full_view() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 7,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"backend said no"}"#),
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
                subcommand: Some(Sub::Log(LogArgs {
                    pending: false,
                    failed: false,
                    skipped: false,
                    id: Some(7),
                })),
                skip: None,
            },
        );
        assert_eq!(code, 0);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(out.contains("Mutation 7  [failed]"));
        assert!(out.contains("Type:       set_item_status"));
        assert!(out.contains("Target:     tk-1 (ticket)"));
        assert!(out.contains("Payload:    {\"status\":\"done\"}"));
        assert!(out.contains("Failure:\n  backend said no"));
    }

    #[test]
    fn sync_log_detail_missing_returns_not_found() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                subcommand: Some(Sub::Log(LogArgs {
                    pending: false,
                    failed: false,
                    skipped: false,
                    id: Some(99),
                })),
                skip: None,
            },
        );
        assert_eq!(code, 1);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("Mutation 99 not found")
        );
    }

    // ---- report / error rendering (engine unreachable via factory) ------

    #[test]
    fn render_report_includes_skipped_and_stopped_clauses() {
        let mut out = Vec::new();
        render_sync_report(
            &mut out,
            &SyncReport {
                pulled_count: 3,
                applied_count: 2,
                stopped_at_sequence: Some(9),
            },
            Some(4),
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Sync complete: 3 pulled, 2 applied, skipped 4, stopped at 9.\n"
        );
    }

    #[test]
    fn render_report_plain_when_no_skip_or_stop() {
        let mut out = Vec::new();
        render_sync_report(
            &mut out,
            &SyncReport {
                pulled_count: 0,
                applied_count: 0,
                stopped_at_sequence: None,
            },
            None,
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Sync complete: 0 pulled, 0 applied.\n"
        );
    }

    #[test]
    fn render_run_sync_error_renders_pull_failure_detail() {
        let mut err_out = Vec::new();
        render_run_sync_error(
            &mut err_out,
            &RunSyncError::Pull(PullError::Failed("gh: HTTP 502".into())),
        );
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: gh: HTTP 502\n"
        );
    }

    #[test]
    fn render_run_sync_error_renders_display_id_collision() {
        let mut err_out = Vec::new();
        render_run_sync_error(
            &mut err_out,
            &RunSyncError::Merge(MergeError::DisplayIdCollision("gh-1".into())),
        );
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: Display ID 'gh-1' already claimed by an existing Item\n"
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
}
