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

use crate::cli::{CommandError, Deps, Exit};
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

/// The failure chooses the frame: even with --skip, storage errors use sync.
#[derive(Debug)]
pub struct Error {
    pub command: &'static str,
    pub error: CommandError,
}

impl From<CommandError> for Error {
    fn from(error: CommandError) -> Self {
        Self {
            command: COMMAND,
            error,
        }
    }
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, Error> {
    match args.subcommand {
        Some(Sub::Log(log_args)) => run_log(deps, log_args).map_err(|error| Error {
            command: LOG_COMMAND,
            error,
        }),
        None => run_sync(deps, args.skip),
    }
}

fn run_sync(deps: &mut Deps<'_>, skip: Option<i64>) -> Result<Exit, Error> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock, deps.data_root)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();
    let workflow = store
        .lock_remote_workflow()
        .map_err(CommandError::failure)?;

    // Commit the skip before opening the adapter: a broken or unimplemented
    // Remote must not block an operator from bypassing a failed Mutation, and
    // the committed local outcome is reported before Backend work begins
    // (ADR-0046).
    if let Some(seq) = skip {
        let outcome = store_sync::mark_mutation_skipped(store.conn_mut(), &workflow, seq, &now)
            .map_err(|err| skip_error(&err))?;
        render_skip_outcome(deps.stdout, seq, &outcome);
    }

    let adapter_opt =
        factory::open_configured(store.conn(), deps.runner, deps.cwd).map_err(|err| match err {
            FactoryOpenError::NotImplemented => CommandError::failure(
                "the configured Remote's adapter is not implemented in this build",
            ),
            FactoryOpenError::Storage(err) => resolver::storage_error(&err),
        })?;
    let Some(mut adapter) = adapter_opt else {
        return Err(CommandError::failure(
            "no Remote configured; run 'tk remote set <kind>' first",
        )
        .into());
    };

    let report = sync::run_sync(store.conn_mut(), &mut *adapter, &workflow, &now)
        .map_err(|err| run_sync_error(&err))?;
    render_sync_report(deps.stdout, &report);
    Ok(if report.stopped_at_sequence.is_some() {
        Exit::Failure
    } else {
        Exit::Ok
    })
}

fn run_log(deps: &mut Deps<'_>, args: LogArgs) -> Result<Exit, CommandError> {
    let styler = deps.styler.for_stdout();
    let store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock, deps.data_root)
        .map_err(|err| resolver::open_error(&err))?;

