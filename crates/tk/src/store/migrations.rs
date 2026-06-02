//! Repository Store schema migrations.
//!
//! The migration SQL is the durable artefact (ADR-0005); it is reused
//! *verbatim* via `include_str!` from the sibling `migrations/` directory so
//! there is a single source of truth.
//!
//! Each migration runs inside its own transaction. The caller is responsible
//! for connection-level setup (`foreign_keys`, `busy_timeout`, `journal_mode`)
//! before invoking [`apply_all`].

use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;

/// Application ID written to `pragma application_id` so an existing SQLite
/// file can be identified as a tk Repository Store. Spelled `TKDB` in
/// big-endian ASCII (`0x54 0x4B 0x44 0x42`).
pub const APPLICATION_ID: i32 = 0x544B_4442;

/// One schema migration in the ordered Repository Store migration list.
pub struct Migration {
    /// Monotonic schema version recorded in `schema_migrations` and mirrored
    /// to `pragma user_version`.
    pub version: u32,
    /// SQL batch executed inside the migration transaction.
    pub sql: &'static str,
}

// `include_str!` resolves paths relative to this source file; the SQL lives in
// the sibling `migrations/` directory. Pulling the SQL by reference keeps the
// "verbatim" promise of ADR-0017 / ADR-0018 mechanical instead of typographic.
// CRLF safety is enforced by `.gitattributes` (`*.sql text eol=lf`) so a
// Windows clone with `core.autocrlf=true` still checks the files out as LF.
const MIGRATION_1_SQL: &str = include_str!("migrations/001_repository_store.sql");
const MIGRATION_2_SQL: &str = include_str!("migrations/002_items_no_escape_from_done.sql");
const MIGRATION_3_SQL: &str = include_str!("migrations/003_closing_reason.sql");

/// V1 Repository Store schema skeleton.
pub const MIGRATION_1: Migration = Migration {
    version: 1,
    sql: MIGRATION_1_SQL,
};

/// Adds the `items_no_escape_from_done` trigger that enforces "Done is
/// terminal" (ADR-0006) at the schema layer.
pub const MIGRATION_2: Migration = Migration {
    version: 2,
    sql: MIGRATION_2_SQL,
};

/// Adds the nullable `closing_reason` Local Field (ADR-0023). The column
/// CHECK keeps a Closing Reason non-empty and confined to `done` items.
pub const MIGRATION_3: Migration = Migration {
    version: 3,
    sql: MIGRATION_3_SQL,
};

/// Ordered migration list applied by [`apply_all`].
pub const ALL_MIGRATIONS: &[Migration] = &[MIGRATION_1, MIGRATION_2, MIGRATION_3];

/// Highest schema version this binary can apply. Named so future migrations
/// surface the threshold to `grep` instead of hiding it behind `.last()`.
/// Adding `MIGRATION_4` is a two-line patch: append to `ALL_MIGRATIONS`, bump
/// this constant, with a debug_assert below catching the drift.
pub const MAX_KNOWN_VERSION: u32 = MIGRATION_3.version;

/// Errors returned while applying migrations.
///
/// `StoreFromFutureVersion` is the only "domain" arm; everything else is a
/// pass-through of the rusqlite error so command-side stderr can render the
/// SQLite errmsg verbatim.
#[derive(Debug, Error)]
pub enum ApplyError {
    /// Store records a higher schema version than this binary knows.
    #[error("store was created by a newer tk version")]
    StoreFromFutureVersion,
    /// Underlying SQLite or driver error from the migration transaction.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Apply every migration missing from the opened Repository Store.
///
/// `now_iso` is supplied by the caller's injectable clock and recorded in
/// `schema_migrations.applied_at`. Stores with a recorded version newer than
/// this binary return [`ApplyError::StoreFromFutureVersion`] instead of
/// attempting a downgrade.
pub fn apply_all(conn: &mut Connection, now_iso: &str) -> Result<(), ApplyError> {
    debug_assert_eq!(
        MAX_KNOWN_VERSION,
        ALL_MIGRATIONS
            .last()
            .expect("non-empty migration list")
            .version,
        "MAX_KNOWN_VERSION must equal the last migration's version"
    );
    let recorded = current_version(conn)?;
    if recorded > i64::from(MAX_KNOWN_VERSION) {
        return Err(ApplyError::StoreFromFutureVersion);
    }

    for mig in ALL_MIGRATIONS {
        if i64::from(mig.version) <= recorded {
            continue;
        }
        apply_one(conn, mig, now_iso)?;
    }
    Ok(())
}

fn apply_one(conn: &mut Connection, mig: &Migration, now_iso: &str) -> Result<(), ApplyError> {
    // BEGIN IMMEDIATE takes the write lock at transaction start, so a second
    // migrator (auto-migrate-on-open, tk-110) waits on `busy_timeout` rather
    // than racing. Re-read the version *inside* the lock: the recorded version
    // [`apply_all`] sampled before the loop may be stale — the lock winner can
    // have applied this migration in the window. Skipping a since-applied
    // version closes the TOCTOU that would otherwise throw `duplicate column`.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if i64::from(mig.version) <= current_version(&tx)? {
        return Ok(());
    }
    tx.execute_batch(mig.sql)?;

