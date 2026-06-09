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

/// Whether a migration needs foreign-key enforcement disabled while it runs
/// (ADR-0028). A SQLite table rebuild drops the table, whose implicit DELETE
/// would trip the on-delete-restrict child foreign keys; the rebuild must run
/// with `foreign_keys = off`, which is a no-op inside a transaction and so is
/// toggled by the runner around `BEGIN`. Every non-rebuild migration is `On`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignKeys {
    On,
    Off,
}

/// One schema migration in the ordered Repository Store migration list.
pub struct Migration {
    /// Monotonic schema version recorded in `schema_migrations` and mirrored
    /// to `pragma user_version`.
    pub version: u32,
    /// SQL batch executed inside the migration transaction.
    pub sql: &'static str,
    /// Foreign-key enforcement mode for this migration's transaction
    /// (ADR-0028). `Off` only for table rebuilds.
    pub foreign_keys: ForeignKeys,
}

// `include_str!` resolves paths relative to this source file; the SQL lives in
// the sibling `migrations/` directory. Pulling the SQL by reference keeps the
// "verbatim" promise of ADR-0017 / ADR-0018 mechanical instead of typographic.
// CRLF safety is enforced by `.gitattributes` (`*.sql text eol=lf`) so a
// Windows clone with `core.autocrlf=true` still checks the files out as LF.
const MIGRATION_1_SQL: &str = include_str!("migrations/001_repository_store.sql");
const MIGRATION_2_SQL: &str = include_str!("migrations/002_items_no_escape_from_done.sql");
const MIGRATION_3_SQL: &str = include_str!("migrations/003_closing_reason.sql");
const MIGRATION_4_SQL: &str = include_str!("migrations/004_selection_state.sql");
const MIGRATION_5_SQL: &str = include_str!("migrations/005_relax_priority_for_triage.sql");
const MIGRATION_6_SQL: &str = include_str!("migrations/006_require_accepted_for_active.sql");

