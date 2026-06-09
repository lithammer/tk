//! `tk park` — hold an accepted Ticket out of automatic selection.
//!
//! Parking keeps accepted work visible and ranked but excludes it from
//! `tk next` until it is unparked (ADR-0027). It preserves the Ticket's
//! Priority, so the held work returns to the queue at the same rank; the
//! success line echoes that Priority. Parking an already-parked Ticket is an
//! idempotent success; a triage Ticket must be accepted first. Selection State
//! is a Local Field, so parking never emits a Mutation.

use clap::Args as ClapArgs;

use crate::cli::{Deps, Exit};
use crate::commands::resolver;
use crate::store::repository::selection::{self, ParkError, ParkOutcome};

const COMMAND: &str = "park";

/// Flags for `tk park`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket to park.
    #[arg(value_name = "ID")]
    pub id: String,
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> Exit {
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
            resolver::render_open_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    let resolved = match resolver::resolve(&store, &args.id) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is not a known Display ID or Alias",
                id = args.id
            );
            return Exit::Failure;
        }
        Err(resolver::ResolveError::Storage(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    match selection::park_ticket(&mut store, clock, &resolved.id) {
        Ok(ParkOutcome::Parked {
            display_id,
            title,
            priority,
        }) => {
            let _ = writeln!(stdout, "Parked Ticket: {display_id} - {title}");
            let _ = writeln!(stdout, "Priority: {priority}");
            Exit::Ok
        }
        Ok(ParkOutcome::AlreadyParked { display_id }) => {
            let _ = writeln!(stdout, "{display_id} is already parked");
            Exit::Ok
        }
        Err(ParkError::NotFound) => {
            // Race: the row vanished between resolve and the write lock.
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is not a known Display ID or Alias",
                id = args.id
            );
            Exit::Failure
        }
        Err(ParkError::NotATicket) => {
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is an Epic; Selection State applies to Tickets",
                id = args.id
            );
            Exit::Failure
        }
        Err(ParkError::Triage) => {
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is in triage; accept it first with \
                 'tk accept {id} --priority P0..P4'",
                id = args.id
            );
            Exit::Failure
        }
        Err(ParkError::Active) => {
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is active; stop it first with 'tk stop {id}'",
                id = args.id
            );
            Exit::Failure
        }
        Err(ParkError::Sqlite(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            Exit::Failure
        }
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
    fn parks_an_accepted_ticket_and_echoes_priority() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "accepted", Some("P1"));
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout.contains("Parked Ticket: tk-1 - Subject"),
            "stdout={stdout:?}"
        );
        assert!(stdout.contains("Priority: P1"), "stdout={stdout:?}");
    }

    #[test]
    fn re_parking_is_idempotent() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "parked", Some("P2"));
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        assert!(
            String::from_utf8(h.stdout)
                .unwrap()
                .contains("tk-1 is already parked")
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
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk park: 'tk-1' is in triage; accept it first with 'tk accept tk-1 --priority P0..P4'"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn parking_an_active_ticket_points_at_stop() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                status: "active",
                priority: Some("P2"),
                selection_state: Some("accepted"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk park: 'tk-1' is active; stop it first with 'tk stop tk-1'"),
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
        let code = run(
            h.deps(),
            Args {
                id: "tk-9999".into(),
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk park: 'tk-9999' is not a known Display ID or Alias"));
    }
}
