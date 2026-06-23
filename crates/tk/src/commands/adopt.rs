//! `tk adopt` — bring one existing Backend issue into the Repository Store as a
//! Backend Ticket (ADR-0034: Adopt is the sole Backend → tk intake path in v1).
//!
//! `tk adopt <key>` eagerly fetches the single issue named by `<key>` through
//! the configured Backend Adapter and inserts it as an `accepted` Backend
//! Ticket (Display ID `gh-<n>` for GitHub). It is the inverse intake direction
//! to Promotion and, like Backend Pull's insert path, is a current-state
//! insert: it records **no** Mutation.
//!
//! `<key>` is the backend-native identifier (a bare issue number for GitHub),
//! passed to the adapter verbatim — tk does not normalise URLs or `#`-prefixes,
//! because the already-adopted pre-check is an exact `backend_key` match and
//! the command itself is backend-agnostic (the adapter interprets the key).
//!
//! Born on the ADR-0032 diagnostics seam: [`run`] returns
//! `Result<Exit, CommandError>` and the dispatch seam frames `tk adopt:
//! <body>`. The shared failure bodies match `tk sync` byte-for-byte.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::remote::adapter::PullError;
use crate::remote::factory::{self, OpenError as FactoryOpenError};
use crate::store::sync::{self as store_sync, MergeError};

/// Flags for `tk adopt`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Backend-native key of the issue to adopt (a bare issue number for
    /// GitHub). Passed to the Backend Adapter verbatim — tk does not normalise
    /// URLs or `#`-prefixes (the already-adopted pre-check is an exact match).
    #[arg(value_name = "KEY")]
    pub key: String,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();

    // The configured Remote names the Backend whose key namespace `<key>` and
    // the pre-check live in. No Remote → the same guidance `tk sync` gives.
    let Some(remote) =
        store_sync::get_remote(store.conn()).map_err(|e| resolver::storage_error(&e))?
    else {
        return Err(no_remote());
    };

    // Idempotent pre-check (mirrors `tk remote set`): if this exact backend key
    // is already adopted, report it and exit 0 WITHOUT a backend call. Adopt is
    // intake, not refresh — re-pulling an Adopted issue is `tk sync`'s job.
    if let Some(existing) =
        store_sync::find_backend_item(store.conn(), &remote.backend_kind, &args.key)
            .map_err(|e| resolver::storage_error(&e))?
    {
        let _ = writeln!(deps.stdout, "Already adopted: {}", existing.display_id);
        return Ok(Exit::Ok);
    }

    let adapter_opt = match factory::open_configured(store.conn(), deps.runner, deps.cwd) {
        Ok(adapter) => adapter,
        Err(err @ FactoryOpenError::NotImplemented) => return Err(CommandError::failure(err)),
        Err(FactoryOpenError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };
    // `get_remote` already proved a Remote exists; `Ok(None)` here is only a
    // concurrent `tk remote clear` between the two reads. Treat it as no Remote.
    let Some(mut adapter) = adapter_opt else {
        return Err(no_remote());
    };

    // Eagerly fetch the single issue. Pull is all-or-nothing (ADR-0034): a
    // non-existent issue or a PR (tk-34's guard) surfaces verbatim.
    let snapshots = adapter
        .fetch_snapshots(&[args.key.as_str()])
        .map_err(pull_error)?;

    // Scenario-A insert via the shared merge; `DisplayIdCollision` is the
    // uniqueness backstop (ADR-0010). No Mutation is recorded — Adopt is a
    // current-state insert, like Backend Pull's insert path.
    store_sync::merge_backend_snapshots(store.conn_mut(), &mut *deps.rng, &snapshots, &now)
        .map_err(merge_error)?;

    // Render from the stored row, not the snapshot: the snapshot carries no
    // Priority, and reading back what merge persisted keeps the displayed
    // Priority honest as backend-Priority mapping arrives (Jira). The fetch
    // returns one snapshot per requested key, so its identity addresses the
    // row merge just wrote.
    let snap = snapshots
        .first()
        .expect("fetch_snapshots returns one snapshot per requested key");
    let adopted =
        store_sync::find_backend_item(store.conn(), &snap.backend_kind, &snap.backend_key)
            .map_err(|e| resolver::storage_error(&e))?
            .expect("the Backend item merge just wrote is present in the store");

    // Mirror `tk add`'s created-item block. The Status line carries the
    // allow-closed signal: a closed issue is adopted as a `done` Backend Ticket
    // (held out of `tk next`/`tk list` and never refreshed), so `Status: done`
    // is how Adopt avoids silently inserting an inert Ticket.
    let _ = writeln!(
        deps.stdout,
        "Adopted Ticket: {} - {}",
        adopted.display_id, adopted.title
    );
    if let Some(kind) = adopted.ticket_kind {
        let _ = writeln!(deps.stdout, "Kind: {kind}");
    }
    if let Some(priority) = adopted.priority {
        let _ = writeln!(deps.stdout, "Priority: {priority}");
    }
    let _ = writeln!(deps.stdout, "Status: {}", adopted.status);
    Ok(Exit::Ok)
}

