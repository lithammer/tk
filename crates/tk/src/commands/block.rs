//! `tk block` — record that one item blocks another.

use clap::Args as ClapArgs;

use crate::cli::{Deps, Exit};
use crate::commands::resolver;
use crate::store::repository::dependency::{self, AddDependencyError, DependencyEdge};

const COMMAND: &str = "block";

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Item being blocked.
    #[arg(value_name = "BLOCKED")]
    pub blocked: String,
    /// Item that must finish first.
    #[arg(value_name = "BLOCKING")]
    pub blocking: String,
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

    let mut store = match resolver::open_for_command(runner, cwd) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    let blocked = match resolver::resolve(&store, &args.blocked) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            let _ = writeln!(
                stderr,
                "tk block: blocked '{}' is not a known Display ID or Alias",
                args.blocked
            );
            return Exit::Failure;
        }
        Err(resolver::ResolveError::Storage(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };
    let blocking = match resolver::resolve(&store, &args.blocking) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            let _ = writeln!(
                stderr,
                "tk block: blocking '{}' is not a known Display ID or Alias",
                args.blocking
            );
            return Exit::Failure;
        }
        Err(resolver::ResolveError::Storage(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    if blocked.id == blocking.id {
        let _ = writeln!(stderr, "tk block: an item cannot block itself");
        return Exit::Failure;
    }

    match dependency::add_dependency(
        &mut store,
        clock,
        DependencyEdge {
            blocked_id: &blocked.id,
            blocking_id: &blocking.id,
        },
    ) {
        Ok(()) => {
            let _ = writeln!(
                stdout,
                "Blocked: {} blocked by {}",
                args.blocked, args.blocking
            );
            Exit::Ok
        }
        Err(AddDependencyError::EndpointMissing) => {
            let _ = writeln!(stderr, "tk block: endpoint missing in items table");
            Exit::Failure
        }
        Err(AddDependencyError::BlockedDone) => {
            let _ = writeln!(stderr, "tk block: blocked '{}' is done", args.blocked);
            Exit::Failure
        }
        Err(AddDependencyError::BlockingDone) => {
            let _ = writeln!(stderr, "tk block: blocking '{}' is done", args.blocking);
            Exit::Failure
        }
        Err(AddDependencyError::Cycle) => {
            let _ = writeln!(stderr, "tk block: dependency cycle");
            Exit::Failure
        }
        Err(AddDependencyError::BackendBlockedLocalBlocking) => {
            let _ = writeln!(
                stderr,
                "tk block: Backend blocked '{}' cannot depend on Local blocking item '{}'",
                args.blocked, args.blocking
            );
            Exit::Failure
        }
        Err(AddDependencyError::BackendKindMismatch) => {
            let _ = writeln!(
                stderr,
                "tk block: Backend blocked '{}' cannot depend on blocking item '{}' from another Backend kind",
                args.blocked, args.blocking
            );
            Exit::Failure
        }
        Err(AddDependencyError::Sqlite(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            Exit::Failure
        }
        Err(AddDependencyError::Mutation(err)) => {
            let _ = writeln!(stderr, "tk block: failed to append Mutation: {err}");
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

    #[test]
    fn block_inserts_dependency_and_renders_confirmation() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocker",
                display: "tk-1",
                title: "B",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocked",
                display: "tk-2",
                title: "C",
                created_seq: 2,
                ..FixtureItem::default()
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
                blocked: "tk-2".into(),
                blocking: "tk-1".into(),
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Blocked: tk-2 blocked by tk-1"));
    }

    #[test]
    fn block_self_dependency_is_refused() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "T",
                created_seq: 1,
                ..FixtureItem::default()
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
                blocked: "tk-1".into(),
                blocking: "tk-1".into(),
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk block: an item cannot block itself"));
    }
}