    // application_id and user_version pragmas don't accept `?` parameters.
    // Inline the values: APPLICATION_ID is a const i32, mig.version is u32.
    let pragma_sql = format!(
        "pragma application_id = {APPLICATION_ID}; pragma user_version = {};",
        mig.version
    );
    tx.execute_batch(&pragma_sql)?;

    tx.execute(
        "insert into schema_migrations(version, applied_at) values (?1, ?2)",
        rusqlite::params![i64::from(mig.version), now_iso],
    )?;

    tx.commit()?;
    Ok(())
}

/// Return the highest applied schema migration version as `i64`, or `0` when
/// the store has no `schema_migrations` table yet.
///
/// Real SQLite errors propagate — masking them with `unwrap_or(0)` would let
/// the future-version guard in [`apply_all`] silently fall through when a
/// transient error hits the lookup.
pub fn current_version(conn: &Connection) -> Result<i64, rusqlite::Error> {
    if !schema_migrations_exists(conn)? {
        return Ok(0);
    }
    conn.query_row(
        "select coalesce(max(version), 0) from schema_migrations",
        [],
        |r| r.get::<_, i64>(0),
    )
}

fn schema_migrations_exists(conn: &Connection) -> Result<bool, rusqlite::Error> {
    let present: Option<i64> = conn
        .query_row(
            "select 1 from sqlite_master where type='table' and name='schema_migrations'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(present.is_some())
}

/// Apply migrations only up to and including `max_version`, leaving the store
/// at an intentionally behind-version state.
///
/// Test-only seam for exercising the auto-migrate-on-open path (tk-110): a
/// store frozen at an older schema is the exact regression an upgraded `tk`
/// binary must heal at the open chokepoint.
#[cfg(test)]
pub(crate) fn apply_through(
    conn: &mut Connection,
    max_version: u32,
    now_iso: &str,
) -> Result<(), ApplyError> {
    for mig in ALL_MIGRATIONS {
        if mig.version > max_version {
            break;
        }
        apply_one(conn, mig, now_iso)?;
    }
    Ok(())
}

// ---- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn open_memory() -> Connection {
        let conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        conn
    }

    #[test]
    fn apply_all_on_empty_db_installs_every_migration() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();

        assert_eq!(current_version(&conn).unwrap(), 3);

        let app_id: i64 = conn
            .query_row("pragma application_id", [], |r| r.get(0))
            .unwrap();
        assert_eq!(app_id, i64::from(APPLICATION_ID));

        let user_version: i64 = conn
            .query_row("pragma user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, 3);
    }

    #[test]
    fn apply_all_is_idempotent_on_a_current_store() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let count: i64 = conn
            .query_row("select count(*) from schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn apply_all_rejects_future_version() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn.execute(
            "insert into schema_migrations(version, applied_at) values (?1, ?2)",
            rusqlite::params![999_i64, "2099-01-01T00:00:00.000Z"],
        )
        .unwrap();

        assert!(matches!(
            apply_all(&mut conn, "2026-05-09T00:00:00.000Z"),
            Err(ApplyError::StoreFromFutureVersion)
        ));
    }

    #[test]
    fn apply_all_records_applied_at_from_caller_supplied_clock() {
        let mut conn = open_memory();
        let fixed = "2026-05-09T12:34:56.789Z";
        apply_all(&mut conn, fixed).unwrap();

        let stamp: String = conn
            .query_row(
                "select applied_at from schema_migrations where version = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamp, fixed);
    }

    #[test]
    fn closing_reason_accepts_nonempty_value_on_a_done_item() {
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
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

        conn.execute(
            "update items set closing_reason = ?1 where id = 't1'",
            rusqlite::params!["Fixed in PR #12"],
        )
        .unwrap();

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
    fn closing_reason_check_rejects_a_reason_on_a_non_done_item() {
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Open",
                status: "open",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let err = conn
            .execute(
                "update items set closing_reason = ?1 where id = 't1'",
                rusqlite::params!["premature"],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "a Closing Reason on a non-done item must violate the CHECK: {err}"
        );
    }

    #[test]
    fn closing_reason_check_rejects_an_empty_reason() {
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
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

        let err = conn
            .execute("update items set closing_reason = '' where id = 't1'", [])
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "an empty Closing Reason must violate the CHECK: {err}"
        );
    }

    #[test]
    fn apply_one_re_reads_version_and_no_ops_when_already_applied() {
        // The TOCTOU close (tk-110): two tk processes can both read v2, then
        // race to apply migration 3. The loser must re-read the version under
        // its write lock and skip — re-running migration 3's SQL would throw
        // `duplicate column: closing_reason`. Driving apply_one directly on an
        // already-current store reproduces the loser's stale-snapshot view.
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        assert_eq!(current_version(&conn).unwrap(), 3);

        apply_one(&mut conn, &MIGRATION_3, "2026-05-09T00:00:01.000Z")
            .expect("re-applying an already-applied migration must be a clean no-op");

        // The skip leaves the original row untouched (no duplicate stamp).
        let count: i64 = conn
            .query_row(
                "select count(*) from schema_migrations where version = 3",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn apply_all_surfaces_sqlite_error_with_table_name() {
        // Pre-create one of the tables migration_1 creates so the migration's
        // `create table items` fails. The error message should mention the
        // conflicting table so command-side stderr can render it verbatim.
        let mut conn = open_memory();
        conn.execute_batch("create table items (x integer)")
            .unwrap();

        let err = apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("items"), "error should mention `items`: {msg}");
    }
}