/// V1 Repository Store schema skeleton.
pub const MIGRATION_1: Migration = Migration {
    version: 1,
    sql: MIGRATION_1_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Adds the `items_no_escape_from_done` trigger that enforces "Done is
/// terminal" (ADR-0006) at the schema layer.
pub const MIGRATION_2: Migration = Migration {
    version: 2,
    sql: MIGRATION_2_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Adds the nullable `closing_reason` Local Field (ADR-0023). The column
/// CHECK keeps a Closing Reason non-empty and confined to `done` items.
pub const MIGRATION_3: Migration = Migration {
    version: 3,
    sql: MIGRATION_3_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Adds the nullable `selection_state` Local Field (ADR-0027): a Ticket-only
/// intake/selection policy. The column CHECK keeps it confined to Tickets
/// (`NULL` for Epics) and pins the legal spellings; the backfill lands every
/// existing Ticket on `accepted` so the field is total over Tickets from the
/// moment it exists.
pub const MIGRATION_4: Migration = Migration {
    version: 4,
    sql: MIGRATION_4_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Rebuilds `items` to admit triage Tickets with no Priority (ADR-0028),
/// folding the priority and selection_state CHECKs into one combined
/// Priority × Selection State invariant and promoting tk-73's
/// writer-guaranteed `ticket ⟹ non-null selection_state` to a schema
/// guarantee. Runs `ForeignKeys::Off`: the table rebuild's DROP would
/// otherwise trip the on-delete-restrict child foreign keys.
pub const MIGRATION_5: Migration = Migration {
    version: 5,
    sql: MIGRATION_5_SQL,
    foreign_keys: ForeignKeys::Off,
};

/// Rebuilds `items` to enforce `active ⟹ accepted` (ADR-0029): a Ticket is
/// `active` only while `accepted`. The clause folds into the combined Ticket
/// invariant CHECK rather than a trigger, because it is a row-shape rule (it
/// reads only the new row), unlike the done-terminal *transition* trigger.
/// Runs `ForeignKeys::Off` for the same table-rebuild reason as MIGRATION_5.
pub const MIGRATION_6: Migration = Migration {
    version: 6,
    sql: MIGRATION_6_SQL,
    foreign_keys: ForeignKeys::Off,
};

/// Ordered migration list applied by [`apply_all`].
pub const ALL_MIGRATIONS: &[Migration] = &[
    MIGRATION_1,
    MIGRATION_2,
    MIGRATION_3,
    MIGRATION_4,
    MIGRATION_5,
    MIGRATION_6,
];

/// Highest schema version this binary can apply. Named so future migrations
/// surface the threshold to `grep` instead of hiding it behind `.last()`.
/// Adding `MIGRATION_6` is a two-line patch: append to `ALL_MIGRATIONS`, bump
/// this constant, with a debug_assert below catching the drift.
pub const MAX_KNOWN_VERSION: u32 = MIGRATION_6.version;

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
    /// A foreign-keys-off migration (a table rebuild, ADR-0028) left a dangling
    /// reference; `pragma foreign_key_check` named this child table. The
    /// migration transaction rolls back so the rebuild is all-or-nothing.
    #[error("migration left a dangling foreign key in table `{0}`")]
    ForeignKeyCheck(String),
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
    let fk_off = mig.foreign_keys == ForeignKeys::Off;
    if fk_off {
        // foreign_keys is a no-op inside a transaction, so a rebuild's FK-off
        // window must be opened here, before BEGIN (ADR-0028).
        conn.execute_batch("pragma foreign_keys = off")?;
    }
    let result = apply_one_txn(conn, mig, now_iso, fk_off);
    if fk_off {
        // Restore enforcement on *every* path — a failed rebuild must never
        // leave the connection with foreign keys disabled for the rest of the
        // session. Best-effort: the migration result is what the caller sees.
        let _ = conn.execute_batch("pragma foreign_keys = on");
    }
    result
}

fn apply_one_txn(
    conn: &mut Connection,
    mig: &Migration,
    now_iso: &str,
    fk_off: bool,
) -> Result<(), ApplyError> {
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

    if fk_off {
        // Validate the rebuild's foreign keys *inside* the transaction so a
        // dangling reference rolls back atomically with the migration rather
        // than committing a store whose child rows dangle (ADR-0028).
        if let Some(table) = first_foreign_key_violation(&tx)? {
            return Err(ApplyError::ForeignKeyCheck(table));
        }
    }

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

/// Return the child table named by the first `pragma foreign_key_check` row,
/// or `None` when every foreign key resolves. Used to gate a foreign-keys-off
/// rebuild's commit (ADR-0028).
fn first_foreign_key_violation(conn: &Connection) -> Result<Option<String>, rusqlite::Error> {
    let mut stmt = conn.prepare("pragma foreign_key_check")?;
    let mut rows = stmt.query([])?;
    match rows.next()? {
        // Column 0 of `foreign_key_check` is the table with the dangling row.
        Some(row) => Ok(Some(row.get::<_, String>(0)?)),
        None => Ok(None),
    }
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

        assert_eq!(current_version(&conn).unwrap(), 6);

        let app_id: i64 = conn
            .query_row("pragma application_id", [], |r| r.get(0))
            .unwrap();
        assert_eq!(app_id, i64::from(APPLICATION_ID));

        let user_version: i64 = conn
            .query_row("pragma user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, 6);
    }

    #[test]
    fn apply_all_is_idempotent_on_a_current_store() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let count: i64 = conn
            .query_row("select count(*) from schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 6);
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

    /// Insert a Ticket (or Epic) directly at the v3 schema — before
    /// `selection_state` exists — so the v4 backfill has a pre-existing row to
    /// migrate. Mirrors the items + display-resolver lockstep the production
    /// writer keeps, so the deferred composite FK holds at COMMIT.
    fn insert_v3_item(conn: &Connection, id: &str, display: &str, class: &str, created_seq: i64) {
        let (kind, priority): (Option<&str>, Option<&str>) = if class == "ticket" {
            (Some("task"), Some("P2"))
        } else {
            (None, None)
        };
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute(
            "insert into items(\
                id, display_value, item_class, ticket_kind, priority, title, body, \
                origin, status, created_seq, created_at, updated_at\
             ) values (?1, ?2, ?3, ?4, ?5, 'T', '', 'local', 'open', ?6, \
                       '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')",
            rusqlite::params![id, display, class, kind, priority, created_seq],
        )
        .unwrap();
        tx.execute(
            "insert into item_ids(value, source, item_id, created_at) \
             values (?1, 'display', ?2, '2026-05-09T00:00:00.000Z')",
            rusqlite::params![display, id],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn selection_state_backfills_existing_tickets_to_accepted() {
        // An older tk binary leaves a store at v3 with bare Tickets. Upgrading
        // to v4 must land every existing Ticket on `accepted` — leaving them
        // NULL would violate the new Ticket-only CHECK on the next write.
        let mut conn = open_memory();
        apply_through(&mut conn, 3, "2026-05-09T00:00:00.000Z").unwrap();
        insert_v3_item(&conn, "t1", "tk-1", "ticket", 1);

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let (selection, updated_at): (Option<String>, String) = conn
            .query_row(
                "select selection_state, updated_at from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(selection.as_deref(), Some("accepted"));
        // The backfill is a schema migration, not a user edit: it must not bump
        // updated_at, or every migrated Ticket would read as freshly modified.
        assert_eq!(updated_at, "2026-05-09T00:00:00.000Z");
    }

    #[test]
    fn selection_state_backfill_leaves_epics_null() {
        let mut conn = open_memory();
        apply_through(&mut conn, 3, "2026-05-09T00:00:00.000Z").unwrap();
        insert_v3_item(&conn, "e1", "tk-1", "epic", 1);

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let selection: Option<String> = conn
            .query_row(
                "select selection_state from items where id = 'e1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            selection.is_none(),
            "Epics stay outside Selection State (ADR-0027): {selection:?}"
        );
    }

    #[test]
    fn selection_state_check_rejects_a_value_on_an_epic() {
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
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

        let err = conn
            .execute(
                "update items set selection_state = 'accepted' where id = 'e1'",
                [],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "a Selection State on an Epic must violate the CHECK: {err}"
        );
    }

    #[test]
    fn selection_state_check_rejects_an_unknown_value() {
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
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

        let err = conn
            .execute(
                "update items set selection_state = 'archived' where id = 't1'",
                [],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "an unknown Selection State must violate the CHECK: {err}"
        );
    }

    #[test]
    fn selection_state_null_on_a_ticket_is_rejected_after_rebuild() {
        // tk-73's tripwire, flipped: the v4 ADD COLUMN CHECK could not reject a
        // NULL `selection_state` on a Ticket (SQLite passes a NULL CHECK
        // result), so Ticket-totality rode on the writers. The v5 table rebuild
        // (ADR-0028) folds in `selection_state is not null` for the Ticket
        // branch, promoting that to a hard schema guarantee.
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
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

        let err = conn
            .execute(
                "update items set selection_state = null where id = 't1'",
                [],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "a NULL Selection State on a Ticket must violate the rebuilt CHECK: {err}"
        );
    }

    #[test]
    fn accepted_ticket_with_null_priority_is_rejected() {
        // AC traceability (tk-73): "accepted Tickets require Priority". In v1
        // the standing Ticket-requires-priority CHECK already enforces this;
        // the dedicated combined invariant lands with triage in tk-74. Pinning
        // it here documents the invariant the acceptance criterion names.
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
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

        let err = conn
            .execute("update items set priority = null where id = 't1'", [])
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "an accepted Ticket without a Priority must violate the CHECK: {err}"
        );
    }

    #[test]
    fn rebuild_admits_a_triage_ticket_with_null_priority() {
        // The point of the v5 rebuild (ADR-0028): a triage Ticket carries no
        // Priority, which the migration-001 CHECK forbade.
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Captured",
                priority: None,
                selection_state: Some("triage"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .expect("a triage Ticket with NULL Priority is valid after the rebuild");

        let (priority, selection): (Option<String>, String) = conn
            .query_row(
                "select priority, selection_state from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(priority.is_none());
        assert_eq!(selection, "triage");
    }

    #[test]
    fn rebuild_rejects_a_parked_ticket_without_priority() {
        // The other half of the combined invariant: accepted/parked require a
        // Priority. (Parked is reachable only via tk-75, but the schema rule
        // baked in here is total.)
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Parked",
                selection_state: Some("parked"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let err = conn
            .execute("update items set priority = null where id = 't1'", [])
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "a parked Ticket without a Priority must violate the CHECK: {err}"
        );
    }

    #[test]
    fn rebuild_heals_active_non_accepted_loophole_rows() {
        // tk-75 shipped `park` status-agnostic and without a start-guard (that
        // is this slice), so a store it touched can hold `active` + parked /
        // `active` + triage rows — legal under v5, illegal under v6's CHECK.
        // The rebuild copy must heal them (demote to `open`, the equivalent of
        // a stop), not abort the upgrade.
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_through(&mut conn, 5, "2026-05-09T00:00:00.000Z").unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "a",
                display: "tk-1",
                title: "Active parked",
                status: "active",
                priority: Some("P2"),
                selection_state: Some("parked"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "b",
                display: "tk-2",
                title: "Active triage",
                status: "active",
                priority: None,
                selection_state: Some("triage"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z")
            .expect("the v6 upgrade must heal loophole rows, not abort");

        let mut rows: Vec<(String, String, String)> = conn
            .prepare("select id, status, selection_state from items order by created_seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("a".into(), "open".into(), "parked".into()),
                ("b".into(), "open".into(), "triage".into()),
            ],
            "loophole rows heal to open, Selection State preserved"
        );
    }

    #[test]
    fn active_ticket_must_be_accepted() {
        // tk-76 (ADR-0029): a Ticket may be `active` only while `accepted`.
        // The CHECK is the defence-in-depth backstop behind the `tk start` /
        // `tk park` Rust guards.
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        // An accepted, active Ticket is valid.
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Working",
                status: "active",
                priority: Some("P2"),
                selection_state: Some("accepted"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .expect("an accepted, active Ticket is valid");

        // Parking it at the SQL layer (bypassing the Rust guard) must trip the
        // CHECK: `active` work cannot be held.
        let err = conn
            .execute(
                "update items set selection_state = 'parked' where id = 't1'",
                [],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "an active Ticket that is not accepted must violate the CHECK: {err}"
        );
    }

    #[test]
    fn active_triage_ticket_is_rejected_at_insert() {
        // The CHECK covers INSERT, not just UPDATE — a reason it is a CHECK and
        // not a BEFORE UPDATE trigger (ADR-0029).
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        let result = insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Captured but active",
                status: "active",
                priority: None,
                selection_state: Some("triage"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        );
        assert!(
            result.is_err(),
            "an active triage Ticket must be rejected at INSERT"
        );
    }

    #[test]
    fn rebuild_preserves_existing_priority_and_selection_state() {
        // Heal a v4 store with real rows through the v5 rebuild and confirm the
        // copy is lossless for the columns the rebuild reshapes.
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_through(&mut conn, 4, "2026-05-09T00:00:00.000Z").unwrap();
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
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-2",
                priority: Some("P1"),
                title: "Child",
                container_id: Some("e1"),
                container_class: Some("epic"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();
        assert_eq!(current_version(&conn).unwrap(), 6);

        let (priority, selection, container): (Option<String>, String, Option<String>) = conn
            .query_row(
                "select priority, selection_state, container_id from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(priority.as_deref(), Some("P1"));
        assert_eq!(selection, "accepted");
        assert_eq!(container.as_deref(), Some("e1"));

        let epic_selection: Option<String> = conn
            .query_row(
                "select selection_state from items where id = 'e1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            epic_selection.is_none(),
            "Epic stays outside Selection State"
        );
    }

    #[test]
    fn fk_off_rebuild_restores_foreign_keys_after_a_failed_check() {
        // ADR-0028's load-bearing contract: a foreign-keys-off migration whose
        // rebuild leaves a dangling reference must roll back AND re-enable
        // foreign keys, or the connection would run the rest of the session
        // with enforcement silently off. This drives a hand-built FK-off
        // migration whose copy violates a foreign key.
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();

        let bad = Migration {
            version: 99,
            // With foreign keys off, the dangling child row inserts cleanly; the
            // runner's `foreign_key_check` inside the transaction then catches it.
            sql: "create table fk_probe(id text, parent_id text references items(id)); \
                  insert into fk_probe(id, parent_id) values ('c', 'nonexistent');",
            foreign_keys: ForeignKeys::Off,
        };
        let result = apply_one(&mut conn, &bad, "2026-05-09T00:00:02.000Z");
        assert!(
            matches!(result, Err(ApplyError::ForeignKeyCheck(_))),
            "a dangling reference must surface as ForeignKeyCheck: {result:?}"
        );

        let fk_on: i64 = conn
            .query_row("pragma foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            fk_on, 1,
            "foreign keys must be re-enabled after a failed rebuild"
        );

        // The failed migration rolled back: no probe table, store still at v6.
        let probe: Option<i64> = conn
            .query_row(
                "select 1 from sqlite_master where type='table' and name='fk_probe'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert!(probe.is_none(), "the failed rebuild must leave no trace");
        assert_eq!(current_version(&conn).unwrap(), 6);
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
        assert_eq!(current_version(&conn).unwrap(), 6);

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
