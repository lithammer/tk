//! Shared Item Status transition for `tk start` / `tk stop` / `tk done`.
//!
//! All three commands open the store, resolve a Display ID or Alias, attempt
//! a [`status::set_item_status`] write to a fixed target, and return the same
//! shape of success / not-found / locked-done outcomes. The [`Transition`]
//! and `success` parameters carry the only per-command variation; the
//! `tk <command>:` frame is supplied by the dispatch seam (ADR-0032).

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::domain::item_class::ItemClass;
use crate::domain::status::ItemStatus;
use crate::store::repository::status::{self, SetStatusError, SetStatusRequest};

/// Per-command success prefix tokens. `tk start` says "Started Ticket: …";
/// `tk stop` says "Stopped Ticket: …"; `tk done` says "Done Ticket: …".
/// Within one command the verb is shared across both classes; across
/// commands only the verb differs.
#[derive(Debug, Clone, Copy)]
pub struct SuccessLabel {
    pub ticket: &'static str,
    pub epic: &'static str,
}

impl SuccessLabel {
    fn select(self, class: ItemClass) -> &'static str {
        match class {
            ItemClass::Ticket => self.ticket,
            ItemClass::Epic => self.epic,
        }
    }
}

/// The Item Status transition one of `tk start` / `tk stop` / `tk done`
/// performs. One variant per command, so no caller can name a status apart
/// from the command asking for it, or pair one with a Closing Reason the
/// command has no way to supply — only [`Transition::Done`] carries one
/// (ADR-0023), since only `tk done -m` has one to give.
///
/// Reopen stays a runtime refusal rather than a type error: `tk stop` on a
/// `done` Item still asks for `Open`, and the store answers `LockedDone`
/// (ADR-0006).
#[derive(Debug, Clone, Copy)]
pub enum Transition<'a> {
    /// `tk start`: Open → Active.
    Start,
    /// `tk stop`: Active → Open.
    Stop,
    /// `tk done`: → Done, with an optional Closing Reason (ADR-0023).
    Done { closing_reason: Option<&'a str> },
}

impl<'a> Transition<'a> {
    /// The [`ItemStatus`] this variant asks the store to write.
    fn target(self) -> ItemStatus {
        match self {
            Self::Start => ItemStatus::Active,
            Self::Stop => ItemStatus::Open,
            Self::Done { .. } => ItemStatus::Done,
        }
    }

    /// The Closing Reason to persist, if any. Only [`Transition::Done`]
    /// carries one.
    fn closing_reason(self) -> Option<&'a str> {
        match self {
            Self::Done { closing_reason } => closing_reason,
            Self::Start | Self::Stop => None,
        }
    }
}

