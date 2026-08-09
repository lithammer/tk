//! `tk adopt` — bring one existing Backend issue into the Repository Store as a
//! Backend Ticket (ADR-0034: Adopt is the sole Backend → tk intake path in v1).
//!
//! `tk adopt <key>` eagerly fetches the single issue named by `<key>` through
//! the configured Backend Adapter and inserts it as an `accepted` Backend
//! Ticket (Display ID `gh-<n>` for GitHub). It is the inverse intake direction
//! to Promotion and records the Backend's current state without a Mutation.
//!
//! `<key>` is the backend-native identifier (a bare issue number for GitHub),
//! passed to the adapter verbatim — tk does not normalise URLs or `#`-prefixes,
//! because the command is backend-agnostic and the Adapter owns
//! canonicalization. The Store checks canonical Backend identity under the
//! insertion transaction.
//!
//! Per ADR-0032, [`run`] returns `Result<Exit, CommandError>` and the dispatch
//! seam frames failures as `tk adopt: <body>`.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::remote::adapter::AdapterReadError;
use crate::remote::factory::{self, OpenError as FactoryOpenError};
use crate::store::sync::{self as store_sync, AdoptOutcome, AdoptStoreError, BackendCohortError};

/// Flags for `tk adopt`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Backend-native key of the issue to adopt (a bare issue number for
    /// GitHub). Passed to the Backend Adapter verbatim — tk does not normalise
    /// URLs or `#`-prefixes before Adapter canonicalization.
    #[arg(value_name = "KEY")]
    pub key: String,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();
    let _workflow = store
        .lock_remote_workflow()
        .map_err(CommandError::failure)?;
    store_sync::ensure_adopt_available(store.conn()).map_err(adopt_store_error)?;

    let adapter_opt = match factory::open_configured(store.conn(), deps.runner, deps.cwd) {
        Ok(adapter) => adapter,
        Err(err @ FactoryOpenError::NotImplemented) => return Err(CommandError::failure(err)),
        Err(FactoryOpenError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };
    let Some(mut adapter) = adapter_opt else {
        return Err(no_remote());
    };

    // Canonicalize the single issue through its Backend Adapter. Adopt does not
    // trust raw input for idempotence because Backends may accept aliases.
    // A non-existent issue or a PR (tk-34's guard) surfaces verbatim.
    let adopted = adapter
        .adopt_ticket(&args.key)
        .map_err(adapter_read_error)?;

    let expected = adapter.backend_kind();
    let outcome = store_sync::adopt_backend_ticket(
        store.conn_mut(),
        expected,
        &mut *deps.rng,
        &adopted,
        &now,
    )
    .map_err(adopt_store_error)?;
    let stored = match outcome {
        AdoptOutcome::Inserted(row) => row,
        AdoptOutcome::AlreadyExists(row) => {
            let _ = writeln!(deps.stdout, "Already adopted: {}", row.display_id);
            return Ok(Exit::Ok);
        }
    };

    // The Status line carries the allow-closed signal: a closed issue is
    // adopted as a `done` Backend Ticket
    // (held out of `tk next`/`tk list` and never refreshed), so `Status: done`
    // is how Adopt avoids silently inserting an inert Ticket.
    let _ = writeln!(
        deps.stdout,
        "Adopted Ticket: {} - {}",
        stored.display_id, stored.title
    );
    if let Some(kind) = stored.ticket_kind {
        let _ = writeln!(deps.stdout, "Kind: {kind}");
    }
    if let Some(priority) = stored.priority {
        let _ = writeln!(deps.stdout, "Priority: {priority}");
    }
    let _ = writeln!(deps.stdout, "Status: {}", stored.status);
    Ok(Exit::Ok)
}

/// The no-Remote diagnostic returned when the Adapter factory finds none.
fn no_remote() -> CommandError {
    CommandError::failure("no Remote configured; run 'tk remote set <kind>' first")
}

