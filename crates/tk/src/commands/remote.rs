//! `tk remote` — configure the Primary Backend (ADR-0033).
//!
//! - `tk remote set github` records that the Primary Backend is GitHub
//!   (`config_json = {}`). The GitHub repository is not stored; the Backend
//!   Adapter resolves it from the checkout at sync time (ADR-0033). The command
//!   takes no repo argument and is idempotent.
//! - `tk remote set jira` is refused in v1 — `jira` parses to a real
//!   [`BackendKind`] but no Jira Backend Adapter exists yet (Usage, exit 2).
//! - `tk remote clear` removes the Remote only when no pending or failed
//!   Mutations would be orphaned (CONTEXT.md).
//! - bare `tk remote` shows the configured kind.
//!
//! Born on the ADR-0032 diagnostics seam: [`run`] returns
//! `Result<Exit, CommandError>` and the dispatch seam frames `tk remote:
//! <body>`. The verbatim message bodies live in each typed error's `#[error]`.

use clap::{Args as ClapArgs, Subcommand};

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::domain::backend_kind::BackendKind;
use crate::store::sync::{self as store_sync, ClearRemoteError, SetRemoteOutcome};

/// Flags for `tk remote`. Absent a subcommand the command shows the configured
/// Remote, mirroring `tk sync`'s bare-invocation shape.
#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub subcommand: Option<Sub>,
}

#[derive(Debug, Subcommand)]
pub enum Sub {
    /// Configure the Primary Backend (v1: `github` only).
    Set(SetArgs),
    /// Remove the configured Remote.
    Clear,
}

/// Flags for `tk remote set`.
#[derive(Debug, ClapArgs)]
pub struct SetArgs {
    /// Backend kind. v1 supports `github`; `jira` is reserved but not yet
    /// settable.
    #[arg(value_name = "KIND")]
    pub kind: String,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    match args.subcommand {
        Some(Sub::Set(set)) => run_set(deps, &set.kind),
        Some(Sub::Clear) => run_clear(deps),
        None => run_show(deps),
    }
}

fn run_set(deps: &mut Deps<'_>, raw_kind: &str) -> Result<Exit, CommandError> {
    // Validate the kind before opening the store: an unsupported kind is a
    // usage error caught from arguments alone (ADR-0032).
    let kind = parse_set_kind(raw_kind)?;
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();

    // A v1 GitHub Remote stores an empty config: the repository is resolved from
    // the checkout at sync time, not persisted (ADR-0033).
    match store_sync::set_remote(store.conn_mut(), kind, "{}", &now) {
        Ok(SetRemoteOutcome::Created) => {
            let _ = writeln!(deps.stdout, "Configured Remote: {kind}");
            Ok(Exit::Ok)
        }
        Ok(SetRemoteOutcome::Unchanged) => {
            let _ = writeln!(deps.stdout, "Remote already configured: {kind}");
            Ok(Exit::Ok)
        }
        Err(err) => Err(resolver::storage_error(&err)),
    }
}

/// Map the raw `<kind>` to a settable [`BackendKind`]. `jira` parses but is
/// refused in v1 (ADR-0033); an unknown token is an unknown-kind error. Both
/// are Usage (exit 2) — the same code a value-restricted argument would yield.
fn parse_set_kind(raw: &str) -> Result<BackendKind, CommandError> {
    match raw.parse::<BackendKind>() {
        Ok(BackendKind::Github) => Ok(BackendKind::Github),
        Ok(BackendKind::Jira) => Err(CommandError::usage("Jira Remotes are not supported in v1")),
        Err(_) => Err(CommandError::usage(format!(
            "unknown backend kind '{raw}'; expected 'github'"
        ))),
    }
}

fn run_clear(deps: &mut Deps<'_>) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    match store_sync::clear_remote(store.conn_mut()) {
        Ok(()) => {
            let _ = writeln!(deps.stdout, "Cleared the configured Remote.");
            Ok(Exit::Ok)
        }
        // A storage fault routes through the busy-aware classifier.
        Err(ClearRemoteError::Storage(err)) => Err(resolver::storage_error(&err)),
        // NotConfigured / WouldOrphan carry their verbatim body in their
        // `#[error]`; forward it as the failure body for the seam to frame.
        Err(other) => Err(CommandError::failure(other)),
    }
}

