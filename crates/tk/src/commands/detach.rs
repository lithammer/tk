//! `tk detach` — remove a concrete Backend Binding while keeping the same
//! Item as local work (ADR-0047).

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::store::repository::detach;

/// Flags for `tk detach`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Backend Item to detach.
    #[arg(value_name = "ID")]
    pub id: String,
}

/// Run `tk detach <id>` without opening a Backend Adapter.
pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let _workflow = store
        .lock_remote_workflow()
        .map_err(CommandError::failure)?;
    let report = detach::detach(&mut store, &args.id, &deps.clock.now_iso())
        .map_err(|err| detach_error(err, &args.id))?;
    let _ = writeln!(
        deps.stdout,
        "Detached: Backend Ticket {} → Local Ticket {}",
        report.backend_display_id, report.local_display_id,
    );
    let _ = writeln!(
        deps.stdout,
        "Backend object left unchanged: {}",
        report.backend_key
    );
    Ok(Exit::Ok)
}

fn detach_error(err: detach::DetachError, id: &str) -> CommandError {
    match err {
        detach::DetachError::NotFound => {
            CommandError::failure(format!("'{id}' is not a known Display ID or Alias"))
        }
        detach::DetachError::Local => CommandError::failure(format!(
            "'{id}' is already a Local Item; only Backend Items can be detached"
        )),
        detach::DetachError::PendingPromotion => CommandError::failure(format!(
            "'{id}' is a Pending Promotion; use 'tk promote cancel {id}' instead"
        )),
        detach::DetachError::UnsupportedHistory => {
            CommandError::failure(format!("'{id}' is not a clean adopted Backend Ticket"))
        }
        detach::DetachError::UnresolvedMutations => CommandError::failure(format!(
            "'{id}' has unresolved Mutations; resolve them before detaching"
        )),
        detach::DetachError::BackendBlockedByDetached {
            display_id,
            item_class,
        } => CommandError::failure(format!(
            "cannot detach '{id}': Backend {} '{display_id}' would remain blocked by a Local Ticket; remove the Dependency first",
            item_class.label()
        )),
        detach::DetachError::DisplayPrefixMissing => CommandError::failure(
            "Repository Store is missing the display_prefix seed (run 'tk init')",
        ),
        detach::DetachError::BackendBinding(err) => resolver::backend_binding_error(&err),
        detach::DetachError::Sequence(err) => {
            CommandError::failure(format!("Repository Store corruption: {err}"))
        }
        detach::DetachError::Storage(err) => resolver::storage_error(&err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::show;
    use crate::commands::sync as sync_command;
    use crate::commands::testing::{Harness, cwd, expect_git, seed_store};
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, TmpStore, commit_promotion, insert_dependency,
        insert_external_blocker, insert_fixture_item, insert_fixture_mutation,
        insert_fixture_remote,
    };

    #[derive(Debug, PartialEq, Eq)]
    struct StoredState {
        id: String,
        display_id: String,
        item_class: String,
        ticket_kind: String,
        title: String,
        body: String,
        lifecycle: String,
        closing_reason: Option<String>,
        priority: Option<String>,
        selection_state: String,
        work_state: String,
        updated_at: String,
        created_seq: i64,
        created_at: String,
        container_id: Option<String>,
    }

    fn run_rendered(h: &mut Harness<'_>, id: &str) -> Exit {
        let mut deps = h.deps();
        match run(&mut deps, Args { id: id.into() }) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "detach");
                exit
            }
        }
    }

    fn run_show_rendered(h: &mut Harness<'_>, id: &str) -> Exit {
        let mut deps = h.deps();
        match show::run(&mut deps, show::Args { id: id.into() }) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "show");
                exit
            }
        }
    }

    #[test]
    fn detaches_a_clean_adopted_ticket_without_a_remote_or_backend_call() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "parent",
                display: "repo-9",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Parent Epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "target",
                display: "gh-42",
                title: "Keep this work",
                body: "Details",
                status: "active",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/42"),
                selection_state: Some("accepted"),
                container_id: Some("parent"),
                created_seq: 2,
                created_at: "2026-05-01T00:00:00.000Z",
                updated_at: "2026-05-02T00:00:00.000Z",
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocker",
                display: "repo-8",
                title: "Blocking work",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "blocker", "target").unwrap();
        insert_external_blocker(&conn, "external", "target", None).unwrap();

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let exit = run_rendered(&mut h, "gh-42");

        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        h.runner.assert_all_consumed();
        assert_eq!(
            h.out(),
            "Detached: Backend Ticket gh-42 → Local Ticket tk-1\n\
             Backend object left unchanged: https://github.com/o/r/issues/42\n"
        );
        let row = conn
            .query_row(
                "select id, display_value, item_class, ticket_kind, title, body, status, \
                        closing_reason, priority, selection_state, work_state, updated_at, \
                        created_seq, created_at, container_id \
                   from items where id = 'target'",
                [],
                |row| {
                    Ok(StoredState {
                        id: row.get(0)?,
                        display_id: row.get(1)?,
                        item_class: row.get(2)?,
                        ticket_kind: row.get(3)?,
                        title: row.get(4)?,
                        body: row.get(5)?,
                        lifecycle: row.get(6)?,
                        closing_reason: row.get(7)?,
                        priority: row.get(8)?,
                        selection_state: row.get(9)?,
                        work_state: row.get(10)?,
                        updated_at: row.get(11)?,
                        created_seq: row.get(12)?,
                        created_at: row.get(13)?,
                        container_id: row.get(14)?,
                    })
                },
            )
            .unwrap();
        assert_eq!(
            row,
            StoredState {
                id: "target".into(),
                display_id: "tk-1".into(),
                item_class: "ticket".into(),
                ticket_kind: "task".into(),
                title: "Keep this work".into(),
                body: "Details".into(),
                lifecycle: "open".into(),
                closing_reason: None,
                priority: Some("P2".into()),
                selection_state: "accepted".into(),
                work_state: "active".into(),
                updated_at: "2026-05-09T00:00:00.000Z".into(),
                created_seq: 2,
                created_at: "2026-05-01T00:00:00.000Z".into(),
                container_id: Some("parent".into()),
            }
        );
        let binding: (String, Option<String>, Option<String>) = conn
            .query_row(
                "select origin, backend_kind, backend_key from items where id = 'target'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(binding, ("local".into(), None, None));
        let former: (String, String, String) = conn
            .query_row(
                "select backend_kind, backend_key, backend_display_value \
                   from former_backend_identities where item_id = 'target'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            former,
            (
                "github".into(),
                "https://github.com/o/r/issues/42".into(),
                "gh-42".into(),
            )
        );
        let old_resolves: i64 = conn
            .query_row(
                "select count(*) from item_ids where value = 'gh-42'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new_resolves: String = conn
            .query_row(
                "select item_id from item_ids where value = 'tk-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_resolves, 0);
        assert_eq!(new_resolves, "target");
        let dependencies: i64 = conn
            .query_row(
                "select count(*) from dependencies where blocked_id = 'target'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let blockers: i64 = conn
            .query_row(
                "select count(*) from external_blockers where item_id = 'target'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!((dependencies, blockers), (1, 1));

        let mut show_h = Harness::new(&cwd_path);
        expect_git(&show_h, &store);
        assert_eq!(run_show_rendered(&mut show_h, "tk-1"), Exit::Ok);
        assert!(
            show_h.out().contains(
                "FORMER BACKEND IDENTITIES\n  • github https://github.com/o/r/issues/42\n"
            ),
            "stdout={}",
            show_h.out()
        );

        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        let mut sync_h = Harness::new(&cwd_path);
        expect_git(&sync_h, &store);
        let sync_exit = sync_command::run(
            sync_h.deps(),
            sync_command::Args {
                subcommand: None,
                skip: None,
            },
        );
        assert_eq!(sync_exit, Exit::Ok, "stderr={}", sync_h.err());
        assert_eq!(sync_h.out(), "Sync complete: 0 pulled, 0 applied.\n");
        sync_h.runner.assert_all_consumed();
    }

    #[test]
    fn detaches_a_done_ticket_without_changing_its_closing_state() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "target",
                display: "gh-7",
                title: "Finished work",
                status: "done",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/7"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        conn.execute(
            "update items set closing_reason = 'Already shipped' where id = 'target'",
            [],
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        assert_eq!(run_rendered(&mut h, "gh-7"), Exit::Ok);
        let state: (String, String, String) = conn
            .query_row(
                "select status, work_state, closing_reason from items where id = 'target'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            state,
            ("done".into(), "idle".into(), "Already shipped".into())
        );
    }

    #[test]
    fn local_item_and_pending_promotion_have_distinct_guidance() {
        let store = TmpStore::new("repo");
        let mut conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "local",
                display: "tk-1",
                title: "Local work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "pending",
                display: "tk-2",
                title: "Pending work",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        commit_promotion(&mut conn, "pending");
        let cwd_path = cwd();

        let mut local_h = Harness::new(&cwd_path);
        expect_git(&local_h, &store);
        assert_eq!(run_rendered(&mut local_h, "tk-1"), Exit::Failure);
        assert_eq!(
            local_h.err(),
            "tk detach: 'tk-1' is already a Local Item; only Backend Items can be detached\n"
        );

        let mut pending_h = Harness::new(&cwd_path);
        expect_git(&pending_h, &store);
        assert_eq!(run_rendered(&mut pending_h, "tk-2"), Exit::Failure);
        assert_eq!(
            pending_h.err(),
            "tk detach: 'tk-2' is a Pending Promotion; use 'tk promote cancel tk-2' instead\n"
        );
    }

    #[test]
    fn detach_takes_the_remote_workflow_lock_before_changing_the_store() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "target",
                display: "gh-42",
                title: "Still backend-bound",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/42"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(store.tk_dir().join("remote.lock"))
            .unwrap();
        lock_file.lock().unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        assert_eq!(run_rendered(&mut h, "gh-42"), Exit::Failure);
        assert_eq!(
            h.err(),
            "tk detach: another remote-changing command is running; retry when it finishes\n"
        );
        let origin: String = conn
            .query_row("select origin from items where id = 'target'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(origin, "backend");
    }

    #[test]
    fn detach_refuses_a_mutation_that_addresses_the_ticket_as_counterpart() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "target",
                display: "gh-42",
                title: "Blocking Ticket",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/42"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocked",
                display: "gh-43",
                title: "Blocked Ticket",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/43"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "add_dependency",
                item_id: "blocked",
                payload_json: r#"{"blocking_id":"target"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        assert_eq!(run_rendered(&mut h, "gh-42"), Exit::Failure);
        assert_eq!(
            h.err(),
            "tk detach: 'gh-42' has unresolved Mutations; resolve them before detaching\n"
        );
        let origin: String = conn
            .query_row("select origin from items where id = 'target'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(origin, "backend");
    }

    #[test]
    fn detach_refuses_to_leave_a_backend_ticket_blocked_by_a_local_ticket() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        for (id, display, key, created_seq) in [
            ("blocking", "gh-42", "https://github.com/o/r/issues/42", 1),
            ("blocked", "gh-43", "https://github.com/o/r/issues/43", 2),
        ] {
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id,
                    display,
                    title: "Backend work",
                    origin: "backend",
                    backend_kind: Some("github"),
                    backend_key: Some(key),
                    created_seq,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
        }
        insert_dependency(&conn, "blocking", "blocked").unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        assert_eq!(run_rendered(&mut h, "gh-42"), Exit::Failure);
        assert_eq!(
            h.err(),
            "tk detach: cannot detach 'gh-42': Backend Ticket 'gh-43' would remain blocked by a Local Ticket; remove the Dependency first\n"
        );
    }
}