/// Map an [`AdapterReadError`] to a seam-framed failure.
///
/// Both arms preserve the Adapter boundary's body: `Failed` carries the
/// Backend CLI stderr or Adapter validation diagnostic, while `Env` carries
/// the subprocess environment failure.
fn adapter_read_error(err: AdapterReadError) -> CommandError {
    match err {
        AdapterReadError::Failed(detail) => CommandError::failure(detail),
        AdapterReadError::Env(e) => CommandError::failure(e),
    }
}

/// Map an [`AdoptStoreError`] to a seam-framed failure.
fn adopt_store_error(err: AdoptStoreError) -> CommandError {
    match err {
        AdoptStoreError::DisplayIdCollision(id) => CommandError::failure(format!(
            "Display ID '{id}' already claimed by an existing Item"
        )),
        AdoptStoreError::RemoteChanged { .. } => CommandError::failure(
            "the configured Remote changed while contacting the Backend; retry 'tk adopt'",
        ),
        AdoptStoreError::Storage(e)
        | AdoptStoreError::BackendCohort(BackendCohortError::Storage(e)) => {
            resolver::storage_error(&e)
        }
        AdoptStoreError::Sequence(e) => {
            CommandError::failure(format!("Repository Store corruption: {e}"))
        }
        AdoptStoreError::BackendCohort(other) => {
            CommandError::failure(format!("Repository Store corruption: {other}"))
        }
        AdoptStoreError::ApplyingMutation(sequence) => CommandError::failure(format!(
            "Mutation {sequence} has an indeterminate Backend creation outcome; resolve it before adopting another Item"
        )),
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
            &["gh", "issue", "view", "https://github.com/o/r/issues/42"],
            ok(&issue_json(
                42,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/42",
            )),
        );

        let code = run_rendered(&mut h, "https://github.com/o/r/issues/42");
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
    fn applying_creation_blocks_adopt_before_the_backend_read() {
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
                sequence: 9,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "applying",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "42");

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            String::from_utf8(h.stderr).unwrap(),
            "tk adopt: Mutation 9 has an indeterminate Backend creation outcome; resolve it before adopting another Item\n"
        );
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
    fn already_adopted_fetches_canonical_identity_without_updating_stored_ticket() {
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
        expect_git(&h, &store);
        h.runner.expect(
            &["gh", "issue", "view", "https://github.com/o/r/issues/42"],
            ok(&issue_json(
                42,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/42",
            )),
        );

        let code = run_rendered(&mut h, "https://github.com/o/r/issues/42");
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        assert!(stdout.contains("Already adopted: gh-42"), "{stdout}");
        assert!(!stdout.contains("Adopted Ticket:"), "{stdout}");
        let title: String = conn
            .query_row("select title from items where id = 'abc'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "Already here");
        let item_count: i64 = conn
            .query_row("select count(*) from items", [], |row| row.get(0))
            .unwrap();
        let created_sequence: i64 = conn
            .query_row(
                "select value from sequences where name = 'item_created_seq'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((item_count, created_sequence), (1, 0));
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
        // mint for issue 42. The transactional canonical-identity lookup finds
        // no Backend Item, so the `item_ids` insert is the collision backstop.
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
    fn adapter_read_error_maps_both_arms_to_their_bodies() {
        // Failed carries the adapter body verbatim; Env is the bare runner
        // failure — both framed `tk adopt:` by the seam.
        let failed = adapter_read_error(AdapterReadError::Failed("HTTP 502".into()));
        let mut out = Vec::new();
        failed.render(&mut out, "adopt");
        assert_eq!(String::from_utf8(out).unwrap(), "tk adopt: HTTP 502\n");

        let env = adapter_read_error(AdapterReadError::Env(ProcError::ExecutableNotFound));
        let mut out = Vec::new();
        env.render(&mut out, "adopt");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "tk adopt: executable not found on PATH\n"
        );
    }
}
