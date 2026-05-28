//! Repository Store facade: open / resolve / list / next / show / create /
//! update / status / dependency operations.
//!
//! Each command surface lives in its own submodule and operates against the
//! shared [`Store`] handle. The split mirrors the operation taxonomy used
//! by CONTEXT.md (Repository Store §2) and keeps each per-operation SQL
//! batch grep-able in a small file rather than buried in a monolithic
//! module.
//!
//! Expected domain outcomes (item not found, scope misses, etc.) ride on
//! enum *return values* — `Result::Err` is reserved for genuine SQLite or
//! discovery faults that should surface as exit-code 3 (ADR-0017). This
//! split lets command handlers render the user-facing diagnostic for an
//! expected miss without having to discriminate against a generic `Err`.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use thiserror::Error;

use crate::domain::item_class::ItemClass;
use crate::domain::origin::Origin;
use crate::domain::priority::Priority;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::git::discovery;
use crate::proc::ProcRunner;
use crate::store::migrations;

pub mod create;
pub mod dependency;
pub mod list;
pub mod next;
pub mod show;
pub mod status;
pub mod update;

/// Handle to an opened Repository Store.
///
/// Owns the underlying SQLite [`Connection`]; dropping the `Store` closes the
/// connection. Operations take `&Store` (the read path) or `&mut Store` (the
/// write path) so callers can keep the handle while threading it through
/// borrow-checked operation calls.
pub struct Store {
    pub(crate) conn: Connection,
}