/// The no-Remote diagnostic, shared by the `get_remote` and (defensive)
/// `open_configured` arms. The body matches `tk sync`'s verbatim; it is re-typed
/// here rather than shared as a constant so the literal stays grep-able.
fn no_remote() -> CommandError {
    CommandError::failure("no Remote configured; run 'tk remote set <kind>' first")
}

/// Map a [`PullError`] to a seam-framed failure. Both arms surface the adapter's
/// own body — `Failed` carries the backend CLI's stderr (or the adapter's PR /
/// parse diagnostic) verbatim; `Env` is the bare runner failure — matching the
/// bodies `tk sync` renders.
fn pull_error(err: PullError) -> CommandError {
    match err {
        PullError::Failed(detail) => CommandError::failure(detail),
        PullError::Env(e) => CommandError::failure(e),
    }
}

/// Map a [`MergeError`] to a seam-framed failure, matching `tk sync`'s bodies.
fn merge_error(err: MergeError) -> CommandError {
    match err {
        MergeError::DisplayIdCollision(id) => CommandError::failure(format!(
            "Display ID '{id}' already claimed by an existing Item"
        )),
        MergeError::Storage(e) => resolver::storage_error(&e),
        MergeError::Sequence(e) => {
            CommandError::failure(format!("Repository Store corruption: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, ProcError, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureRemote, TmpStore, insert_fixture_item, insert_fixture_remote,
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
    }

    /// Queue the `git rev-parse` discovery call `open_for_command` makes. FIFO,
    /// so this must precede any `gh` expectation.
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

    fn ok(stdout: &str) -> RunOutput {
        RunOutput {
            exit_code: 0,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn fail(exit_code: i32, stderr: &str) -> RunOutput {
        RunOutput {
            exit_code,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// A `gh issue view --json` object shaped like the adapter's `GhIssue`.
    fn issue_json(number: i64, state: &str, issue_type: &str, url: &str) -> String {
        let it = if issue_type == "null" {
            "null".to_string()
        } else {
            format!(r#"{{"name":"{issue_type}"}}"#)
        };
        format!(
            r#"{{"number":{number},"title":"Fix login","body":"B","state":"{state}","issueType":{it},"updatedAt":"2026-06-20T00:00:00Z","url":"{url}"}}"#
        )
    }

    /// Drive `run` and frame any error exactly as the dispatch seam does
    /// (ADR-0032: `tk adopt: <body>`), so a test asserts the framed bytes.
    fn run_rendered(h: &mut Harness<'_>, key: &str) -> Exit {
        let mut deps = h.deps();
        match run(&mut deps, Args { key: key.into() }) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "adopt");
                exit
            }
        }
    }

    #[test]
    fn adopts_an_open_issue_and_renders_the_created_block() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        h.runner.expect(
            &["gh", "issue", "view", "42"],
            ok(&issue_json(
                42,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/42",
            )),
        );

        let code = run_rendered(&mut h, "42");
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        assert!(
            stdout.contains("Adopted Ticket: gh-42 - Fix login"),
            "{stdout}"
        );
        assert!(stdout.contains("Kind: task"), "{stdout}");
        assert!(stdout.contains("Priority: P2"), "{stdout}");
        assert!(stdout.contains("Status: open"), "{stdout}");

        // The merged row is an accepted, backend-origin Ticket — and Adopt is a
        // current-state insert, so it leaves the Mutation Log empty.
        let mutations: i64 = conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0, "Adopt records no Mutation");
        let (origin, selection): (String, String) = conn
            .query_row(
                "select origin, selection_state from items where backend_key = '42'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(origin, "backend");
        assert_eq!(selection, "accepted");
    }

    #[test]
    fn adopting_a_closed_issue_shows_status_done() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        h.runner.expect(
            &["gh", "issue", "view", "7"],
            ok(&issue_json(
                7,
                "CLOSED",
                "Bug",
                "https://github.com/o/r/issues/7",
            )),
        );

        let code = run_rendered(&mut h, "7");
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        assert!(
            stdout.contains("Adopted Ticket: gh-7 - Fix login"),
            "{stdout}"
        );
        assert!(stdout.contains("Kind: bug"), "{stdout}");
        // The allow-closed signal: a closed issue is adopted as `done`, not
        // silently inserted as inert work.
        assert!(stdout.contains("Status: done"), "{stdout}");
    }

    #[test]
    fn already_adopted_is_idempotent_and_makes_no_backend_call() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "abc",
                display: "gh-42",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("42"),
                title: "Already here",
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        // Only the discovery call is queued: a `gh` call would exhaust the
        // FakeRunner and panic, proving the pre-check short-circuits before it.
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "42");
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        assert!(stdout.contains("Already adopted: gh-42"), "{stdout}");
        assert!(!stdout.contains("Adopted Ticket:"), "{stdout}");
    }

    #[test]
    fn no_remote_configured_is_a_failure_with_the_sync_guidance() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "42");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk adopt: no Remote configured; run 'tk remote set <kind>' first"),
            "{stderr}"
        );
    }

    #[test]
    fn jira_remote_is_not_implemented() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "jira",
                config_json: r#"{"site":"x","project":"P"}"#,
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "42");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains(
                "tk adopt: the configured Remote's adapter is not implemented in this build"
            ),
            "{stderr}"
        );
    }

    #[test]
    fn a_pull_request_is_rejected_verbatim() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        h.runner.expect(
            &["gh", "issue", "view", "99"],
            ok(&issue_json(
                99,
                "OPEN",
                "null",
                "https://github.com/o/r/pull/99",
            )),
        );

        let code = run_rendered(&mut h, "99");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk adopt: #99 is a pull request, not an issue"),
            "{stderr}"
        );
    }

    #[test]
    fn a_non_existent_issue_surfaces_the_backend_stderr() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let stderr_line = "GraphQL: Could not resolve to an issue or pull request \
                           with the number of 5. (repository.issue)";
        expect_git(&h, &store);
        h.runner
            .expect(&["gh", "issue", "view", "5"], fail(1, stderr_line));

        let code = run_rendered(&mut h, "5");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains(&format!("tk adopt: {stderr_line}")),
            "{stderr}"
        );
    }

    #[test]
    fn display_id_collision_is_reported() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        // A local Item already owns the `gh-42` Display ID the adapter would
        // mint for issue 42. The pre-check (keyed on backend identity) misses
        // it, so the merge's `item_ids` insert is the backstop.
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "local1",
                display: "gh-42",
                title: "Collides",
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        h.runner.expect(
            &["gh", "issue", "view", "42"],
            ok(&issue_json(
                42,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/42",
            )),
        );

        let code = run_rendered(&mut h, "42");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk adopt: Display ID 'gh-42' already claimed by an existing Item"),
            "{stderr}"
        );
    }

    #[test]
    fn pull_error_maps_both_arms_to_their_bodies() {
        // Failed carries the adapter body verbatim; Env is the bare runner
        // failure — both framed `tk adopt:` by the seam.
        let failed = pull_error(PullError::Failed("HTTP 502".into()));
        let mut out = Vec::new();
        failed.render(&mut out, "adopt");
        assert_eq!(String::from_utf8(out).unwrap(), "tk adopt: HTTP 502\n");

        let env = pull_error(PullError::Env(ProcError::ExecutableNotFound));
        let mut out = Vec::new();
        env.render(&mut out, "adopt");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "tk adopt: executable not found on PATH\n"
        );
    }
}