    if let Some(seq) = args.id {
        let detail = store_sync::show_mutation_log(store.conn(), seq).map_err(|err| match err {
            LogError::MutationNotFound(seq) => {
                CommandError::failure(format_args!("Mutation {seq} not found"))
            }
            err => log_error(&err),
        })?;
        render_log_detail(deps.stdout, &detail, styler);
        return Ok(Exit::Ok);
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

    let rows =
        store_sync::list_mutation_log(store.conn(), filter).map_err(|err| log_error(&err))?;
    if rows.is_empty() {
        let message = match filter {
            LogListFilter::Default => {
                if store_sync::mutation_log_is_empty(store.conn()).map_err(|err| log_error(&err))? {
                    "No Mutations recorded."
                } else {
                    "All Mutations applied."
                }
            }
            LogListFilter::Pending => "No pending Mutations.",
            LogListFilter::Failed => "No failed Mutations.",
            LogListFilter::Skipped => "No skipped Mutations.",
            LogListFilter::Cancelled => "No cancelled Mutations.",
            LogListFilter::Abandoned => "No abandoned Mutations.",
        };
        let _ = writeln!(deps.stdout, "{message}");
        return Ok(Exit::Ok);
    }
    for row in &rows {
        render_log_row(deps.stdout, row, styler);
    }
    Ok(Exit::Ok)
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

fn skip_error(err: &MarkSkippedError) -> Error {
    let error = match err {
        MarkSkippedError::MutationNotFailed(seq) => CommandError::failure(format_args!(
            "Mutation {seq} is not in the failed state; --skip only bypasses failed Mutations"
        )),
        MarkSkippedError::MutationNotFound(seq) => {
            CommandError::failure(format_args!("Mutation {seq} not found"))
        }
        MarkSkippedError::CannotSkipPromotion(seq) => CommandError::failure(format_args!(
            "Mutation {seq} is a Promotion; skipping it would leave every Mutation queued behind it with no backend identity to apply against. Use 'tk promote cancel <id>' to withdraw the whole Promotion Operation."
        )),
        MarkSkippedError::Transition(_)
        | MarkSkippedError::ReopenMatchedNothing(_)
        | MarkSkippedError::ReopenRefusedByTrigger(_) => CommandError::failure(format_args!(
            "{err}; this is a Ticket bug — please report it"
        )),
        MarkSkippedError::Storage(err) => return resolver::storage_error(err).into(),
    };
    Error {
        command: "sync --skip",
        error,
    }
}

fn log_error(err: &LogError) -> CommandError {
    match err {
        LogError::Storage(err) => resolver::storage_error(err),
        LogError::MutationNotFound(_) | LogError::FailureJson(_) => {
            CommandError::failure(format_args!("failed to read Repository Store\n{err}"))
        }
    }
}

fn run_sync_error(err: &RunSyncError) -> CommandError {
    match err.category() {
        RunSyncErrorCategory::BackendDetail(detail) => CommandError::failure(detail),
        RunSyncErrorCategory::MutationSchemaDrift(_) => CommandError::failure(
            "Mutation Log row has an unrecognised mutation kind; this is a Ticket bug — please report it",
        ),
        RunSyncErrorCategory::TicketBug(error) => CommandError::failure(format_args!(
            "{error}; this is a Ticket bug — please report it"
        )),
        RunSyncErrorCategory::Storage(error) => resolver::storage_error(error),
        RunSyncErrorCategory::CreatedIdentityNotStored {
            error,
            sequence,
            cause,
        } => {
            let guidance = if matches!(cause, CreatedIdentityNotStoredCause::TargetNotLocal) {
                "\nThis is Repository Store corruption or a Ticket bug — please report it"
            } else {
                ""
            };
            CommandError::failure(format_args!(
                "{error}{guidance}\nMutation {sequence} remains applying; use 'tk promote reconcile <id> <backend-key>' after confirming the created Backend object"
            ))
        }

        RunSyncErrorCategory::Direct(error) => CommandError::failure(error),
        RunSyncErrorCategory::IndeterminateCreation(sequence) => {
            CommandError::failure(format_args!(
                "Mutation {sequence} has an indeterminate Backend creation outcome; use 'tk promote reconcile <id> <backend-key>' if the object exists, 'tk promote retry <id>' only when creating it again is safe, or 'tk promote cancel <id>' to withdraw the Promotion Operation, leaving any object it created untracked"
            ))
        }
        RunSyncErrorCategory::RemoteChanged => CommandError::failure(
            "the configured Remote changed while contacting the Backend; retry 'tk sync'",
        ),
        RunSyncErrorCategory::RepositoryInvariant(error) => CommandError::failure(format_args!(
            "{error}; this is a Repository Store invariant failure"
        )),
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
        let _ = write!(stdout, "  └─ ");
        // The class is shown only when the adapter actually classified the
        // failure; an `unknown` row carries no signal, so it renders bare.
        if let Some(class) = row.failure_class.filter(|c| *c != FailureClass::Unknown) {
            let _ = write!(stdout, "[{class}] ");
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
    // `payload_json` is `serde_json` output, whose escaping is narrower than
    // it looks: the `ESCAPE` table covers `0x00..=0x1F` and leaves DEL and
    // the whole C1 block alone. A Ticket title holding U+009B — the 8-bit CSI
    // a terminal reads as `ESC [` — is therefore stored raw, so this line
    // needs the same output-boundary treatment as the failure text below.
    let _ = write!(stdout, "Payload:    ");
    let _ = sanitize::write_sanitized_line(stdout, detail.payload_json.as_bytes());
    let _ = stdout.write_all(b"\n");
    if let Some(d) = &detail.failure_detail {
        if let Some(class) = detail.failure_class.filter(|c| *c != FailureClass::Unknown) {
            let _ = writeln!(stdout, "Class:      {class}");
        }
        // Backend-controlled text, as in [`render_log_row`], but sanitised as
        // a body: this block is the one place a Remote's multi-line error is
        // shown whole, so LF stays layout and only control bytes go inert.
        // The trailing guard matches `show`'s body sections, so a detail that
        // already ends in LF does not print a blank line after it.
        let _ = write!(stdout, "Failure:\n  ");
        let _ = sanitize::write_sanitized_body(stdout, d.as_bytes());
        if !d.ends_with('\n') {
            let _ = stdout.write_all(b"\n");
        }
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
                payload_json: r#"{"title":"A","body":""}"#,
                state: "pending",
                ..FixtureMutation::new(MutationType::UpdateTicket, "t1")
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(failure_json),
                ..FixtureMutation::new(MutationType::SetItemStatus, "t2")
            },
        )
        .unwrap();
    }

    /// Seed the single failed Mutation the detail-view tests inspect:
    /// sequence 7 on tk-1. The caller supplies both untrusted fields the view
    /// renders, so a test can make either one hostile.
    fn seed_detail_view_log(store: &TmpStore, payload_json: &str, failure_json: &str) {
        let conn = seed_store(store);
        backend_ticket(&conn, "t1", "tk-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 7,
                payload_json,
                state: "failed",
                failure_json: Some(failure_json),
                ..FixtureMutation::new(MutationType::SetItemStatus, "t1")
            },
        )
        .unwrap();
    }

    const CLEAN_PAYLOAD_JSON: &str = r#"{"status":"done"}"#;
    const CLEAN_FAILURE_JSON: &str = r#"{"detail":"backend said no"}"#;

    /// A payload whose title carries U+009B, written as a Rust escape so no
    /// control byte sits in this source. `serde_json` stores it raw — see
    /// [`render_log_detail`] for why.
    const HOSTILE_PAYLOAD_JSON: &str = "{\"title\":\"boom\u{9b}31m\",\"body\":\"\"}";

    /// A Failure whose detail carries an SGR escape and a bell, as a Remote
    /// error message relaying user or collaborator content can. `\u001b` is
    /// JSON's spelling of ESC, so the stored `failure_json` decodes to a
    /// detail holding the raw control bytes.
    const HOSTILE_FAILURE_JSON: &str =
        r#"{"detail":"HTTP 422: \u001b[31mred\u0007 title rejected"}"#;

    fn run(deps: Deps<'_>, args: &[&str]) -> Exit {
        let argv = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        crate::cli::run_argv(deps, &argv).unwrap()
    }

    #[test]
    fn sync_no_remote_returns_1_with_diagnostic() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run(h.deps(), &["sync"]);
        assert_eq!(code, Exit::Failure);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("no Remote configured")
        );
    }

    #[test]
    fn sync_github_with_no_adopted_items_is_a_noop() {
        // With no Adopted items, sync must make no Backend calls.
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
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store); // only git discovery; no gh call expected
        let code = run(h.deps(), &["sync"]);
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
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "pending",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::new(MutationType::PromoteTicket, "t1")
            },
        )
        .unwrap();
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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

        let code = run(h.deps(), &["sync"]);

        assert_eq!(code, Exit::Failure);
        assert!(h.stderr.is_empty());
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
                payload_json: r#"{"title":"New Title","body":""}"#,
                state: "pending",
                ..FixtureMutation::new(MutationType::UpdateTicket, "t1")
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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
        let code = run(h.deps(), &["sync"]);
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
                payload_json: r#"{"title":"A","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::new(MutationType::UpdateTicket, "t1")
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        // No Remote configured: sync still exits 1 on no-remote, but the skip
        // committed first.
        let code = run(h.deps(), &["sync", "--skip", "1"]);
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
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::new(MutationType::SetItemStatus, "t1")
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        // Report the committed reopen even when no Remote is configured.
        let code = run(h.deps(), &["sync", "--skip", "1"]);
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
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::new(MutationType::SetItemStatus, "t1")
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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
        let code = run(h.deps(), &["sync", "--skip", "1"]);
        assert_eq!(code, Exit::Ok);
        assert_eq!(
            String::from_utf8(h.stdout).unwrap(),
            "Skipped Mutation 1; restored gh-1 to open.\nSync complete: 1 pulled, 0 applied.\n"
        );

        // Pull must refresh title and body as well as Lifecycle after the reopen.
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
                payload_json: r#"{"title":"A","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::new(MutationType::UpdateTicket, "t1")
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
        crate::store::testing::expect_pointer(&holder_runner);
        let clock = FakeClock::new(1_778_284_800_000);
        let first =
            resolver::open_for_command(&holder_runner, &cwd_path, &clock, Some(&store.data_root))
                .unwrap();
        let first_guard = first.lock_remote_workflow().unwrap();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let exit = run(h.deps(), &["sync", "--skip", "1"]);
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
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"boom"}"#),
                ..FixtureMutation::new(MutationType::PromoteTicket, "t1")
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run(h.deps(), &["sync", "--skip", "1"]);
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
    fn sync_skip_storage_failure_keeps_the_sync_frame() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        conn.execute_batch("DROP TABLE mutations").unwrap();
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run(h.deps(), &["sync", "--skip", "1"]);

        assert_eq!(code, Exit::Failure);
        assert!(h.stdout.is_empty());
        assert_eq!(
            h.err(),
            "tk sync: failed to read Repository Store\nno such table: mutations\n"
        );
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
                payload_json: r#"{"title":"A","body":""}"#,
                state: "pending",
                ..FixtureMutation::new(MutationType::UpdateTicket, "t1")
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run(h.deps(), &["sync", "--skip", "1"]);
        assert_eq!(code, Exit::Failure);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("is not in the failed state")
        );
    }

    #[test]
    fn sync_log_empty_prints_default_message() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run(h.deps(), &["sync", "log"]);
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
                state: "applied",
                ..FixtureMutation::new(MutationType::UpdateTicket, "t1")
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run(h.deps(), &["sync", "log"]);

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
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run(h.deps_with(Styler::always()), &["sync", "log"]);

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
    /// Left raw, an SGR escape in it would emit colour through no palette
    /// entry and under no `ColorChoice` — colour even under `NO_COLOR`,
    /// which is the one thing ADR-0014 exists to prevent. See
    /// [`render_log_row`] on where the text comes from.
    #[test]
    fn sync_log_row_renders_backend_failure_text_inert() {
        let store = TmpStore::new("repo");
        seed_list_view_log(&store, HOSTILE_FAILURE_JSON);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run(h.deps(), &["sync", "log"]);

        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(
            out.contains("  └─ HTTP 422: \\x1b[31mred\\x07 title rejected\n"),
            "Backend control bytes must reach stdout as visible text, never \
             as SGR the palette never chose (ADR-0014): {out:?}"
        );
    }

    #[test]
    fn sync_log_lists_rows_with_failure_continuation() {
        let store = TmpStore::new("repo");
        seed_list_view_log(&store, r#"{"detail":"HTTP 422: rejected"}"#);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run(h.deps(), &["sync", "log"]);
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
        seed_detail_view_log(&store, CLEAN_PAYLOAD_JSON, HOSTILE_FAILURE_JSON);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run(h.deps(), &["sync", "log", "7"]);

        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(
            out.contains("Failure:\n  HTTP 422: \\x1b[31mred\\x07 title rejected"),
            "Backend control bytes must reach stdout as visible text: {out:?}"
        );
    }

    /// Stored payload text reaches the `Payload:` line inert too. A C1
    /// control survives `serde_json` into the Repository Store, so this line
    /// needs the sanitiser as much as the failure text does — see
    /// [`render_log_detail`].
    #[test]
    fn sync_log_detail_renders_payload_text_inert() {
        let store = TmpStore::new("repo");
        seed_detail_view_log(&store, HOSTILE_PAYLOAD_JSON, CLEAN_FAILURE_JSON);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run(h.deps(), &["sync", "log", "7"]);

        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(
            out.contains(r#"Payload:    {"title":"boom\x9b31m","body":""}"#),
            "the C1 control should render as visible text: {out:?}"
        );
    }

    #[test]
    fn sync_log_detail_renders_full_view() {
        let store = TmpStore::new("repo");
        seed_detail_view_log(&store, CLEAN_PAYLOAD_JSON, CLEAN_FAILURE_JSON);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run(h.deps(), &["sync", "log", "7"]);
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
        seed_detail_view_log(&store, CLEAN_PAYLOAD_JSON, CLEAN_FAILURE_JSON);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run(h.deps_with(Styler::always()), &["sync", "log", "7"]);

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
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"HTTP 401: Bad credentials","class":"auth"}"#),
                ..FixtureMutation::new(MutationType::SetItemStatus, "t1")
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run(h.deps(), &["sync", "log"]);
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
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(
                    r#"{"detail":"HTTP 422: Validation Failed","class":"validation"}"#,
                ),
                ..FixtureMutation::new(MutationType::SetItemStatus, "t1")
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run(h.deps(), &["sync", "log", "3"]);
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
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run(h.deps(), &["sync", "log", "99"]);
        assert_eq!(code, Exit::Failure);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("Mutation 99 not found")
        );
    }

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
    fn skip_error_frames_a_failed_reopen_as_a_bug() {
        // Store writers cannot produce these invariant failures. Construct them
        // directly to check that the diagnostic asks the user to report a bug.
        for err in [
            MarkSkippedError::ReopenMatchedNothing(4),
            MarkSkippedError::ReopenRefusedByTrigger(4),
        ] {
            let mut out = Vec::new();
            let error = skip_error(&err);
            error.error.render(&mut out, error.command);
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
    fn run_sync_error_renders_pull_failure_detail() {
        let mut err_out = Vec::new();
        run_sync_error(&RunSyncError::Pull(AdapterReadError::Failed(
            "gh: HTTP 502".into(),
        )))
        .render(&mut err_out, COMMAND);
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: gh: HTTP 502\n"
        );
    }

    #[test]
    fn run_sync_error_renders_remote_change_retry_guidance() {
        let mut err_out = Vec::new();
        run_sync_error(&RunSyncError::Refresh(RefreshStoreError::RemoteChanged {
            expected: BackendKind::Github,
            actual: Some(BackendKind::Jira),
        }))
        .render(&mut err_out, COMMAND);
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: the configured Remote changed while contacting the Backend; \
             retry 'tk sync'\n"
        );
    }

    #[test]
    fn run_sync_error_renders_unknown_backend_cohort_as_an_invariant_failure() {
        let mut err_out = Vec::new();
        run_sync_error(&RunSyncError::Refresh(RefreshStoreError::BackendCohort(
            BackendCohortError::UnknownBackendKind("gitlab".into()),
        )))
        .render(&mut err_out, COMMAND);
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: Repository Store contains unknown Backend kind 'gitlab'; \
             this is a Repository Store invariant failure\n"
        );
    }

    #[test]
    fn run_sync_error_renders_schema_drift() {
        let mut err_out = Vec::new();
        run_sync_error(&RunSyncError::Load(
            LoadApplicableError::UnknownMutationType("weird".into()),
        ))
        .render(&mut err_out, COMMAND);
        assert!(
            String::from_utf8(err_out)
                .unwrap()
                .contains("unrecognised mutation kind")
        );
    }

    #[test]
    fn run_sync_error_names_an_outcome_boundary_mismatch_as_a_bug() {
        // An Adapter that answers a Promotion with a bare acknowledgement has
        // broken its contract: the Mutation stays applicable, so the user needs
        // to know retrying will not clear it.
        let mut err_out = Vec::new();
        run_sync_error(&RunSyncError::Outcome(
            PersistMutationOutcomeError::OperationShapeMismatch {
                sequence: 4,
                mutation_type: MutationType::PromoteTicket,
            },
        ))
        .render(&mut err_out, COMMAND);
        assert_eq!(
            String::from_utf8(err_out).unwrap(),
            "tk sync: mutation 4 of type promote_ticket cannot carry this receipt; \
             this is a Ticket bug — please report it\n"
        );
    }

    #[test]
    fn run_sync_error_names_a_malformed_payload_as_a_bug() {
        let mut err_out = Vec::new();
        run_sync_error(&RunSyncError::Outcome(
            PersistMutationOutcomeError::PayloadJson {
                sequence: 4,
                source: serde_json::from_str::<Promotion>("{}").unwrap_err(),
            },
        ))
        .render(&mut err_out, COMMAND);
        let rendered = String::from_utf8(err_out).unwrap();
        assert!(
            rendered.starts_with("tk sync: mutation 4 has malformed payload_json: ")
                && rendered.ends_with("; this is a Ticket bug — please report it\n"),
            "{rendered}"
        );
    }

    #[test]
    fn run_sync_error_blocks_retry_after_indeterminate_creation() {
        let mut stderr = Vec::new();

        run_sync_error(&RunSyncError::ApplyingMutation(7)).render(&mut stderr, COMMAND);

        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "tk sync: Mutation 7 has an indeterminate Backend creation outcome; use 'tk promote reconcile <id> <backend-key>' if the object exists, 'tk promote retry <id>' only when creating it again is safe, or 'tk promote cancel <id>' to withdraw the Promotion Operation, leaving any object it created untracked\n"
        );
    }

    #[test]
    fn run_sync_error_preserves_an_unstored_created_identity() {
        let mut stderr = Vec::new();
        let error = RunSyncError::CreatedIdentityNotStored {
            sequence: 7,
            identity: BackendItemIdentity {
                display_id: "gh-42".into(),
                backend_key: "https://github.com/o/r/issues/42".into(),
            },
            source: PersistMutationOutcomeError::MutationNotFound(7),
        };

        run_sync_error(&error).render(&mut stderr, COMMAND);

        let rendered = String::from_utf8(stderr).unwrap();
        assert!(rendered.contains("gh-42"));
        assert!(rendered.contains("https://github.com/o/r/issues/42"));
        assert!(rendered.contains("remains applying"));
        assert!(rendered.contains("tk promote reconcile"));
    }

    #[test]
    fn run_sync_error_labels_post_create_origin_drift_as_corruption() {
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

        run_sync_error(&error).render(&mut stderr, COMMAND);

        let rendered = String::from_utf8(stderr).unwrap();
        assert!(rendered.contains("Repository Store corruption or a Ticket bug"));
        assert!(rendered.contains("gh-42"));
        assert!(rendered.contains("remains applying"));
        assert!(rendered.contains("tk promote reconcile"));
    }

    #[test]
    fn run_sync_error_preserves_storage_classification() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        let mut stderr = Vec::new();

        run_sync_error(&RunSyncError::Outcome(
            PersistMutationOutcomeError::Storage(busy),
        ))
        .render(&mut stderr, COMMAND);

        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "tk sync: Repository Store is busy; retry the command\n"
        );
    }

    #[test]
    fn run_sync_error_preserves_direct_technical_errors() {
        let mut stderr = Vec::new();

        run_sync_error(&RunSyncError::Outcome(
            PersistMutationOutcomeError::MutationNotFound(8),
        ))
        .render(&mut stderr, COMMAND);

        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "tk sync: mutation 8 not found\n"
        );
    }
}