fn run_show(deps: &mut Deps<'_>) -> Result<Exit, CommandError> {
    let store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    match store_sync::get_remote(store.conn()) {
        // Kind-only identity line (ADR-0033): the v1 config carries no repo, and
        // the Sync Cursor belongs to `tk sync log`, not this config view.
        Ok(Some(remote)) => {
            let _ = writeln!(deps.stdout, "Remote: {}", remote.backend_kind);
            Ok(Exit::Ok)
        }
        // A query result, not a failure: empty stdout line at exit 0, mirroring
        // `tk sync log`'s empty message.
        Ok(None) => {
            let _ = writeln!(deps.stdout, "No Remote configured.");
            Ok(Exit::Ok)
        }
        Err(err) => Err(resolver::storage_error(&err)),
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
        FixtureItem, FixtureMutation, TmpStore, insert_fixture_item, insert_fixture_mutation,
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

    /// Drive `run` and frame any returned error exactly as the dispatch seam
    /// does (ADR-0032: `tk remote: <body>`), so a test asserts the framed bytes.
    fn run_rendered(h: &mut Harness<'_>, args: Args) -> Exit {
        let mut deps = h.deps();
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "remote");
                exit
            }
        }
    }

    fn set_args(kind: &str) -> Args {
        Args {
            subcommand: Some(Sub::Set(SetArgs { kind: kind.into() })),
        }
    }

    fn show_args() -> Args {
        Args { subcommand: None }
    }

    fn clear_args() -> Args {
        Args {
            subcommand: Some(Sub::Clear),
        }
    }

    #[test]
    fn set_github_configures_empty_remote_and_zero_cursor() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, set_args("github"));
        assert_eq!(code, Exit::Ok);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("Configured Remote: github")
        );

        let conn = Connection::open(store.db_path()).unwrap();
        let (kind, config): (String, String) = conn
            .query_row(
                "select backend_kind, config_json from remotes where name = 'primary'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "github");
        assert_eq!(config, "{}", "v1 GitHub Remote stores no repo (ADR-0033)");
        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 0);
    }

    #[test]
    fn set_github_twice_is_idempotent() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();

        {
            let mut h = Harness::new(&cwd_path);
            expect_git(&h, &store);
            assert_eq!(run_rendered(&mut h, set_args("github")), Exit::Ok);
        }
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, set_args("github"));
        assert_eq!(code, Exit::Ok);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("Remote already configured: github")
        );

        let conn = Connection::open(store.db_path()).unwrap();
        let count: i64 = conn
            .query_row("select count(*) from remotes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "idempotent set keeps one Remote row");
    }

    #[test]
    fn set_jira_is_refused_as_usage() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        // No git expectation: parse_set_kind rejects before the store opens.

        let code = run_rendered(&mut h, set_args("jira"));
        assert_eq!(code, Exit::Usage);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk remote: Jira Remotes are not supported in v1"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn set_unknown_kind_is_refused_as_usage() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);

        let code = run_rendered(&mut h, set_args("gitlab"));
        assert_eq!(code, Exit::Usage);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk remote: unknown backend kind 'gitlab'; expected 'github'"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn show_unconfigured_prints_none_at_exit_zero() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, show_args());
        assert_eq!(code, Exit::Ok);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("No Remote configured.")
        );
    }

    #[test]
    fn show_configured_prints_kind_only() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        {
            let mut h = Harness::new(&cwd_path);
            expect_git(&h, &store);
            run_rendered(&mut h, set_args("github"));
        }

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, show_args());
        assert_eq!(code, Exit::Ok);
        assert_eq!(
            String::from_utf8(h.stdout).unwrap(),
            "Remote: github\n",
            "show is kind-only"
        );
    }

    #[test]
    fn clear_unconfigured_refuses() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, clear_args());
        assert_eq!(code, Exit::Failure);
        assert!(
            String::from_utf8(h.stderr)
                .unwrap()
                .contains("tk remote: no Remote configured")
        );
    }

    #[test]
    fn clear_removes_configured_remote() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        {
            let mut h = Harness::new(&cwd_path);
            expect_git(&h, &store);
            run_rendered(&mut h, set_args("github"));
        }

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, clear_args());
        assert_eq!(code, Exit::Ok);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("Cleared the configured Remote.")
        );

        let conn = Connection::open(store.db_path()).unwrap();
        let remotes: i64 = conn
            .query_row("select count(*) from remotes", [], |r| r.get(0))
            .unwrap();
        let cursors: i64 = conn
            .query_row("select count(*) from sync_cursors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remotes, 0);
        assert_eq!(cursors, 0, "the Sync Cursor is removed with the Remote");
    }

    #[test]
    fn clear_refuses_when_pending_mutation_would_be_orphaned() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        // A backend Ticket with a pending Mutation: clearing would orphan it.
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-1",
                title: "T",
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
        {
            let mut h = Harness::new(&cwd_path);
            expect_git(&h, &store);
            run_rendered(&mut h, set_args("github"));
        }

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, clear_args());
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("pending or failed Mutation"),
            "stderr={stderr:?}"
        );

        // The Remote survives a refused clear.
        let conn = Connection::open(store.db_path()).unwrap();
        let remotes: i64 = conn
            .query_row("select count(*) from remotes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remotes, 1);
    }
}
