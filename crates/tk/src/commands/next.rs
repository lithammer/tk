//! `tk next` — select the next ready Ticket.
//!
//! Ranks ready Tickets by Effective Priority (lowest first), then own
//! Priority, then created_seq, within the active Scope (per ADR-0015).
//! Prints one Display ID to stdout. When the pick's Effective Priority is
//! lower than its own Priority, also writes a rationale line to stderr
//! (`<display>: Effective Priority <ep> (via <contributor>)`) so
//! `id="$(tk next)"` scripting stays uncluttered.
//!
//! Scope is the optional `<epic-id>` argument or `TK_SCOPE` (ADR-0022),
//! resolved Epic-only here before selection runs.

use std::io::Write;

use clap::Args as ClapArgs;

use crate::cli::{Deps, Exit};
use crate::commands::{resolver, scope};
use crate::store::repository::next::{self, NextError, NextOptions, NextScope, Rationale};

const COMMAND: &str = "next";

/// Flags for `tk next`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Restrict selection to this Epic's child Tickets. Falls back to the
    /// `TK_SCOPE` environment variable; absent both, all ready Tickets are
    /// considered.
    #[arg(value_name = "EPIC_ID")]
    pub epic: Option<String>,
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> Exit {
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
            resolver::render_open_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    let scope_value = scope::effective_value(args.epic.as_deref(), scope::env_value().as_deref());
    let scope_epic = match scope_value.as_deref() {
        None => None,
        Some(value) => match resolver::resolve_epic_with_display(&store, value) {
            Ok(epic) => Some(epic),
            Err(resolver::ResolveEpicError::NotFound) => {
                let _ = writeln!(
                    stderr,
                    "tk next: scope '{value}' is not a known Display ID or Alias"
                );
                return Exit::Failure;
            }
            Err(resolver::ResolveEpicError::NotAnEpic) => {
                let _ = writeln!(stderr, "tk next: scope '{value}' is not an Epic");
                return Exit::Failure;
            }
            Err(resolver::ResolveEpicError::Storage(err)) => {
                resolver::render_storage_error(stderr, COMMAND, &err);
                return Exit::Failure;
            }
        },
    };

    let next_scope = match &scope_epic {
        None => NextScope::None,
        Some(epic) => NextScope::Epic(epic.id.as_str()),
    };

    match next::next_ready_ticket(&store, NextOptions { scope: next_scope }) {
        Ok(Some(ticket)) => {
            let _ = writeln!(stdout, "{}", ticket.display_id);
            if let Some(rationale) = ticket.rationale.as_ref() {
                render_rationale(stderr, &ticket.display_id, rationale);
            }
            Exit::Ok
        }
        Ok(None) => {
            match &scope_epic {
                Some(epic) => {
                    let _ = writeln!(
                        stderr,
                        "tk next: no ready Tickets in Epic {}",
                        epic.display_id
                    );
                }
                None => {
                    let _ = writeln!(stderr, "tk next: no ready Tickets");
                }
            }
            Exit::Failure
        }
        Err(NextError::Storage(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            Exit::Failure
        }
    }
}

fn render_rationale<W: Write + ?Sized>(stderr: &mut W, display_id: &str, r: &Rationale) {
    let _ = writeln!(
        stderr,
        "{display_id}: Effective Priority {ep} (via {blocked})",
        ep = r.effective_priority,
        blocked = r.blocked_display_id,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, TmpStore, insert_dependency, insert_fixture_item};
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

    fn seed_ticket(conn: &Connection, id: &str, display: &str, priority: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: id,
                priority: Some(priority),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
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

    /// Stage the single git call store-open makes (`git rev-parse` for
    /// repository discovery). Scope no longer reads git state (ADR-0022).
    fn expect_open(h: &Harness<'_>, store: &TmpStore) {
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
    }

    fn seed_epic(conn: &Connection, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: id,
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_child(
        conn: &Connection,
        id: &str,
        display: &str,
        priority: &str,
        epic: &str,
        created_seq: i64,
    ) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: id,
                priority: Some(priority),
                container_id: Some(epic),
                container_class: Some("epic"),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn args(epic: Option<&str>) -> Args {
        Args {
            epic: epic.map(str::to_owned),
        }
    }

    #[test]
    fn empty_store_reports_no_ready_ticket() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run(h.deps(), args(None));
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk next: no ready Tickets"));
        assert!(!stderr.contains("Epic"));
    }

    #[test]
    fn prints_highest_priority_ready_ticket_to_stdout() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "low", "tk-1", "P3", 1);
        seed_ticket(&conn, "high", "tk-2", "P0", 2);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run(h.deps(), args(None));
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(stdout.trim(), "tk-2");
    }

    #[test]
    fn rationale_lands_on_stderr_when_effective_priority_promotes() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "blocker", "tk-1", "P3", 1);
        seed_ticket(&conn, "blocked-high", "tk-2", "P0", 2);
        insert_dependency(&conn, "blocker", "blocked-high").unwrap();
        seed_ticket(&conn, "ready", "tk-3", "P1", 3);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run(h.deps(), args(None));
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert_eq!(stdout.trim(), "tk-1");
        assert!(
            stderr.contains("tk-1: Effective Priority P0 (via tk-2)"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn epic_scope_selects_a_child_over_a_higher_priority_outsider() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "child", "tk-2", "P2", "epic", 2);
        seed_ticket(&conn, "outside", "tk-3", "P0", 3);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run(h.deps(), args(Some("tk-1")));
        assert_eq!(code, Exit::Ok);
        // tk-3 outranks tk-2 globally, but the Epic Scope confines selection.
        assert_eq!(String::from_utf8(h.stdout).unwrap().trim(), "tk-2");
    }

    #[test]
    fn epic_scope_with_no_ready_child_names_the_epic_in_the_empty_message() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "blocked-child", "tk-2", "P0", "epic", 2);
        seed_ticket(&conn, "blocker", "tk-3", "P0", 3);
        insert_dependency(&conn, "blocker", "blocked-child").unwrap();
        // A ready Ticket outside the Epic must not rescue the empty result.
        seed_ticket(&conn, "outside", "tk-4", "P0", 4);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run(h.deps(), args(Some("tk-1")));
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk next: no ready Tickets in Epic tk-1"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn unknown_scope_value_renders_typed_error() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run(h.deps(), args(Some("vanished")));
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk next: scope 'vanished' is not a known Display ID or Alias"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn ticket_scope_is_rejected_as_not_an_epic() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "P2", 1);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run(h.deps(), args(Some("tk-1")));
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk next: scope 'tk-1' is not an Epic"),
            "stderr={stderr:?}"
        );
    }
}
