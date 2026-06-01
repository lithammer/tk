//! `tk next` — select the next ready Ticket.
//!
//! Ranks ready Tickets by Effective Priority (lowest first), then own
//! Priority, then created_seq, within the active Workspace Scope (per
//! ADR-0015). Prints one Display ID to stdout. When the pick's Effective
//! Priority is lower than its own Priority, also writes a rationale line
//! to stderr (`<display>: Effective Priority <ep> (via <contributor>)`)
//! so `id="$(tk next)"` scripting stays uncluttered.

use std::io::Write;

use clap::Args as ClapArgs;

use crate::cli::{Deps, Exit};
use crate::commands::resolver;
use crate::store::repository::next::{self, NextError, NextOptions, NextScope, Rationale};
use crate::worktree::scope as worktree_scope;

const COMMAND: &str = "next";

/// Flags for `tk next`. No options today; reserved for future scope
/// overrides (e.g. `--all` to ignore Workspace Scope).
#[derive(Debug, ClapArgs)]
pub struct Args {}

#[must_use]
pub fn run(deps: Deps<'_>, _args: Args) -> Exit {
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

    let raw = worktree_scope::read_git_side(runner, cwd);
    let resolved_scope = match worktree_scope::resolve_against_store(&store, &raw) {
        Ok(scope) => scope,
        Err(worktree_scope::ScopeError::ConfiguredUnresolved(stored)) => {
            let _ = writeln!(
                stderr,
                "tk next: Workspace Scope '{stored}' is not a known Display ID or Alias"
            );
            return Exit::Failure;
        }
        Err(worktree_scope::ScopeError::Storage(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    let (next_scope, has_scope) = match &resolved_scope {
        None => (NextScope::None, false),
        Some(s) => (NextScope::DisplayArg(s.display_id.as_str()), true),
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
            let line = if has_scope {
                "tk next: no ready Tickets in Workspace Scope"
            } else {
                "tk next: no ready Tickets"
            };
            let _ = writeln!(stderr, "{line}");
            Exit::Failure
        }
        Err(NextError::ScopeNotFound) => {
            let _ = writeln!(
                stderr,
                "tk next: Workspace Scope does not resolve to a Ticket or Epic"
            );
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

    /// Stage one fake git invocation: `git rev-parse` succeeds with the
    /// repo's paths, scope reads fail (treated as "no scope").
    fn expect_open_and_no_scope(h: &Harness<'_>, store: &TmpStore) {
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
        // scope: tk.scope absent.
        h.runner.expect(
            &["git", "config", "--worktree", "--get", "tk.scope"],
            RunOutput {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
        // scope: branch name detached.
        h.runner.expect(
            &["git", "symbolic-ref", "--short", "HEAD"],
            RunOutput {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
    }

    #[test]
    fn empty_store_reports_no_ready_ticket() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open_and_no_scope(&h, &store);
        let code = run(h.deps(), Args {});
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk next: no ready Tickets"));
        assert!(!stderr.contains("Workspace Scope"));
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
        expect_open_and_no_scope(&h, &store);
        let code = run(h.deps(), Args {});
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
        expect_open_and_no_scope(&h, &store);
        let code = run(h.deps(), Args {});
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
    fn configured_scope_filters_selection_and_changes_empty_message() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "in-scope-blocked", "tk-1", "P0", 1);
        seed_ticket(&conn, "out-of-scope", "tk-2", "P0", 2);
        seed_ticket(&conn, "blocker", "tk-3", "P0", 3);
        insert_dependency(&conn, "blocker", "in-scope-blocked").unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        // open
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
        // scope configured to tk-1
        h.runner.expect(
            &["git", "config", "--worktree", "--get", "tk.scope"],
            RunOutput {
                exit_code: 0,
                stdout: b"tk-1\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        h.runner.expect(
            &["git", "symbolic-ref", "--short", "HEAD"],
            RunOutput {
                exit_code: 0,
                stdout: b"main\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        let code = run(h.deps(), Args {});
        // tk-1 is blocked; with scope=tk-1, no other Ticket is in scope.
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk next: no ready Tickets in Workspace Scope"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn configured_scope_value_that_does_not_resolve_renders_typed_error() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
        h.runner.expect(
            &["git", "config", "--worktree", "--get", "tk.scope"],
            RunOutput {
                exit_code: 0,
                stdout: b"vanished\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        h.runner.expect(
            &["git", "symbolic-ref", "--short", "HEAD"],
            RunOutput {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
        let code = run(h.deps(), Args {});
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr
                .contains("tk next: Workspace Scope 'vanished' is not a known Display ID or Alias")
        );
    }
}
