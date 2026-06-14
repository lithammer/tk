//! `tk block` — record that one item blocks another.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::store::repository::dependency::{self, AddDependencyError, DependencyEdge};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Item being blocked.
    #[arg(value_name = "BLOCKED")]
    pub blocked: String,
    /// Item that must finish first.
    #[arg(value_name = "BLOCKING")]
    pub blocking: String,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    let blocked = match resolver::resolve(&store, &args.blocked) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "blocked '{}' is not a known Display ID or Alias",
                args.blocked
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };
    let blocking = match resolver::resolve(&store, &args.blocking) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "blocking '{}' is not a known Display ID or Alias",
                args.blocking
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };

    if blocked.id == blocking.id {
        return Err(CommandError::failure("an item cannot block itself"));
    }

    match dependency::add_dependency(
        &mut store,
        deps.clock,
        DependencyEdge {
            blocked_id: &blocked.id,
            blocking_id: &blocking.id,
        },
    ) {
        Ok(()) => {
            let _ = writeln!(
                deps.stdout,
                "Blocked: {} blocked by {}",
                args.blocked, args.blocking
            );
            Ok(Exit::Ok)
        }
        Err(AddDependencyError::EndpointMissing) => {
            Err(CommandError::failure("endpoint missing in items table"))
        }
        Err(AddDependencyError::BlockedDone) => Err(CommandError::failure(format!(
            "blocked '{}' is done",
            args.blocked
        ))),
        Err(AddDependencyError::BlockingDone) => Err(CommandError::failure(format!(
            "blocking '{}' is done",
            args.blocking
        ))),
        Err(AddDependencyError::Cycle) => Err(CommandError::failure("dependency cycle")),
        Err(AddDependencyError::BackendBlockedLocalBlocking) => {
            Err(CommandError::failure(format!(
                "Backend blocked '{}' cannot depend on Local blocking item '{}'",
                args.blocked, args.blocking
            )))
        }
        Err(AddDependencyError::BackendKindMismatch) => Err(CommandError::failure(format!(
            "Backend blocked '{}' cannot depend on blocking item '{}' from another Backend kind",
            args.blocked, args.blocking
        ))),
        Err(AddDependencyError::Sqlite(err)) => Err(resolver::storage_error(&err)),
        Err(AddDependencyError::Mutation(err)) => Err(CommandError::failure(format!(
            "failed to append Mutation: {err}"
        ))),
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
    /// (ADR-0032: `tk block: <body>`), so a test asserts the framed bytes.
    fn run_rendered(h: &mut Harness<'_>, args: Args) -> Exit {
        let mut deps = h.deps();
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "block");
                exit
            }
        }
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
        let code = run_rendered(
            &mut h,
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
        let code = run_rendered(
            &mut h,
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