/// Run an Item Status transition. On failure returns the [`CommandError`] for
/// the dispatch seam to frame as `tk start:` / `tk stop:` / `tk done:`
/// (ADR-0032); `success` selects the per-command success phrasing.
pub fn transition(
    deps: &mut Deps<'_>,
    id: &str,
    change: Transition<'_>,
    success: SuccessLabel,
) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    let resolved = match resolver::resolve(&store, id) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "'{id}' is not a known Display ID or Alias"
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };

    match status::set_item_status(
        &mut store,
        deps.clock,
        SetStatusRequest {
            id: &resolved.id,
            status: change.target(),
            closing_reason: change.closing_reason(),
        },
    ) {
        Ok(item) => {
            let prefix = success.select(item.item_class);
            let _ = writeln!(deps.stdout, "{prefix}{} - {}", item.display_id, item.title);
            Ok(Exit::Ok)
        }
        // Race: row vanished between resolve and the BEGIN IMMEDIATE.
        Err(SetStatusError::NotFound) => Err(CommandError::failure(format!(
            "'{id}' is not a known Display ID or Alias"
        ))),
        Err(SetStatusError::LockedDone(class)) => Err(CommandError::failure(format!(
            "{label} '{id}' is done and cannot be reopened",
            label = class.label()
        ))),
        // Set-once (ADR-0023): re-closing is not an amend path.
        Err(SetStatusError::AlreadyClosed(_)) => Err(CommandError::failure(format!(
            "'{id}' is already done; closing reason not changed"
        ))),
        Err(SetStatusError::TriageNotStartable) => Err(CommandError::failure(format!(
            "'{id}' is in triage; accept it first with \
             'tk accept {id} --priority P0..P4'"
        ))),
        Err(SetStatusError::ParkedNotStartable) => Err(CommandError::failure(format!(
            "'{id}' is parked; unpark it first with 'tk unpark {id}'"
        ))),
        Err(SetStatusError::Sqlite(err)) => Err(resolver::storage_error(&err)),
        Err(SetStatusError::BackendBinding(err)) => Err(resolver::backend_binding_error(&err)),
        Err(SetStatusError::Mutation(err)) => Err(CommandError::failure(format!(
            "failed to append Mutation: {err}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{Harness, cwd, expect_git, seed_store, seed_store_at_version};
    use crate::store::testing::{FixtureItem, TmpStore, insert_fixture_item};
    use rusqlite::Connection;

    /// Drive `transition` and frame any returned error as the dispatch seam
    /// does (ADR-0032: `tk <command>: <body>`), so a test asserts the framed
    /// bytes. `command` is the subcommand name the seam would supply.
    fn run_rendered(
        h: &mut Harness<'_>,
        command: &str,
        id: &str,
        change: Transition<'_>,
        success: SuccessLabel,
    ) -> Exit {
        let mut deps = h.deps();
        match transition(&mut deps, id, change, success) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, command);
                exit
            }
        }
    }

    const STARTED: SuccessLabel = SuccessLabel {
        ticket: "Started Ticket: ",
        epic: "Started Epic: ",
    };

    #[test]
    fn start_transitions_open_ticket_to_active() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, "start", "tk-1", Transition::Start, STARTED);
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Started Ticket: tk-1 - Subject"));
    }

    #[test]
    fn done_lock_refuses_to_reopen_a_done_ticket() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Done",
                status: "done",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, "start", "tk-1", Transition::Start, STARTED);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk start: Ticket 'tk-1' is done and cannot be reopened"));
    }

    const STOPPED: SuccessLabel = SuccessLabel {
        ticket: "Stopped Ticket: ",
        epic: "Stopped Epic: ",
    };

    const DONE: SuccessLabel = SuccessLabel {
        ticket: "Done Ticket: ",
        epic: "Done Epic: ",
    };

    #[test]
    fn done_with_reason_heals_a_behind_version_store_then_writes_closing_reason() {
        // End-to-end tk-110 regression: an upgraded binary opening a store
        // written before migration 3 (no `closing_reason` column) must heal
        // the schema on open so `tk done -m` succeeds rather than failing with
        // `no such column: closing_reason`.
        let store = TmpStore::new("repo");
        let conn = seed_store_at_version(&store, 2);
        // Seed a v2-shaped Ticket directly: at migration 2 the items table has
        // neither `closing_reason` (v3) nor `selection_state` (v4), so
        // `insert_fixture_item` — which now writes `selection_state` — cannot
        // be used. The heal on open backfills `selection_state` to `accepted`.
        // The items + display-resolver rows go in one transaction so the
        // deferred composite foreign key holds at COMMIT.
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute(
            "insert into items(\
                id, display_value, item_class, ticket_kind, priority, title, body, \
                origin, status, created_seq, created_at, updated_at\
             ) values ('t1', 'tk-1', 'ticket', 'task', 'P2', 'Subject', '', 'local', 'open', 1, \
                       '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')",
            [],
        )
        .unwrap();
        tx.execute(
            "insert into item_ids(value, source, item_id, created_at) \
             values ('tk-1', 'display', 't1', '2026-05-09T00:00:00.000Z')",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            "done",
            "tk-1",
            Transition::Done {
                closing_reason: Some("Fixed in PR #12"),
            },
            DONE,
        );
        assert_eq!(
            code,
            Exit::Ok,
            "stderr: {}",
            String::from_utf8_lossy(&h.stderr)
        );

        let conn = Connection::open(store.db_path()).unwrap();
        let stored: Option<String> = conn
            .query_row(
                "select closing_reason from items where id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored.as_deref(), Some("Fixed in PR #12"));
    }

    fn seed_ticket_with_selection(
        conn: &Connection,
        id: &str,
        display: &str,
        status: &str,
        selection: &str,
        priority: Option<&str>,
    ) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Subject",
                status,
                priority,
                selection_state: Some(selection),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn start_refuses_a_triage_ticket_pointing_at_accept() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket_with_selection(&conn, "t1", "tk-1", "open", "triage", None);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, "start", "tk-1", Transition::Start, STARTED);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains(
                "tk start: 'tk-1' is in triage; accept it first with 'tk accept tk-1 --priority P0..P4'"
            ),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn start_refuses_a_parked_ticket_pointing_at_unpark() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket_with_selection(&conn, "t1", "tk-1", "open", "parked", Some("P2"));
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, "start", "tk-1", Transition::Start, STARTED);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk start: 'tk-1' is parked; unpark it first with 'tk unpark tk-1'"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn unknown_id_renders_not_found_per_command() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, "stop", "tk-9999", Transition::Stop, STOPPED);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk stop: 'tk-9999' is not a known Display ID or Alias"));
    }
}
