//! `tk block` — record that one item blocks another.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::dependency_edge;
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
    let (mut store, blocked, blocking) = dependency_edge::resolve(
        deps.runner,
        deps.cwd,
        deps.clock,
        &args.blocked,
        &args.blocking,
    )?;

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
        Err(AddDependencyError::BackendBinding(err)) => Err(resolver::backend_binding_error(&err)),
        Err(AddDependencyError::Mutation(err)) => Err(CommandError::failure(format!(
            "failed to append Mutation: {err}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{Harness, cwd, expect_git, seed_store};
    use crate::store::testing::{FixtureItem, TmpStore, insert_fixture_item};

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
