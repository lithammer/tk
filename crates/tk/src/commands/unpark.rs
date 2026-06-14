//! `tk unpark` — return a parked Ticket to accepted, selectable work.
//!
//! Unparking is the inverse of `tk park` (ADR-0027): it moves a parked Ticket
//! back to `accepted` so `tk next` can select it again, without requiring a
//! Priority flag — the held Priority rides through unchanged, and the success
//! line echoes the rank the work re-enters the queue at. Unparking an
//! already-accepted Ticket is an idempotent success; a triage Ticket must be
//! accepted first. Selection State is a Local Field, so unparking never emits a
//! Mutation.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::store::repository::selection::{self, UnparkError, UnparkOutcome};

/// Flags for `tk unpark`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket to unpark.
    #[arg(value_name = "ID")]
    pub id: String,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    let resolved = match resolver::resolve(&store, &args.id) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "'{id}' is not a known Display ID or Alias",
                id = args.id
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };

    match selection::unpark_ticket(&mut store, deps.clock, &resolved.id) {
        Ok(UnparkOutcome::Unparked {
            display_id,
            title,
            priority,
        }) => {
            let _ = writeln!(deps.stdout, "Unparked Ticket: {display_id} - {title}");
            let _ = writeln!(deps.stdout, "Priority: {priority}");
            Ok(Exit::Ok)
        }
        Ok(UnparkOutcome::AlreadyAccepted { display_id }) => {
            let _ = writeln!(deps.stdout, "{display_id} is already accepted");
            Ok(Exit::Ok)
        }
        // Race: the row vanished between resolve and the write lock.
        Err(UnparkError::NotFound) => Err(CommandError::failure(format!(
            "'{id}' is not a known Display ID or Alias",
            id = args.id
        ))),
        Err(UnparkError::NotATicket) => Err(CommandError::failure(format!(
            "'{id}' is an Epic; Selection State applies to Tickets",
            id = args.id
        ))),
        Err(UnparkError::Triage) => Err(CommandError::failure(format!(
            "'{id}' is in triage; accept it first with \
             'tk accept {id} --priority P0..P4'",
            id = args.id
        ))),
        Err(UnparkError::Sqlite(err)) => Err(resolver::storage_error(&err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, TmpStore, insert_fixture_item};
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

    /// Drive `run` and frame any returned error as the dispatch seam does
    /// (ADR-0032: `tk unpark: <body>`), so a test asserts the framed bytes.
    fn run_rendered(h: &mut Harness<'_>, args: Args) -> Exit {
        let mut deps = h.deps();
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "unpark");
                exit
            }
        }
    }

    fn seed_ticket(
        conn: &Connection,
        id: &str,
        display: &str,
        selection: &str,
        prio: Option<&str>,
    ) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Subject",
                priority: prio,
                selection_state: Some(selection),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn unparks_a_parked_ticket_and_echoes_priority() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "parked", Some("P1"));
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout.contains("Unparked Ticket: tk-1 - Subject"),
            "stdout={stdout:?}"
        );
        assert!(stdout.contains("Priority: P1"), "stdout={stdout:?}");
    }

    #[test]
    fn re_unparking_is_idempotent() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "accepted", Some("P2"));
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("tk-1 is already accepted")
        );
    }

    #[test]
    fn triage_points_at_accept() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "triage", None);
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk unpark: 'tk-1' is in triage; accept it first with 'tk accept tk-1 --priority P0..P4'"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn unknown_id_is_not_found() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                id: "tk-9999".into(),
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk unpark: 'tk-9999' is not a known Display ID or Alias"));
    }
}
