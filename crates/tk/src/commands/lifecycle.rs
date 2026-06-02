//! Shared lifecycle-transition implementation for `tk start` / `tk stop` /
//! `tk done`.
//!
//! All three commands open the store, resolve a Display ID or Alias,
//! attempt a [`set_item_status`] write to a fixed target, and render the
//! same shape of success / not-found / locked-done diagnostics. The
//! `command` and `target` parameters carry the only per-command
//! variation.

use crate::cli::{Deps, Exit};
use crate::commands::resolver;
use crate::domain::item_class::ItemClass;
use crate::domain::status::ItemStatus;
use crate::store::repository::status::{self, SetStatusError, SetStatusRequest};

/// Per-command success prefix tokens. `tk start` says "Started Ticket: …";
/// `tk stop` says "Stopped Ticket: …"; `tk done` says "Done Ticket: …".
/// The active-verb suffix is shared across both classes; only the verb
/// differs across commands.
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

/// Run a lifecycle transition against the supplied Deps and report the
/// outcome with per-command phrasing. `command` is the subcommand name
/// (`"start"` / `"stop"` / `"done"`) used in stderr diagnostics.
#[must_use]
pub fn transition(
    deps: Deps<'_>,
    command: &'static str,
    id: &str,
    target: ItemStatus,
    success: SuccessLabel,
    closing_reason: Option<&str>,
) -> Exit {
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
            resolver::render_open_error(stderr, command, &err);
            return Exit::Failure;
        }
    };

    let resolved = match resolver::resolve(&store, id) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            let _ = writeln!(
                stderr,
                "tk {command}: '{id}' is not a known Display ID or Alias"
            );
            return Exit::Failure;
        }
        Err(resolver::ResolveError::Storage(err)) => {
            resolver::render_storage_error(stderr, command, &err);
            return Exit::Failure;
        }
    };

    match status::set_item_status(
        &mut store,
        clock,
        SetStatusRequest {
            id: &resolved.id,
            status: target,
            closing_reason,
        },
    ) {
        Ok(item) => {
            let prefix = success.select(item.item_class);
            let _ = writeln!(stdout, "{prefix}{} - {}", item.display_id, item.title);
            Exit::Ok
        }
        Err(SetStatusError::NotFound) => {
            // Race: row vanished between resolve and the BEGIN IMMEDIATE.
            let _ = writeln!(
                stderr,
                "tk {command}: '{id}' is not a known Display ID or Alias"
            );
            Exit::Failure
        }
        Err(SetStatusError::LockedDone(class)) => {
            let _ = writeln!(
                stderr,
                "tk {command}: {label} '{id}' is done and cannot be reopened",
                label = class.label()
            );
            Exit::Failure
        }
        Err(SetStatusError::AlreadyClosed(_)) => {
            // Set-once (ADR-0023): re-closing is not an amend path. Only
            // `tk done -m` reaches this, so `{command}` is always "done".
            let _ = writeln!(
                stderr,
                "tk {command}: '{id}' is already done; closing reason not changed"
            );
            Exit::Failure
        }
        Err(SetStatusError::Sqlite(err)) => {
            resolver::render_storage_error(stderr, command, &err);
            Exit::Failure
        }
        Err(SetStatusError::Mutation(err)) => {
            let _ = writeln!(stderr, "tk {command}: failed to append Mutation: {err}");
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
        seed_store_at_version(store, migrations::MAX_KNOWN_VERSION)
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
        let code = transition(h.deps(), "start", "tk-1", ItemStatus::Active, STARTED, None);
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
        let code = transition(h.deps(), "start", "tk-1", ItemStatus::Active, STARTED, None);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk start: Ticket 'tk-1' is done and cannot be reopened"));
    }

    /// Seed a store frozen at `version`, omitting later migrations — stands in
    /// for an on-disk store written by an older `tk` binary.
    fn seed_store_at_version(store: &TmpStore, version: u32) -> Connection {
        std::fs::create_dir_all(store.tk_dir()).unwrap();
        let mut conn = Connection::open(store.db_path()).unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_through(&mut conn, version, "2026-05-09T00:00:00.000Z").unwrap();
        conn.execute(
            "insert into store_config(key, value) values ('display_prefix', 'tk')",
            [],
        )
        .unwrap();
        conn
    }

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
        let code = transition(
            h.deps(),
            "done",
            "tk-1",
            ItemStatus::Done,
            DONE,
            Some("Fixed in PR #12"),
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

    #[test]
    fn unknown_id_renders_not_found_per_command() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = transition(h.deps(), "stop", "tk-9999", ItemStatus::Open, STARTED, None);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk stop: 'tk-9999' is not a known Display ID or Alias"));
    }
}
