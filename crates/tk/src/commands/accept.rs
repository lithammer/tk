//! `tk accept` — move a triage Ticket into accepted work with a Priority.
//!
//! Acceptance is the boundary where captured work becomes selectable: a triage
//! Ticket carries no Priority, and `tk accept <id> --priority Pn` ranks it and
//! flips its Selection State to `accepted` (ADR-0027). Re-accepting an
//! already-accepted Ticket without a Priority is an idempotent success;
//! supplying a Priority then is rejected — reprioritizing is `tk update`'s job.
//! Selection State is a Local Field, so acceptance never emits a Mutation.

use clap::Args as ClapArgs;

use crate::cli::{Deps, Exit};
use crate::commands::resolver;
use crate::domain::priority::Priority;
use crate::store::repository::selection::{self, AcceptError, AcceptOutcome};

const COMMAND: &str = "accept";

/// Flags for `tk accept`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket to accept.
    #[arg(value_name = "ID")]
    pub id: String,
    /// Priority to assign while accepting (P0..P4); required for a triage
    /// Ticket.
    #[arg(short = 'p', long, value_name = "PRIORITY")]
    pub priority: Option<Priority>,
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

    match selection::accept_ticket(&mut store, clock, &resolved.id, args.priority) {
        Ok(AcceptOutcome::Accepted {
            display_id,
            title,
            priority,
        }) => {
            let _ = writeln!(stdout, "Accepted Ticket: {display_id} - {title}");
            let _ = writeln!(stdout, "Priority: {priority}");
            Exit::Ok
        }
        Ok(AcceptOutcome::AlreadyAccepted { display_id }) => {
            let _ = writeln!(stdout, "{display_id} is already accepted");
            Exit::Ok
        }
        Err(AcceptError::NotFound) => {
            // Race: the row vanished between resolve and the write lock.
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is not a known Display ID or Alias",
                id = args.id
            );
            Exit::Failure
        }
        Err(AcceptError::NotATicket) => {
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is an Epic; Selection State applies to Tickets",
                id = args.id
            );
            Exit::Failure
        }
        Err(AcceptError::PriorityRequired) => {
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is in triage and needs a Priority (use --priority P0..P4)",
                id = args.id
            );
            Exit::Failure
        }
        Err(AcceptError::PriorityOnAccepted) => {
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is already accepted; change its Priority with \
                 'tk update {id} --priority Pn'",
                id = args.id
            );
            Exit::Failure
        }
        Err(AcceptError::Parked) => {
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is parked; restore it with 'tk unpark {id}'",
                id = args.id
            );
            Exit::Failure
        }
        Err(AcceptError::Sqlite(err)) => {
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
    fn accepts_a_triage_ticket_with_priority() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "triage", None);
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                id: "tk-1".into(),
                priority: Some(Priority::P1),
            },
        );
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Accepted Ticket: tk-1 - Subject"));
        assert!(stdout.contains("Priority: P1"));
    }

    #[test]
    fn triage_without_priority_points_at_the_flag() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "triage", None);
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                id: "tk-1".into(),
                priority: None,
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk accept: 'tk-1' is in triage and needs a Priority"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn re_accepting_is_idempotent() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "accepted", Some("P2"));
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                id: "tk-1".into(),
                priority: None,
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("tk-1 is already accepted"));
    }

    #[test]
    fn accepted_with_priority_points_at_update() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "accepted", Some("P2"));
        drop(conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                id: "tk-1".into(),
                priority: Some(Priority::P0),
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains(
                "already accepted; change its Priority with 'tk update tk-1 --priority Pn'"
            ),
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
                priority: Some(Priority::P1),
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk accept: 'tk-9999' is not a known Display ID or Alias"));
    }
}
