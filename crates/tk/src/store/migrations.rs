//! Repository Store schema migrations.
//!
//! The migration SQL is the durable artefact (ADR-0005); the Rust port reuses
//! it *verbatim* via `include_str!` from `src/store/migrations/` so there is
//! one source of truth while the Zig oracle still serves `main`.
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

// `include_str!` resolves paths relative to this source file. From
// `crates/tk/src/store/migrations.rs` the Zig oracle's SQL lives four levels
// up under `src/store/migrations/`. Pulling the SQL by reference keeps the
// "verbatim" promise of ADR-0017 / ADR-0018 mechanical instead of typographic.
// CRLF safety is enforced by `.gitattributes` (`*.sql text eol=lf`) so a
// Windows clone with `core.autocrlf=true` still checks the files out as LF.
const MIGRATION_1_SQL: &str = include_str!("../../../../src/store/migrations/001_repository_store.sql");
const MIGRATION_2_SQL: &str = include_str!("../../../../src/store/migrations/002_items_no_escape_from_done.sql");

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

/// Ordered migration list applied by [`apply_all`].
pub const ALL_MIGRATIONS: &[Migration] = &[MIGRATION_1, MIGRATION_2];

/// Highest schema version this binary can apply. Named so future migrations
/// surface the threshold to `grep` instead of hiding it behind `.last()`.
/// Adding `MIGRATION_3` is a two-line patch: append to `ALL_MIGRATIONS`, bump
/// this constant, with a debug_assert below catching the drift.
pub const MAX_KNOWN_VERSION: u32 = MIGRATION_2.version;

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
        ALL_MIGRATIONS.last().expect("non-empty migration list").version,
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
    let tx = conn.transaction()?;
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

        assert_eq!(current_version(&conn).unwrap(), 2);

        let app_id: i64 = conn
            .query_row("pragma application_id", [], |r| r.get(0))
            .unwrap();
        assert_eq!(app_id, i64::from(APPLICATION_ID));

        let user_version: i64 = conn
            .query_row("pragma user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, 2);
    }

    #[test]
    fn apply_all_is_idempotent_on_a_current_store() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let count: i64 = conn
            .query_row("select count(*) from schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
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
    fn apply_all_surfaces_sqlite_error_with_table_name() {
        // Pre-create one of the tables migration_1 creates so the migration's
        // `create table items` fails. The error message should mention the
        // conflicting table so command-side stderr can render it verbatim.
        let mut conn = open_memory();
        conn.execute_batch("create table items (x integer)").unwrap();

        let err = apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("items"), "error should mention `items`: {msg}");
    }
}