impl Store {
    /// Borrow the underlying SQLite connection.
    ///
    /// Exposed for tests and tightly-scoped helper modules (e.g. fixture
    /// inserts under `#[cfg(test)]`). Production command handlers should
    /// reach for the typed operation functions in this module's children
    /// rather than running ad-hoc SQL against the connection.
    #[must_use]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mutable borrow of the underlying connection. Required for
    /// `Connection::transaction` and for migrations.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

/// Why [`open_existing`] could not return an opened [`Store`].
///
/// One enum for every non-success path: the expected refusals (no store yet,
/// foreign file, future schema) that render at exit 1, the forwarded
/// discovery failure, and the genuine SQLite faults. The command layer picks
/// the exit code and phrasing per variant (see `resolver::render_open_error`);
/// each `#[error]` string is the stable user-facing line (ADR-0017), so an
/// arg-free renderer can print it directly.
#[derive(Debug, Error)]
pub enum OpenError {
    /// `git rev-parse` failed; the inner error's `Display` is the message.
    #[error(transparent)]
    DiscoveryFailed(#[from] discovery::DiscoveryError),
    /// `<git-common-dir>/tk/tk.db` does not exist — `tk init` has not run here.
    #[error("Repository Store not initialized; run 'tk init'")]
    StoreMissing,
    /// A SQLite file exists at the Repository Store path but its
    /// `application_id` is not tk's; refuse to touch a foreign database.
    #[error("Repository Store is not a tk Repository Store")]
    NotTicketStore,
    /// The store records a higher schema version than this binary knows.
    #[error("Repository Store was created by a newer tk version")]
    FromFutureVersion,
    /// Genuine SQLite fault opening or inspecting the store.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Open the Repository Store for the Git repository containing `cwd`.
///
/// The `git rev-parse` step locates `<git-common-dir>/tk/tk.db` so a worktree
/// shares the store with its main checkout. `application_id` and
/// `schema_migrations.version` are inspected before any pragma mutation so
/// foreign files are refused without rewriting their headers.
pub fn open_existing<R: ProcRunner + ?Sized>(runner: &R, cwd: &Path) -> Result<Store, OpenError> {
    // `?` converts a DiscoveryError into OpenError::DiscoveryFailed via #[from].
    let paths = discovery::discover_paths(runner, cwd)?;
    let db_path = paths.git_common_dir.join("tk").join("tk.db");

    if !db_path.exists() {
        return Err(OpenError::StoreMissing);
    }

    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("pragma foreign_keys = on")?;

    let app_id: i64 = conn
        .query_row("pragma application_id", [], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    if app_id != i64::from(migrations::APPLICATION_ID) {
        return Err(OpenError::NotTicketStore);
    }

    let version = migrations::current_version(&conn)?;
    if version > i64::from(migrations::MAX_KNOWN_VERSION) {
        return Err(OpenError::FromFutureVersion);
    }

    Ok(Store { conn })
}

/// A Display ID or Alias resolved to a stable internal item ID and class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedItemRef {
    pub id: String,
    pub item_class: ItemClass,
}

/// A resolved item plus its current Display ID, used when a command must
/// echo the current Display ID rather than the user-supplied resolver value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedItemRefWithDisplay {
    pub id: String,
    pub display_id: String,
    pub item_class: ItemClass,
}

/// Why resolving a Display ID or Alias that must be an Epic failed. Shared by
/// [`resolve_as_epic`] and [`resolve_as_epic_with_display`] and re-exported as
/// `resolver::ResolveEpicError`; the command picks the exit-1 phrasing per
/// variant. With only Ticket and Epic in v1, "not an Epic" means "is a
/// Ticket", so `NotAnEpic` carries no payload.
#[derive(Debug, Error)]
pub enum ResolveEpicError {
    #[error("Display ID or Alias not found")]
    NotFound,
    #[error("resolved Item is not an Epic")]
    NotAnEpic,
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
}

/// Resolve a Display ID or Alias to its stable internal `items.id`.
///
/// `item_ids.value` uses a case-insensitive collation so `TK-1` and `tk-1`
/// resolve to the same row. Returns `None` when no row matches.
pub fn resolve_item_ref(
    conn: &Connection,
    display_arg: &str,
) -> Result<Option<ResolvedItemRef>, rusqlite::Error> {
    conn.query_row(
        "select i.id, i.item_class \
           from item_ids ids \
           join items i on i.id = ids.item_id \
          where ids.value = ?1",
        params![display_arg],
        |row| {
            let id: String = row.get(0)?;
            let class_text: String = row.get(1)?;
            Ok(ResolvedItemRef {
                id,
                item_class: item_class_from_text(&class_text),
            })
        },
    )
    .optional()
}

/// Resolve a Display ID or Alias to its internal ID plus the current Display ID.
pub fn resolve_item_ref_with_display(
    conn: &Connection,
    display_arg: &str,
) -> Result<Option<ResolvedItemRefWithDisplay>, rusqlite::Error> {
    conn.query_row(
        "select i.id, i.display_value, i.item_class \
           from item_ids ids \
           join items i on i.id = ids.item_id \
          where ids.value = ?1",
        params![display_arg],
        |row| {
            let id: String = row.get(0)?;
            let display_id: String = row.get(1)?;
            let class_text: String = row.get(2)?;
            Ok(ResolvedItemRefWithDisplay {
                id,
                display_id,
                item_class: item_class_from_text(&class_text),
            })
        },
    )
    .optional()
}

/// Resolve a Display ID or Alias to an Epic reference, classifying the
/// outcome so callers can render `not_found` vs `not_an_epic` differently.
///
/// Use this for `--parent <epic-id>` validation so the deferred composite
/// foreign key on `items(container_id, container_class)` does not surface
/// as a raw constraint error when the user supplies a Ticket's Display ID.
pub fn resolve_as_epic(
    conn: &Connection,
    display_arg: &str,
) -> Result<ResolvedItemRef, ResolveEpicError> {
    // `?` converts a SQLite fault into ResolveEpicError::Storage via #[from].
    let Some(resolved) = resolve_item_ref(conn, display_arg)? else {
        return Err(ResolveEpicError::NotFound);
    };
    if resolved.item_class == ItemClass::Epic {
        Ok(resolved)
    } else {
        Err(ResolveEpicError::NotAnEpic)
    }
}

/// Like [`resolve_as_epic`] but with the current Display ID attached.
pub fn resolve_as_epic_with_display(
    conn: &Connection,
    display_arg: &str,
) -> Result<ResolvedItemRefWithDisplay, ResolveEpicError> {
    let Some(resolved) = resolve_item_ref_with_display(conn, display_arg)? else {
        return Err(ResolveEpicError::NotFound);
    };
    if resolved.item_class == ItemClass::Epic {
        Ok(resolved)
    } else {
        Err(ResolveEpicError::NotAnEpic)
    }
}

// ---- Text-column decoders ----------------------------------------------
//
// The `items.*` columns carry CHECK constraints that pin the set of legal
// spellings, so reading an unknown value is Repository Store corruption.
// The decoders panic rather than thread `Result` through every read site —
// surfacing a debug-mode panic in tests is more useful than silently
// returning a default value that would lie to downstream callers.
//
// `dead_code` is permitted at the umbrella scope because not every decoder
// is exercised until the matching leaf operation lands — keeping them all
// here matches the schema column set and lets the leaves grow without
// adding helpers piecemeal.

#[allow(dead_code)]
pub(crate) fn item_class_from_text(text: &str) -> ItemClass {
    match text {
        "ticket" => ItemClass::Ticket,
        "epic" => ItemClass::Epic,
        other => panic!("repository store corruption: unknown item_class `{other}`"),
    }
}

#[allow(dead_code)]
pub(crate) fn ticket_kind_from_text(text: &str) -> TicketKind {
    match text {
        "task" => TicketKind::Task,
        "bug" => TicketKind::Bug,
        other => panic!("repository store corruption: unknown ticket_kind `{other}`"),
    }
}

#[allow(dead_code)]
pub(crate) fn priority_from_text(text: &str) -> Priority {
    match text {
        "P0" => Priority::P0,
        "P1" => Priority::P1,
        "P2" => Priority::P2,
        "P3" => Priority::P3,
        "P4" => Priority::P4,
        other => panic!("repository store corruption: unknown priority `{other}`"),
    }
}

#[allow(dead_code)]
pub(crate) fn status_from_text(text: &str) -> ItemStatus {
    match text {
        "open" => ItemStatus::Open,
        "active" => ItemStatus::Active,
        "done" => ItemStatus::Done,
        other => panic!("repository store corruption: unknown status `{other}`"),
    }
}

#[allow(dead_code)]
pub(crate) fn origin_from_text(text: &str) -> Origin {
    match text {
        "local" => Origin::Local,
        "backend" => Origin::Backend,
        other => panic!("repository store corruption: unknown origin `{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::store::testing::{FixtureItem, TmpStore, insert_alias, insert_fixture_item};

    fn open_seeded() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn
    }

    #[test]
    fn resolve_item_ref_returns_none_for_unknown_value() {
        let conn = open_seeded();
        assert!(resolve_item_ref(&conn, "nothing-here").unwrap().is_none());
    }

    #[test]
    fn resolve_item_ref_finds_by_display_id() {
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let r = resolve_item_ref(&conn, "tk-1").unwrap().unwrap();
        assert_eq!(r.id, "t1");
        assert_eq!(r.item_class, ItemClass::Ticket);
    }

    #[test]
    fn resolve_item_ref_is_case_insensitive() {
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let r = resolve_item_ref(&conn, "TK-1").unwrap().unwrap();
        assert_eq!(r.id, "t1");
    }

    #[test]
    fn resolve_item_ref_finds_by_alias() {
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_alias(&conn, "my-alias", "t1").unwrap();
        let r = resolve_item_ref(&conn, "my-alias").unwrap().unwrap();
        assert_eq!(r.id, "t1");
    }

    #[test]
    fn resolve_item_ref_with_display_returns_canonical_display() {
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-42",
                title: "Ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_alias(&conn, "alias", "t1").unwrap();
        let r = resolve_item_ref_with_display(&conn, "alias")
            .unwrap()
            .unwrap();
        assert_eq!(r.display_id, "tk-42");
    }

    #[test]
    fn resolve_as_epic_returns_epic_for_an_epic_display_id() {
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "tk-1",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        assert_eq!(resolve_as_epic(&conn, "tk-1").unwrap().id, "e1");
    }

    #[test]
    fn resolve_as_epic_flags_a_ticket_resolution() {
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        assert!(matches!(
            resolve_as_epic(&conn, "tk-1"),
            Err(ResolveEpicError::NotAnEpic)
        ));
    }

    #[test]
    fn resolve_as_epic_returns_not_found_for_unknown_value() {
        let conn = open_seeded();
        assert!(matches!(
            resolve_as_epic(&conn, "missing"),
            Err(ResolveEpicError::NotFound)
        ));
    }

    // ---- open_existing ---------------------------------------------------
    //
    // Exercises the full discover → open → classify → version-check pipeline
    // against a tempfile database driven by a FakeRunner-replayed git
    // rev-parse. Each variant of `OpenError` plus the success path is
    // exercised here.

    fn cwd() -> std::path::PathBuf {
        std::env::current_dir().unwrap()
    }

    fn fake_runner_for(store: &TmpStore) -> FakeRunner {
        let runner = FakeRunner::new();
        runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
        runner
    }

    fn seed_tk_db(store: &TmpStore) {
        std::fs::create_dir_all(store.tk_dir()).unwrap();
        let mut conn = Connection::open(store.db_path()).unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
    }

    #[test]
    fn open_existing_reports_store_missing_when_db_file_does_not_exist() {
        let store = TmpStore::new("repo");
        let runner = fake_runner_for(&store);
        assert!(matches!(
            open_existing(&runner, &cwd()),
            Err(OpenError::StoreMissing)
        ));
    }

    #[test]
    fn open_existing_reports_not_ticket_store_when_application_id_mismatches() {
        let store = TmpStore::new("repo");
        std::fs::create_dir_all(store.tk_dir()).unwrap();
        // Plant a foreign SQLite file with no application_id at the
        // Repository Store path.
        let foreign = Connection::open(store.db_path()).unwrap();
        foreign
            .execute_batch("create table other_app(x integer)")
            .unwrap();
        drop(foreign);

        let runner = fake_runner_for(&store);
        assert!(matches!(
            open_existing(&runner, &cwd()),
            Err(OpenError::NotTicketStore)
        ));
    }

    #[test]
    fn open_existing_reports_from_future_version_when_schema_is_newer() {
        let store = TmpStore::new("repo");
        seed_tk_db(&store);
        // Stamp a synthetic schema_migrations row past MAX_KNOWN_VERSION.
        let conn = Connection::open(store.db_path()).unwrap();
        conn.execute(
            "insert into schema_migrations(version, applied_at) values (?1, ?2)",
            params![999_i64, "2099-01-01T00:00:00.000Z"],
        )
        .unwrap();
        drop(conn);

        let runner = fake_runner_for(&store);
        assert!(matches!(
            open_existing(&runner, &cwd()),
            Err(OpenError::FromFutureVersion)
        ));
    }

    #[test]
    fn open_existing_returns_ok_for_a_well_formed_repository_store() {
        let store = TmpStore::new("repo");
        seed_tk_db(&store);
        let runner = fake_runner_for(&store);
        let opened = match open_existing(&runner, &cwd()) {
            Ok(s) => s,
            Err(e) => panic!("expected Ok, got {e:?}"),
        };
        // Foreign keys pragma is connection-scoped; verify it survived the open.
        let fk: i64 = opened
            .conn()
            .query_row("pragma foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn open_existing_propagates_discovery_failure() {
        // Empty argv prefix matches any git invocation; return a non-zero
        // exit with the canonical "not a git repository" stderr.
        let runner = FakeRunner::new();
        runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 128,
                stdout: Vec::new(),
                stderr: b"fatal: not a git repository\n".to_vec(),
            },
        );
        assert!(matches!(
            open_existing(&runner, &cwd()),
            Err(OpenError::DiscoveryFailed(_))
        ));
    }
}
