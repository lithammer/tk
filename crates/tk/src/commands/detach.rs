//! `tk detach` — remove a concrete Backend Binding while keeping the same
//! Item as local work (ADR-0047).

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::domain::mutation_state::MutationState;
use crate::store::promotion;
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
        "Detached: Backend {} {} → Local {} {}",
        report.item_class.label(),
        report.backend_display_id,
        report.item_class.label(),
        report.local_display_id,
    );
    let _ = writeln!(
        deps.stdout,
        "Backend object left unchanged: {}",
        report.backend_key
    );
    // Lost Backend intent is never a count: the user chose Detach, not the
    // withdrawal of these Mutations (ADR-0047).
    for mutation in &report.withdrawn {
        let _ = writeln!(
            deps.stdout,
            "Withdrew {} for {} (Mutation {})",
            mutation.mutation_type, mutation.target_display_id, mutation.sequence
        );
    }
    Ok(Exit::Ok)
}

/// The exits an unresolved Promotion's state actually offers.
///
/// Only an indeterminate creation may be retried, and ordinary sync still
/// carries a pending or failed Promotion, so naming all three recovery verbs
/// everywhere would recommend a command that refuses (ADR-0037).
fn promotion_remedy(promotion: &promotion::MutationSummary) -> String {
    let target = &promotion.target_display_id;
    match promotion.state {
        MutationState::Applying => format!(
            "Its Backend creation outcome was never observed: use 'tk promote reconcile {target} <backend-key>' if the Backend object exists, \
             'tk promote retry {target}' only when creating it again is safe, or 'tk promote cancel {target}' to withdraw the Promotion Operation, \
             leaving any object it created untracked. Then detach again."
        ),
        // A terminal Promotion never reaches here: only nonterminal rows are
        // unresolved. Exhaustive so a Mutation state added later has to say
        // which remedy it offers.
        MutationState::Pending
        | MutationState::Failed
        | MutationState::Applied
        | MutationState::Skipped
        | MutationState::Cancelled
        | MutationState::Abandoned => format!(
            "Run 'tk sync' to let Mutation {} resolve, 'tk promote reconcile {target} <backend-key>' if the Backend object already exists, \
             or 'tk promote cancel {target}' to withdraw the Promotion Operation. Then detach again.",
            promotion.sequence
        ),
    }
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
        detach::DetachError::AmbiguousDisplayProvenance => CommandError::failure(format!(
            "'{id}' has ambiguous local Display ID provenance from multiple legacy Aliases; Detach cannot choose one"
        )),
        detach::DetachError::UnresolvedPromotionOperation {
            sequence,
            mutation_type,
            promotion,
        } => CommandError::failure(format!(
            "cannot detach '{id}': Mutation {sequence} ({mutation_type}) belongs to the Promotion Operation of {}, \
             whose Promotion is unresolved.\n{}",
            promotion.target_display_id,
            promotion_remedy(&promotion)
        )),
        detach::DetachError::BackendBlockedByDetached {
            display_id,
            item_class,
            detached_item_class,
        } => CommandError::failure(format!(
            "cannot detach '{id}': Backend {} '{display_id}' would remain blocked by a Local {}; remove the Dependency first",
            item_class.label(),
            detached_item_class.label()
        )),
        detach::DetachError::DisplayPrefixMissing => CommandError::failure(
            "Repository Store is missing the display_prefix seed (run 'tk init')",
        ),
        detach::DetachError::BackendBinding(err) => resolver::backend_binding_error(&err),
        err @ (detach::DetachError::InvalidDisplayProvenance(_)
        | detach::DetachError::Transition(_)
        | detach::DetachError::Sequence(_)) => {
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
    use crate::domain::backend_operation::BackendItemIdentity;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, TmpStore, apply_promotion_receipt,
        commit_promotion, insert_dependency, insert_external_blocker, insert_fixture_item,
        insert_fixture_mutation, insert_fixture_remote,
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
        insert_fixture_item(&conn, local_epic("parent", "repo-9", "Parent Epic", 1)).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                body: "Details",
                status: "active",
                selection_state: Some("accepted"),
                container_id: Some("parent"),
                created_at: "2026-05-01T00:00:00.000Z",
                updated_at: "2026-05-02T00:00:00.000Z",
                ..backend_ticket(
                    "target",
                    "gh-42",
                    "Keep this work",
                    "https://github.com/o/r/issues/42",
                    2,
                )
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
                status: "done",
                ..backend_ticket(
                    "target",
                    "gh-7",
                    "Finished work",
                    "https://github.com/o/r/issues/7",
                    1,
                )
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
    fn detaching_a_promoted_ticket_restores_its_exact_local_display_id() {
        let store = TmpStore::new("repo");
        let mut conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "target",
                display: "tk-17",
                title: "Promoted work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        apply_promotion_receipt(
            &mut conn,
            "target",
            "github",
            &BackendItemIdentity {
                backend_key: "https://github.com/o/r/issues/53".into(),
                display_id: "gh-53".into(),
            },
            "2026-05-08T00:00:00.000Z",
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        assert_eq!(run_rendered(&mut h, "gh-53"), Exit::Ok, "{}", h.err());
        assert_eq!(
            h.out(),
            "Detached: Backend Ticket gh-53 → Local Ticket tk-17\n\
             Backend object left unchanged: https://github.com/o/r/issues/53\n"
        );
        let resolvers = conn
            .prepare("select value, source from item_ids where item_id = 'target' order by value")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(resolvers, vec![("tk-17".into(), "display".into())]);
    }

    #[test]
    fn detaching_a_promoted_epic_preserves_class_state_and_relationships() {
        let store = TmpStore::new("repo");
        let mut conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                body: "Epic details",
                status: "active",
                ..local_epic("target", "tk-4", "Promoted Epic", 1)
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "child",
                display: "tk-5",
                title: "Member Ticket",
                container_id: Some("target"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocker",
                display: "tk-6",
                title: "Blocking Item",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "blocker", "target").unwrap();
        insert_external_blocker(&conn, "external", "target", None).unwrap();
        apply_promotion_receipt(
            &mut conn,
            "target",
            "github",
            &BackendItemIdentity {
                backend_key: "https://github.com/o/r/issues/9".into(),
                display_id: "gh-9".into(),
            },
            "2026-05-08T00:00:00.000Z",
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        assert_eq!(run_rendered(&mut h, "gh-9"), Exit::Ok, "{}", h.err());
        assert_eq!(
            h.out(),
            "Detached: Backend Epic gh-9 → Local Epic tk-4\n\
             Backend object left unchanged: https://github.com/o/r/issues/9\n"
        );
        let state: (
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "select display_value, item_class, title, body, status, work_state, \
                        selection_state, origin \
                   from items where id = 'target'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            state,
            (
                "tk-4".into(),
                "epic".into(),
                "Promoted Epic".into(),
                "Epic details".into(),
                "open".into(),
                "active".into(),
                None,
                "local".into(),
            )
        );
        let relationships: (i64, i64, i64) = conn
            .query_row(
                "select \
                    (select count(*) from items where container_id = 'target'), \
                    (select count(*) from dependencies where blocked_id = 'target'), \
                    (select count(*) from external_blockers where item_id = 'target')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(relationships, (1, 1, 1));
    }

    #[test]
    fn detach_refuses_ambiguous_legacy_alias_provenance() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            backend_ticket(
                "target",
                "gh-42",
                "Legacy promoted work",
                "https://github.com/o/r/issues/42",
                1,
            ),
        )
        .unwrap();
        crate::store::testing::insert_alias(&conn, "tk-1", "target").unwrap();
        crate::store::testing::insert_alias(&conn, "old-1", "target").unwrap();
        conn.execute(
            "update items set binding_display_provenance = 'ambiguous' where id = 'target'",
            [],
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "target",
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
            "tk detach: 'gh-42' has ambiguous local Display ID provenance from multiple legacy Aliases; Detach cannot choose one\n"
        );
        let origin: String = conn
            .query_row("select origin from items where id = 'target'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(origin, "backend");
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
            backend_ticket(
                "target",
                "gh-42",
                "Still backend-bound",
                "https://github.com/o/r/issues/42",
                1,
            ),
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

    #[derive(Debug, PartialEq, Eq)]
    struct MutationRow {
        sequence: i64,
        state: String,
        failure_json: Option<String>,
        state_changed_at: String,
    }

    fn mutation_states(conn: &rusqlite::Connection) -> Vec<String> {
        mutation_rows(conn)
            .into_iter()
            .map(|row| row.state)
            .collect()
    }

    fn mutation_rows(conn: &rusqlite::Connection) -> Vec<MutationRow> {
        conn.prepare(
            "select sequence, state, failure_json, state_changed_at \
               from mutations order by sequence",
        )
        .unwrap()
        .query_map([], |row| {
            Ok(MutationRow {
                sequence: row.get(0)?,
                state: row.get(1)?,
                failure_json: row.get(2)?,
                state_changed_at: row.get(3)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    }

    /// A Local Epic fixture, leaving the Ticket-only columns null as the
    /// schema requires.
    fn local_epic<'a>(
        id: &'a str,
        display: &'a str,
        title: &'a str,
        created_seq: i64,
    ) -> FixtureItem<'a> {
        FixtureItem {
            id,
            display,
            item_class: "epic",
            ticket_kind: None,
            priority: None,
            title,
            created_seq,
            ..FixtureItem::default()
        }
    }

    /// A Backend Ticket fixture, keeping `origin` and the identity columns in
    /// the agreement the schema requires.
    fn backend_ticket<'a>(
        id: &'a str,
        display: &'a str,
        title: &'a str,
        backend_key: &'a str,
        created_seq: i64,
    ) -> FixtureItem<'a> {
        FixtureItem {
            id,
            display,
            title,
            origin: "backend",
            backend_kind: Some("github"),
            backend_key: Some(backend_key),
            created_seq,
            ..FixtureItem::default()
        }
    }

    #[test]
    fn detach_withdraws_target_and_counterpart_mutations_and_keeps_terminal_history() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            backend_ticket(
                "target",
                "gh-42",
                "Backend work",
                "https://github.com/o/r/issues/42",
                1,
            ),
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            backend_ticket(
                "blocked",
                "gh-43",
                "Backend work",
                "https://github.com/o/r/issues/43",
                2,
            ),
        )
        .unwrap();
        for mutation in [
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "target",
                state: "applied",
                ..FixtureMutation::default()
            },
            FixtureMutation {
                sequence: 2,
                mutation_type: "update_ticket",
                item_id: "target",
                state: "pending",
                ..FixtureMutation::default()
            },
            FixtureMutation {
                sequence: 3,
                mutation_type: "set_item_status",
                item_id: "target",
                state: "failed",
                failure_json: Some(r#"{"detail":"HTTP 422: rejected"}"#),
                ..FixtureMutation::default()
            },
            // The local edge is already gone, which is why the Mutation exists:
            // the withdrawal loses the intent to remove it upstream.
            FixtureMutation {
                sequence: 4,
                mutation_type: "remove_dependency",
                item_id: "blocked",
                payload_json: r#"{"blocking_id":"target"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
            FixtureMutation {
                sequence: 5,
                mutation_type: "set_item_status",
                item_id: "target",
                state: "skipped",
                failure_json: Some(r#"{"detail":"backend said no"}"#),
                ..FixtureMutation::default()
            },
            FixtureMutation {
                sequence: 6,
                mutation_type: "update_ticket",
                item_id: "target",
                state: "cancelled",
                ..FixtureMutation::default()
            },
        ] {
            insert_fixture_mutation(&conn, mutation).unwrap();
        }
        let before = mutation_rows(&conn);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        assert_eq!(run_rendered(&mut h, "gh-42"), Exit::Ok, "{}", h.err());
        assert_eq!(
            h.out(),
            "Detached: Backend Ticket gh-42 → Local Ticket tk-1\n\
             Backend object left unchanged: https://github.com/o/r/issues/42\n\
             Withdrew update_ticket for tk-1 (Mutation 2)\n\
             Withdrew set_item_status for tk-1 (Mutation 3)\n\
             Withdrew remove_dependency for gh-43 (Mutation 4)\n"
        );
        let after = mutation_rows(&conn);
        assert_eq!(
            mutation_states(&conn),
            [
                "applied",
                "cancelled",
                "cancelled",
                "cancelled",
                "skipped",
                "cancelled"
            ]
        );
        // Withdrawal keeps the Mutation Failure that explains the rejection.
        assert_eq!(
            after[2].failure_json.as_deref(),
            Some(r#"{"detail":"HTTP 422: rejected"}"#)
        );
        for sequence in [1, 5, 6] {
            let index = sequence - 1;
            assert_eq!(
                after[index], before[index],
                "terminal Mutation {sequence} must survive Detach untouched"
            );
        }
    }

    #[test]
    fn detach_withdraws_membership_intent_once_its_promotion_operation_resolved() {
        let store = TmpStore::new("repo");
        let mut conn = seed_store(&store);
        insert_fixture_item(&conn, local_epic("target", "tk-4", "Promoted Epic", 1)).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "member",
                container_id: Some("target"),
                ..backend_ticket(
                    "member",
                    "gh-10",
                    "Backend work",
                    "https://github.com/o/r/issues/10",
                    2,
                )
            },
        )
        .unwrap();
        apply_promotion_receipt(
            &mut conn,
            "target",
            "github",
            &BackendItemIdentity {
                backend_key: "https://github.com/o/r/issues/9".into(),
                display_id: "gh-9".into(),
            },
            "2026-05-08T00:00:00.000Z",
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_epic",
                item_id: "target",
                item_class: "epic",
                state: "applied",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        // A withdrawn child Promotion resolves the operation as surely as an
        // applied one: neither leaves a prospective identity to split.
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "promote_ticket",
                item_id: "member",
                state: "cancelled",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 3,
                mutation_type: "add_ticket_to_epic",
                item_id: "member",
                payload_json: r#"{"epic_id":"target"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"sub-issues unavailable"}"#),
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        assert_eq!(run_rendered(&mut h, "gh-9"), Exit::Ok, "{}", h.err());
        assert_eq!(
            h.out(),
            "Detached: Backend Epic gh-9 → Local Epic tk-4\n\
             Backend object left unchanged: https://github.com/o/r/issues/9\n\
             Withdrew add_ticket_to_epic for gh-10 (Mutation 3)\n"
        );
        let rows = mutation_rows(&conn);
        assert_eq!(
            mutation_states(&conn),
            ["applied", "cancelled", "cancelled"]
        );
        assert_eq!(
            rows[2].failure_json.as_deref(),
            Some(r#"{"detail":"sub-issues unavailable"}"#)
        );
        // Epic Membership stays local current state (ADR-0035).
        let container: Option<String> = conn
            .query_row(
                "select container_id from items where id = 'member'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(container.as_deref(), Some("target"));
    }

    /// A Promotion in the same operation as an affected Mutation blocks Detach,
    /// and the remedy depends on its state: only an indeterminate creation may
    /// be retried (ADR-0037).
    #[test]
    fn detach_refuses_to_split_an_operation_whose_promotion_is_unresolved() {
        for (state, remedy) in [
            (
                "pending",
                "Run 'tk sync' to let Mutation 1 resolve, 'tk promote reconcile repo-1 <backend-key>' if the Backend object already exists, \
                 or 'tk promote cancel repo-1' to withdraw the Promotion Operation. Then detach again.",
            ),
            (
                "applying",
                "Its Backend creation outcome was never observed: use 'tk promote reconcile repo-1 <backend-key>' if the Backend object exists, \
                 'tk promote retry repo-1' only when creating it again is safe, or 'tk promote cancel repo-1' to withdraw the Promotion Operation, \
                 leaving any object it created untracked. Then detach again.",
            ),
        ] {
            let store = TmpStore::new("repo");
            let conn = seed_store(&store);
            insert_fixture_item(
                &conn,
                FixtureItem {
                    origin: "backend",
                    backend_kind: Some("github"),
                    backend_key: Some("https://github.com/o/r/issues/9"),
                    ..local_epic("target", "gh-9", "Backend Epic", 1)
                },
            )
            .unwrap();
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id: "child",
                    display: "repo-1",
                    title: "Awaiting creation",
                    container_id: Some("target"),
                    created_seq: 2,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence: 1,
                    mutation_type: "promote_ticket",
                    item_id: "child",
                    state,
                    promotion_operation_id: Some("op-1"),
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence: 2,
                    mutation_type: "add_ticket_to_epic",
                    item_id: "child",
                    payload_json: r#"{"epic_id":"target"}"#,
                    state: "pending",
                    promotion_operation_id: Some("op-1"),
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
            let cwd_path = cwd();
            let mut h = Harness::new(&cwd_path);
            expect_git(&h, &store);

            assert_eq!(run_rendered(&mut h, "gh-9"), Exit::Failure);
            assert_eq!(
                h.err(),
                format!(
                    "tk detach: cannot detach 'gh-9': Mutation 2 (add_ticket_to_epic) belongs to the Promotion Operation of repo-1, \
                     whose Promotion is unresolved.\n{remedy}\n"
                ),
                "Promotion state {state} must name a remedy its state allows"
            );
            let origin: String = conn
                .query_row("select origin from items where id = 'target'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(origin, "backend");
            assert_eq!(mutation_states(&conn), [state, "pending"]);
        }
    }

    #[test]
    fn an_unrelated_applying_promotion_does_not_block_detach() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            backend_ticket(
                "target",
                "gh-42",
                "Backend work",
                "https://github.com/o/r/issues/42",
                1,
            ),
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "other",
                display: "repo-1",
                title: "Creation outcome unknown",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "other",
                state: "applying",
                failure_json: Some(r#"{"detail":"timed out"}"#),
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "update_ticket",
                item_id: "target",
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        assert_eq!(run_rendered(&mut h, "gh-42"), Exit::Ok, "{}", h.err());
        assert_eq!(
            h.out(),
            "Detached: Backend Ticket gh-42 → Local Ticket tk-1\n\
             Backend object left unchanged: https://github.com/o/r/issues/42\n\
             Withdrew update_ticket for tk-1 (Mutation 2)\n"
        );
        assert_eq!(mutation_states(&conn), ["applying", "cancelled"]);
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
                backend_ticket(id, display, "Backend work", key, created_seq),
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
