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

use crate::store::backup::{self, BackupError};

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
const MIGRATION_7_SQL: &str = include_str!("migrations/007_promotion_operation.sql");
const MIGRATION_8_SQL: &str = include_str!("migrations/008_mutation_applying.sql");
const MIGRATION_9_SQL: &str = include_str!("migrations/009_mutation_cancelled.sql");
const MIGRATION_10_SQL: &str = include_str!("migrations/010_mutation_abandoned.sql");
const MIGRATION_11_SQL: &str = include_str!("migrations/011_split_work_state.sql");
const MIGRATION_12_SQL: &str = include_str!("migrations/012_drop_dead_next_index.sql");
const MIGRATION_13_SQL: &str = include_str!("migrations/013_former_backend_identities.sql");
const MIGRATION_14_SQL: &str = include_str!("migrations/014_binding_display_provenance.sql");
const MIGRATION_15_SQL: &str = include_str!("migrations/015_readopt_reopens_a_former_identity.sql");
const MIGRATION_16_SQL: &str = include_str!("migrations/016_sync_skip_relinquishes_a_close.sql");

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

/// Adds the nullable `promotion_operation_id` column grouping every Mutation
/// one `tk promote` invocation appends into a single Promotion Operation
/// (ADR-0036). A plain `ALTER TABLE ADD COLUMN` with no CHECK — ADR-0036
/// rejected pairing this with a table rebuild since none is otherwise
/// required.
pub const MIGRATION_7: Migration = Migration {
    version: 7,
    sql: MIGRATION_7_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Rebuilds `mutations` to add the durable `applying` state used to guard
/// non-idempotent Backend creation. Runs with foreign keys disabled because
/// the table-level State x Failure CHECK cannot be altered in place.
pub const MIGRATION_8: Migration = Migration {
    version: 8,
    sql: MIGRATION_8_SQL,
    foreign_keys: ForeignKeys::Off,
};

/// Rebuilds `mutations` to add the terminal `cancelled` state Promotion
/// Cancellation writes (ADR-0038), and to restrict `skipped` to non-Promotion
/// Mutation Types — a Promotion leaves the queue through cancellation, never
/// through Sync Skip. Runs with foreign keys disabled because the table-level
/// State x Failure CHECK cannot be altered in place.
pub const MIGRATION_9: Migration = Migration {
    version: 9,
    sql: MIGRATION_9_SQL,
    foreign_keys: ForeignKeys::Off,
};

/// Rebuilds `mutations` to add the terminal `abandoned` state Promotion
/// Cancellation writes for a Promotion whose Backend creation outcome was never
/// observed (ADR-0039). The clause admits Promotion Mutation Types alone, the
/// way `applying` does, since `applying` is the only edge into it. Runs with
/// foreign keys disabled because the table-level State x Failure CHECK cannot
/// be altered in place.
pub const MIGRATION_10: Migration = Migration {
    version: 10,
    sql: MIGRATION_10_SQL,
    foreign_keys: ForeignKeys::Off,
};

/// Rebuilds `items` to split Work State out of Item Status (ADR-0043):
/// `status` narrows to the two-valued Lifecycle a Backend Adapter shares, and
/// the new `work_state` column carries the local idle/active axis ADR-0029's
/// `active ⟹ accepted` conjunct now reads. Also cancels the `set_item_status`
/// Mutations the fused column manufactured for `tk start` / `tk stop`, sparing
/// those a Promotion Operation owns. Runs `ForeignKeys::Off` for the same
/// table-rebuild reason as MIGRATION_5 and MIGRATION_6.
pub const MIGRATION_11: Migration = Migration {
    version: 11,
    sql: MIGRATION_11_SQL,
    foreign_keys: ForeignKeys::Off,
};

/// Drops the dead `items_next_idx` (ADR-0045). No query plan in the crate
/// reaches it, and its `(priority, created_seq)` key cannot serve the
/// Effective-Priority-led ordering ADR-0015 gave `tk next`. `ForeignKeys::On`
/// because dropping an index is not a table rebuild.
pub const MIGRATION_12: Migration = Migration {
    version: 12,
    sql: MIGRATION_12_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Adds canonical Former Backend Identity history and enforces ownership
/// across active and former identities (ADR-0047).
pub const MIGRATION_13: Migration = Migration {
    version: 13,
    sql: MIGRATION_13_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Records whether an active Backend Binding displaced a known local Display
/// ID, and recovers that provenance from unambiguous legacy Alias history
/// (ADR-0047).
pub const MIGRATION_14: Migration = Migration {
    version: 14,
    sql: MIGRATION_14_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Recreates `items_no_escape_from_done` with a narrow exception for the exact
/// Re-Adopt rebind, so an imported `open` Lifecycle can clear a Closing Reason
/// the CHECK confines to `done` (ADR-0006 as amended, ADR-0047).
pub const MIGRATION_15: Migration = Migration {
    version: 15,
    sql: MIGRATION_15_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Recreates `items_no_escape_from_done` with a second, time-shaped exception:
/// any `done` -> `open` write is admitted for as long as the Item carries a
/// `failed` closing (`set_item_status` targeting `done`) Mutation, which is
/// what authorizes Sync Skip's relinquished close (ADR-0046 as amended,
/// ADR-0006).
pub const MIGRATION_16: Migration = Migration {
    version: 16,
    sql: MIGRATION_16_SQL,
    foreign_keys: ForeignKeys::On,
};

/// Ordered migration list applied by [`apply_all`].
pub const ALL_MIGRATIONS: &[Migration] = &[
    MIGRATION_1,
    MIGRATION_2,
    MIGRATION_3,
    MIGRATION_4,
    MIGRATION_5,
    MIGRATION_6,
    MIGRATION_7,
    MIGRATION_8,
    MIGRATION_9,
    MIGRATION_10,
    MIGRATION_11,
    MIGRATION_12,
    MIGRATION_13,
    MIGRATION_14,
    MIGRATION_15,
    MIGRATION_16,
];

/// Highest schema version this binary can apply. Named so future migrations
/// surface the threshold to `grep` instead of hiding it behind `.last()`.
/// Adding a migration is a two-line patch: append to `ALL_MIGRATIONS`, bump
/// this constant, with a debug_assert below catching the drift.
pub const MAX_KNOWN_VERSION: u32 = MIGRATION_16.version;

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
    /// The Store Backup that must precede a migration could not be written
    /// (ADR-0048). Fail closed: the store keeps its old schema rather than
    /// being upgraded with no copy of what it held.
    #[error("failed to back up the Repository Store before migrating")]
    Backup(#[from] BackupError),
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

    // One Store Backup per run, before the run (ADR-0048). `recorded > 0` is
    // "the store already has a schema": a store being created has nothing to
    // lose, and `tk init` reaches here for both cases. The upper bound is what
    // the loop below would find pending, since `ALL_MIGRATIONS` ascends to
    // `MAX_KNOWN_VERSION` (the `debug_assert` above pins that).
    if recorded > 0 && recorded < i64::from(MAX_KNOWN_VERSION) {
        backup::write_pre_migration(conn, now_iso)?;
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
    let result = apply_one_txn(conn, mig, now_iso);
    if fk_off {
        // Restore enforcement on *every* path — a failed rebuild must never
        // leave the connection with foreign keys disabled for the rest of the
        // session. Best-effort: the migration result is what the caller sees.
        let _ = conn.execute_batch("pragma foreign_keys = on");
    }
    result
}

fn apply_one_txn(conn: &mut Connection, mig: &Migration, now_iso: &str) -> Result<(), ApplyError> {
    let fk_off = mig.foreign_keys == ForeignKeys::Off;
    // The write lock is taken at transaction start, so a second migrator
    // (auto-migrate-on-open, tk-110) waits on `busy_timeout` rather than
    // racing. Re-read the version *inside* the lock: the recorded version
    // [`apply_all`] sampled before the loop may be stale — the lock winner can
    // have applied this migration in the window. Skipping a since-applied
    // version closes the TOCTOU that would otherwise throw `duplicate column`.
    let tx = crate::store::write_transaction(conn)?;
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
    use crate::store::testing::{
        FixtureFormerIdentity, FixtureItem, insert_alias, insert_fixture_former_identity,
        insert_fixture_item,
    };
    use rusqlite::params;

    fn open_memory() -> Connection {
        let conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        conn
    }

    #[test]
    fn apply_all_on_empty_db_installs_every_migration() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();

        assert_eq!(
            current_version(&conn).unwrap(),
            i64::from(MAX_KNOWN_VERSION)
        );

        let app_id: i64 = conn
            .query_row("pragma application_id", [], |r| r.get(0))
            .unwrap();
        assert_eq!(app_id, i64::from(APPLICATION_ID));

        let user_version: i64 = conn
            .query_row("pragma user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, i64::from(MAX_KNOWN_VERSION));
    }

    #[test]
    fn apply_all_is_idempotent_on_a_current_store() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let count: i64 = conn
            .query_row("select count(*) from schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(usize::try_from(count).unwrap(), ALL_MIGRATIONS.len());
    }

    /// Seed one detached Backend Ticket: a `done` Local Item whose Former
    /// Backend Identity still reserves the canonical Backend object to it,
    /// which is the state Re-Adopt rebinds from (ADR-0047).
    fn insert_detached_done_ticket(conn: &Connection, backend_key: &str) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Closed locally",
                status: "done",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_former_identity(
            conn,
            FixtureFormerIdentity {
                backend_key,
                item_id: "t1",
                backend_display_value: "gh-7",
                ..FixtureFormerIdentity::default()
            },
        )
        .unwrap();
    }

    /// Re-Adopt's rebind: the local Display ID becomes an Alias, the Backend
    /// Display ID becomes current, and one `items` UPDATE moves the Item onto
    /// the canonical identity while importing an `open` Lifecycle — which
    /// clears the Closing Reason the CHECK confines to `done`.
    ///
    /// The Display ID moves ride along because `items.display_value` carries a
    /// deferred composite foreign key into `item_ids`; the rebind is only ever
    /// committed as a whole.
    fn readopt_rebind(conn: &mut Connection, backend_key: &str) -> rusqlite::Result<()> {
        let tx = crate::store::write_transaction(conn)?;
        tx.execute(
            "update item_ids set source = 'alias' where item_id = 't1' and source = 'display'",
            [],
        )?;
        tx.execute(
            "insert into item_ids(value, source, item_id, created_at) \
             values ('gh-7', 'display', 't1', '2026-05-09T00:00:00.000Z')",
            [],
        )?;
        tx.execute(
            "update items \
                set origin = 'backend', backend_kind = 'github', backend_key = ?1, \
                    display_value = 'gh-7', status = 'open', closing_reason = null, \
                    binding_display_provenance = 'known', \
                    binding_local_display_value = 'tk-1' \
              where id = 't1'",
            params![backend_key],
        )?;
        tx.commit()
    }

    /// Assert the done-terminal trigger is what refused a `done` -> `open`
    /// write, not some other constraint.
    ///
    /// `is_err()` alone cannot tell the two apart: these fixtures carry a
    /// Closing Reason, and an UPDATE that clears `status` without clearing it
    /// trips the `closing_reason` CHECK too. Dropping a trigger conjunct then
    /// leaves every negative test passing for the wrong reason.
    fn assert_trigger_refused(result: rusqlite::Result<usize>, why: &str) {
        match result {
            Err(err) => assert!(
                err.to_string().contains("cannot leave done state"),
                "{why}; refused, but by {err} rather than the trigger"
            ),
            Ok(_) => panic!("{why}"),
        }
    }

    #[test]
    fn readopt_may_import_an_open_lifecycle_onto_a_done_item() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_detached_done_ticket(&conn, "https://github.com/o/r/issues/7");
        conn.execute(
            "update items set closing_reason = 'Superseded' where id = 't1'",
            [],
        )
        .unwrap();

        readopt_rebind(&mut conn, "https://github.com/o/r/issues/7")
            .expect("Re-Adopt imports the Backend Lifecycle (ADR-0047)");

        let (status, closing_reason): (String, Option<String>) = conn
            .query_row(
                "select status, closing_reason from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((status.as_str(), closing_reason), ("open", None));
    }

    #[test]
    fn the_readopt_exception_needs_the_items_own_former_identity() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_detached_done_ticket(&conn, "https://github.com/o/r/issues/7");

        // A Backend object this Item never held is ordinary intake, not a
        // rebind, so it stays under the done-terminal rule (ADR-0006).
        let unknown_identity = readopt_rebind(&mut conn, "https://github.com/o/r/issues/8");
        assert!(
            unknown_identity.is_err(),
            "only the Item's own Former Backend Identity authorizes the reopen"
        );

        // And the authorization does not outlive the rebind: once the Item is
        // Backend-bound again, every done -> open write is forbidden.
        readopt_rebind(&mut conn, "https://github.com/o/r/issues/7").unwrap();
        conn.execute("update items set status = 'done' where id = 't1'", [])
            .unwrap();
        let later_reopen = conn.execute("update items set status = 'open' where id = 't1'", []);
        assert!(
            later_reopen.is_err(),
            "a restored Backend Item must not escape done again"
        );
    }

    /// Seed a plain `done` Ticket carrying a Closing Reason: the ordinary Sync
    /// Skip population, with no Former Backend Identity in play.
    fn insert_done_ticket(conn: &Connection) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Closed by tk done",
                status: "done",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        conn.execute(
            "update items set closing_reason = 'Shipped' where id = 't1'",
            [],
        )
        .unwrap();
    }

    #[test]
    fn sync_skip_may_reopen_a_done_item_with_its_failed_closing_mutation() {
        use crate::store::testing::{FixtureMutation, insert_fixture_mutation};
        const TO_DONE: &str = r#"{"status":"done"}"#;
        const REJECTION: &str = r#"{"detail":"rejected"}"#;

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_done_ticket(&conn);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: TO_DONE,
                state: "failed",
                failure_json: Some(REJECTION),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        conn.execute(
            "update items set status = 'open', closing_reason = null where id = 't1'",
            [],
        )
        .expect("a failed closing Mutation authorizes Sync Skip's reopen (ADR-0046)");

        let (status, closing_reason): (String, Option<String>) = conn
            .query_row(
                "select status, closing_reason from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((status.as_str(), closing_reason), ("open", None));
    }

    #[test]
    fn the_reopen_exception_admits_an_epic_on_the_same_terms() {
        use crate::store::testing::{FixtureMutation, insert_fixture_mutation};

        // ADR-0046: "The rule is identical for Tickets and Epics." The
        // trigger reaches a Mutation through `(item_id, item_class)`, the pair
        // ADR-0010 addresses Mutations by, so an Epic has to be reachable
        // through the same conjunct a Ticket is.
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
                title: "Closed Epic",
                status: "done",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "e1",
                item_class: "epic",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        conn.execute("update items set status = 'open' where id = 'e1'", [])
            .expect("a failed closing Mutation authorizes the reopen for an Epic too (ADR-0046)");

        let status: String = conn
            .query_row("select status from items where id = 'e1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "open");
    }

    #[test]
    fn a_done_item_with_no_failed_closing_mutation_stays_refused() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_done_ticket(&conn);

        let reopen = conn.execute("update items set status = 'open' where id = 't1'", []);
        assert_trigger_refused(
            reopen,
            "no failed closing Mutation exists, so the done-terminal rule still applies (ADR-0006)",
        );
    }

    #[test]
    fn a_failed_non_closing_status_mutation_does_not_authorize_reopen() {
        use crate::store::testing::{FixtureMutation, insert_fixture_mutation};
        const TO_ACTIVE: &str = r#"{"status":"active"}"#;
        const REJECTION: &str = r#"{"detail":"rejected"}"#;

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_done_ticket(&conn);
        // Migration 011 spares exactly this shape from its cancellation sweep —
        // a failed, non-`done` `set_item_status` row belonging to a Promotion
        // Operation (011_split_work_state.sql:132-135) — so it is a real
        // population, not a hypothetical one. The payload conjunct is what
        // keeps it from authorizing an unrelated done -> open write.
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: TO_ACTIVE,
                state: "failed",
                failure_json: Some(REJECTION),
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let reopen = conn.execute("update items set status = 'open' where id = 't1'", []);
        assert_trigger_refused(
            reopen,
            "a failed non-closing status Mutation must not authorize a done -> open write",
        );
    }

    #[test]
    fn a_pending_closing_mutation_does_not_authorize_reopen() {
        use crate::store::testing::{FixtureMutation, insert_fixture_mutation};

        // `tk done` on a Backend-bound Item leaves exactly this shape — a
        // `done` row plus a `pending` closing Mutation — until the next
        // `tk sync`. It is the most common live state in the Store, so the
        // exception must not admit it.
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_done_ticket(&conn);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        assert_trigger_refused(
            conn.execute(
                "update items set status = 'open', closing_reason = null where id = 't1'",
                [],
            ),
            "a closing Mutation still queued must not authorize a done -> open write",
        );
    }

    #[test]
    fn a_failed_close_on_another_item_does_not_authorize_this_one() {
        use crate::store::testing::{FixtureMutation, insert_fixture_mutation};

        // The exception is reserved to the Item its own failed closing
        // Mutation names. Without that, one failed close anywhere in the Store
        // would authorize reopening every `done` Item.
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_done_ticket(&conn);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t2",
                display: "tk-2",
                title: "Its close failed",
                status: "done",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t2",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        assert_trigger_refused(
            conn.execute(
                "update items set status = 'open', closing_reason = null where id = 't1'",
                [],
            ),
            "another Item's failed close must not authorize this one's reopen",
        );
    }

    #[test]
    fn an_applied_closing_mutation_does_not_authorize_reopen() {
        use crate::store::testing::{FixtureMutation, insert_fixture_mutation};

        // The ordinary state of every successfully closed Backend-bound Item
        // is a `done` row plus an `applied` closing Mutation. If the trigger
        // exception did not test the Mutation's state, that shape alone would
        // authorize a reopen — which is the whole of ADR-0006's backstop, not
        // an edge case.
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_done_ticket(&conn);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "applied",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        assert_trigger_refused(
            conn.execute(
                "update items set status = 'open', closing_reason = null where id = 't1'",
                [],
            ),
            "an applied closing Mutation must not authorize a done -> open write (ADR-0006)",
        );
    }

    #[test]
    fn a_later_done_to_open_is_refused_once_the_mutation_moves_to_skipped() {
        use crate::store::testing::{FixtureMutation, insert_fixture_mutation};
        const TO_DONE: &str = r#"{"status":"done"}"#;
        const REJECTION: &str = r#"{"detail":"rejected"}"#;

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_done_ticket(&conn);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: TO_DONE,
                state: "failed",
                failure_json: Some(REJECTION),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        // The exception is live while the row is `failed` — this is the same
        // reopen as the first test above, replayed here as the setup for the
        // cycle below.
        conn.execute(
            "update items set status = 'open', closing_reason = null where id = 't1'",
            [],
        )
        .expect("the failed closing Mutation authorizes this reopen");

        // The Item closes again (open -> done is unrestricted), and Sync
        // Skip's real transaction would reopen it and move this same row to
        // `skipped` together. Simulating just the state move shows the
        // authorization does not survive past that row leaving `failed`.
        conn.execute(
            "update items set status = 'done', closing_reason = 'Shipped again' where id = 't1'",
            [],
        )
        .unwrap();
        conn.execute(
            "update mutations set state = 'skipped' where sequence = 1",
            [],
        )
        .unwrap();

        let later_reopen = conn.execute("update items set status = 'open' where id = 't1'", []);
        assert_trigger_refused(
            later_reopen,
            "once the closing Mutation is no longer failed, a later done -> open write is refused again (ADR-0046)",
        );
    }

    #[test]
    fn former_backend_identity_is_reserved_to_its_stable_item() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "former-owner",
                display: "gh-1",
                title: "Former owner",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/1"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "other",
                display: "gh-2",
                title: "Other Item",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/2"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_former_identity(
            &conn,
            FixtureFormerIdentity {
                backend_key: "https://github.com/o/r/issues/1",
                item_id: "former-owner",
                backend_display_value: "gh-1",
                ..FixtureFormerIdentity::default()
            },
        )
        .unwrap();
        conn.execute(
            "update items set origin = 'local', backend_kind = null, backend_key = null \
              where id = 'former-owner'",
            [],
        )
        .unwrap();

        let claim = conn.execute(
            "update items \
                set backend_key = 'https://github.com/o/r/issues/1' \
              where id = 'other'",
            [],
        );
        assert!(
            claim.is_err(),
            "another Item must not claim a former identity"
        );

        let move_history = conn.execute(
            "update former_backend_identities set item_id = 'other' \
              where backend_key = 'https://github.com/o/r/issues/1'",
            [],
        );
        assert!(
            move_history.is_err(),
            "former identity history must not move onto an actively bound Item"
        );
    }

    #[test]
    fn binding_display_provenance_migration_classifies_legacy_aliases() {
        let mut conn = open_memory();
        apply_through(&mut conn, 13, "2026-05-09T00:00:00.000Z").unwrap();
        for (id, display, key, item_class, status, created_seq) in [
            ("adopted", "gh-1", "1", "ticket", "open", 1),
            ("promoted", "gh-2", "2", "epic", "open", 2),
            ("ambiguous", "gh-3", "3", "ticket", "done", 3),
        ] {
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id,
                    display,
                    title: "Backend Item",
                    item_class,
                    ticket_kind: (item_class == "ticket").then_some("task"),
                    priority: (item_class == "ticket").then_some("P2"),
                    status,
                    origin: "backend",
                    backend_kind: Some("github"),
                    backend_key: Some(key),
                    created_seq,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
        }
        insert_alias(&conn, "tk-2", "promoted").unwrap();
        insert_alias(&conn, "tk-3", "ambiguous").unwrap();
        insert_alias(&conn, "old-3", "ambiguous").unwrap();

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let mut stmt = conn
            .prepare(
                "select id, binding_display_provenance, binding_local_display_value \
                   from items order by created_seq",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("adopted".into(), "none".into(), None),
                ("promoted".into(), "known".into(), Some("tk-2".into())),
                ("ambiguous".into(), "ambiguous".into(), None),
            ]
        );

        assert!(
            conn.execute(
                "update items set binding_display_provenance = 'known' \
                  where id = 'adopted'",
                [],
            )
            .is_err(),
            "known provenance requires the exact displaced Display ID"
        );
        assert!(
            conn.execute(
                "update items set origin = 'local', backend_kind = null, backend_key = null \
                  where id = 'ambiguous'",
                [],
            )
            .is_err(),
            "a Local Item cannot retain active Binding provenance"
        );
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
        use crate::store::testing::{FixtureItem, insert_pre_split_fixture_item};

        let mut conn = open_memory();
        apply_through(&mut conn, 5, "2026-05-09T00:00:00.000Z").unwrap();
        insert_pre_split_fixture_item(
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
        insert_pre_split_fixture_item(
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
        use crate::store::testing::{FixtureItem, insert_pre_split_fixture_item};

        let mut conn = open_memory();
        apply_through(&mut conn, 4, "2026-05-09T00:00:00.000Z").unwrap();
        insert_pre_split_fixture_item(
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
        insert_pre_split_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-2",
                priority: Some("P1"),
                title: "Child",
                container_id: Some("e1"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();
        assert_eq!(
            current_version(&conn).unwrap(),
            i64::from(MAX_KNOWN_VERSION)
        );

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
    fn promotion_operation_id_column_exists_after_apply_all() {
        use crate::store::testing::{
            FixtureItem, FixtureMutation, insert_fixture_item, insert_fixture_mutation,
        };

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
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                mutation_type: "update_ticket",
                item_id: "t1",
                promotion_operation_id: Some("promo-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let stored: Option<String> = conn
            .query_row(
                "select promotion_operation_id from mutations where sequence = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored.as_deref(), Some("promo-1"));
    }

    #[test]
    fn promotion_operation_id_heals_in_on_a_store_frozen_at_v6() {
        // An older tk binary leaves a store at v6, with Mutations rows that
        // predate the Promotion Operation column. Upgrading to v7 must add
        // the column without disturbing those existing rows (migration 007
        // is a plain ADD COLUMN, not a rebuild).
        use crate::store::testing::{FixtureItem, insert_pre_split_fixture_item};

        let mut conn = open_memory();
        apply_through(&mut conn, 6, "2026-05-09T00:00:00.000Z").unwrap();
        insert_pre_split_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ticket",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("1"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        // Hand-written insert matching the pre-007 column list: the
        // production `mutations::append` and `FixtureMutation` helper both
        // already target the post-007 schema, so a v6-shaped row needs its
        // own statement here.
        conn.execute(
            "insert into mutations(\
                sequence, mutation_type, item_id, item_class, payload_json, \
                state, failure_json, created_at, state_changed_at\
             ) values (1, 'update_ticket', 't1', 'ticket', '{}', 'pending', null, \
                       '2026-05-09T00:00:00.000Z', '2026-05-09T00:00:00.000Z')",
            [],
        )
        .unwrap();

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        assert_eq!(
            current_version(&conn).unwrap(),
            i64::from(MAX_KNOWN_VERSION)
        );
        let (mutation_type, promotion_operation_id): (String, Option<String>) = conn
            .query_row(
                "select mutation_type, promotion_operation_id from mutations where sequence = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            mutation_type, "update_ticket",
            "the pre-existing row survives the heal"
        );
        assert_eq!(
            promotion_operation_id, None,
            "the healed column must be selectable and NULL for pre-existing rows"
        );
    }

    #[test]
    fn applying_state_upgrade_preserves_rows_foreign_keys_and_indexes() {
        use crate::store::testing::{
            FixtureItem, FixtureMutation, insert_fixture_mutation, insert_pre_split_fixture_item,
        };

        let mut conn = open_memory();
        apply_through(&mut conn, 7, "2026-05-09T00:00:00.000Z").unwrap();
        insert_pre_split_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 4,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let preserved: (String, String, Option<String>) = conn
            .query_row(
                "select mutation_type, state, promotion_operation_id \
                   from mutations where sequence = 4",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            preserved,
            (
                "promote_ticket".into(),
                "pending".into(),
                Some("op-1".into())
            )
        );

        conn.execute(
            "update mutations set state = 'applying' where sequence = 4",
            [],
        )
        .unwrap();
        conn.execute(
            "update mutations set failure_json = '{\"detail\":\"unknown effect\"}' where sequence = 4",
            [],
        )
        .unwrap();

        let index_count: i64 = conn
            .query_row(
                "select count(*) from sqlite_master where type = 'index' \
                   and name in ('mutations_state_idx', 'mutations_promotion_operation_idx')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 2);

        let dangling = conn.execute(
            "insert into mutations(\
                sequence, mutation_type, item_id, item_class, payload_json, state, \
                failure_json, created_at, state_changed_at\
             ) values (5, 'update_ticket', 'missing', 'ticket', '{}', 'pending', \
                       null, 'now', 'now')",
            [],
        );
        assert!(
            dangling.is_err(),
            "the rebuilt table must retain its Item foreign key"
        );
    }

    #[test]
    fn applying_state_accepts_only_matching_promotion_shapes() {
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "ticket",
                display: "tk-1",
                title: "Ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "epic",
                display: "tk-2",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        for (sequence, mutation_type, item_id, item_class) in [
            (1, "update_ticket", "ticket", "ticket"),
            (2, "promote_epic", "ticket", "ticket"),
            (3, "promote_ticket", "epic", "epic"),
        ] {
            let result = conn.execute(
                "insert into mutations(\
                    sequence, mutation_type, item_id, item_class, payload_json, state, \
                    failure_json, created_at, state_changed_at\
                 ) values (?1, ?2, ?3, ?4, '{}', 'applying', null, 'now', 'now')",
                rusqlite::params![sequence, mutation_type, item_id, item_class],
            );
            assert!(
                result.is_err(),
                "{mutation_type}/{item_class} must not enter applying"
            );
        }
    }

    #[test]
    fn cancelled_state_upgrade_preserves_rows_and_the_promotion_foreign_key() {
        use crate::store::testing::{
            FixtureItem, FixtureMutation, insert_fixture_mutation, insert_pre_split_fixture_item,
        };

        let mut conn = open_memory();
        apply_through(&mut conn, 8, "2026-05-09T00:00:00.000Z").unwrap();
        insert_pre_split_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 4,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let preserved: (String, String, Option<String>, Option<String>) = conn
            .query_row(
                "select mutation_type, state, failure_json, promotion_operation_id \
                   from mutations where sequence = 4",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            preserved,
            (
                "promote_ticket".into(),
                "failed".into(),
                Some(r#"{"detail":"rejected"}"#.into()),
                Some("op-1".into())
            )
        );

        conn.execute(
            "update mutations set state = 'cancelled' where sequence = 4",
            [],
        )
        .expect("a failed Promotion may be withdrawn, keeping its failure evidence");

        let index_count: i64 = conn
            .query_row(
                "select count(*) from sqlite_master where type = 'index' \
                   and name in ('mutations_state_idx', 'mutations_promotion_operation_idx')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 2);

        let dangling = conn.execute(
            "insert into mutations(\
                sequence, mutation_type, item_id, item_class, payload_json, state, \
                failure_json, created_at, state_changed_at\
             ) values (5, 'update_ticket', 'missing', 'ticket', '{}', 'pending', \
                       null, 'now', 'now')",
            [],
        );
        assert!(
            dangling.is_err(),
            "the rebuilt table must retain its Item foreign key"
        );
    }

    #[test]
    fn a_promotion_may_be_cancelled_but_never_skipped() {
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

        let insert = |sequence: i64, mutation_type: &str, state: &str| {
            conn.execute(
                "insert into mutations(\
                    sequence, mutation_type, item_id, item_class, payload_json, state, \
                    failure_json, created_at, state_changed_at\
                 ) values (?1, ?2, 't1', 'ticket', '{}', ?3, null, 'now', 'now')",
                rusqlite::params![sequence, mutation_type, state],
            )
        };

        assert!(
            insert(1, "promote_ticket", "skipped").is_err(),
            "Sync Skip never touches a Promotion; the schema, not a runtime guard, says so"
        );
        insert(2, "promote_ticket", "cancelled").expect("cancellation is a Promotion's exit");
        insert(3, "update_ticket", "skipped")
            .expect("Sync Skip still curates an ordinary Mutation");
        insert(4, "update_ticket", "cancelled")
            .expect("collateral of a withdrawn operation is cancelled whatever its kind");
    }

    #[test]
    fn only_a_promotion_may_be_abandoned() {
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
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "tk-2",
                title: "Epic",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                selection_state: None,
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let insert = |sequence: i64, mutation_type: &str, item: &str, class: &str, state: &str| {
            conn.execute(
                "insert into mutations(\
                    sequence, mutation_type, item_id, item_class, payload_json, state, \
                    failure_json, created_at, state_changed_at\
                 ) values (?1, ?2, ?3, ?4, '{}', ?5, null, 'now', 'now')",
                rusqlite::params![sequence, mutation_type, item, class, state],
            )
        };

        insert(1, "promote_ticket", "t1", "ticket", "abandoned")
            .expect("an unobserved Ticket creation is abandoned");
        insert(2, "promote_epic", "e1", "epic", "abandoned")
            .expect("an unobserved Epic creation is abandoned");
        assert!(
            insert(3, "update_ticket", "t1", "ticket", "abandoned").is_err(),
            "only a creation can have an unobserved outcome, so only a Promotion is abandoned"
        );
        assert!(
            insert(4, "promote_epic", "t1", "ticket", "abandoned").is_err(),
            "the clause pins the Mutation Type to the Item Class, as `applying` does"
        );
    }

    /// Seed one Item at the pre-split schema, where `items.status` still holds
    /// the fused three-valued spelling.
    /// Seed an item into a store frozen at v10, where `items.status` still
    /// carries the fused three-valued spelling this migration splits.
    fn insert_v10_item(conn: &Connection, id: &str, display: &str, status: &str, seq: i64) {
        use crate::store::testing::{FixtureItem, insert_pre_split_fixture_item};

        insert_pre_split_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Work",
                status,
                created_seq: seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    /// The `(status, work_state)` pair stored for `id`, as raw strings.
    ///
    /// Deliberately not `store::testing::item_axes`, which decodes into
    /// [`Lifecycle`] and [`WorkState`]. A migration's job is to put particular
    /// spellings in the columns, so these tests assert the stored bytes rather
    /// than routing them through the `FromSql` layer — which would let a
    /// migration and a decoder be wrong together and still agree.
    fn axes(conn: &Connection, id: &str) -> (String, String) {
        conn.query_row(
            "select status, work_state from items where id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn split_rewrites_active_rows_across_both_axes() {
        // The upgrade's whole job on `items`: `active` was the fused value, so
        // it becomes an open Lifecycle carrying an active Work State, while
        // `open` and `done` keep their Lifecycle and land idle. One column
        // could never hold both, so no row can come out `(done, active)`.
        let mut conn = open_memory();
        apply_through(&mut conn, 10, "2026-05-09T00:00:00.000Z").unwrap();
        insert_v10_item(&conn, "o", "tk-1", "open", 1);
        insert_v10_item(&conn, "a", "tk-2", "active", 2);
        insert_v10_item(&conn, "d", "tk-3", "done", 3);

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        assert_eq!(axes(&conn, "o"), ("open".into(), "idle".into()));
        assert_eq!(axes(&conn, "a"), ("open".into(), "active".into()));
        assert_eq!(axes(&conn, "d"), ("done".into(), "idle".into()));
    }

    #[test]
    fn status_check_rejects_the_departed_active_spelling() {
        // `active` left `items.status` with this migration. A writer still
        // spelling it must fail loudly rather than store a value no reader
        // decodes.
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_v10_item(&conn, "t1", "tk-1", "open", 1);

        let err = conn
            .execute("update items set status = 'active' where id = 't1'", [])
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "`active` must no longer be a legal Lifecycle: {err}"
        );
    }

    #[test]
    fn work_state_check_rejects_an_unknown_value() {
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        insert_v10_item(&conn, "t1", "tk-1", "open", 1);

        let err = conn
            .execute("update items set work_state = 'paused' where id = 't1'", [])
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "Work State is idle or active and nothing else: {err}"
        );
    }

    #[test]
    fn an_active_ticket_must_still_be_accepted_after_the_conjunct_moves() {
        // ADR-0029 relocated, not retired: the conjunct now reads `work_state`
        // and still lives in the Ticket branch, because Selection State is what
        // it compares against. `tk start`'s guard owns the diagnostic; this is
        // the schema backstop behind it.
        use crate::store::testing::{FixtureItem, insert_fixture_item};

        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        for (id, display, selection, priority, seq) in [
            ("t1", "tk-1", "triage", None, 1),
            ("t2", "tk-2", "parked", Some("P2"), 2),
        ] {
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id,
                    display,
                    title: "Work",
                    priority,
                    selection_state: Some(selection),
                    created_seq: seq,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
            let err = conn
                .execute(
                    "update items set work_state = 'active' where id = ?1",
                    rusqlite::params![id],
                )
                .unwrap_err();
            assert!(
                format!("{err}").contains("CHECK"),
                "a {selection} Ticket must not go active: {err}"
            );
        }
    }

    #[test]
    fn the_relocated_conjunct_admits_an_active_epic() {
        // Work State covers Epics as well as Tickets (ADR-0043), and an Epic
        // carries a NULL Selection State. The conjunct sits inside the Ticket
        // branch of the combined CHECK, so the Epic branch must not read it —
        // otherwise `tk start` on an Epic would abort against the schema and,
        // worse, a store holding an active Epic could not upgrade at all.
        use crate::store::testing::{FixtureItem, insert_pre_split_fixture_item};

        let mut conn = open_memory();
        apply_through(&mut conn, 10, "2026-05-09T00:00:00.000Z").unwrap();
        insert_pre_split_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "tk-1",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic under way",
                status: "active",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z")
            .expect("an active Epic must survive the upgrade, not abort it");

        assert_eq!(axes(&conn, "e1"), ("open".into(), "active".into()));
    }

    #[test]
    fn split_cancels_only_the_status_mutations_it_manufactured() {
        // Every `pending`/`failed` `set_item_status` Mutation targeting a
        // non-`done` status is tk's own record of the defect this split
        // removes: nobody asked tk to push "start working" to a Backend. Rows
        // belonging to a Promotion Operation are left queued instead — ADR-0038
        // reserves that withdrawal for a human — and every other Mutation is
        // none of this migration's business.
        //
        // Three of the WHERE clause's four conjuncts are pinned here; the
        // `mutation_type` one is not, and cannot be. The `update_ticket` row
        // below looks like its guard but is not: `json_extract` returns NULL
        // for a payload carrying no `$.status`, and `NULL <> 'done'` is not
        // TRUE, so the status conjunct already excluded it. `ItemStatus` is the
        // only `MutationPayload` variant that serializes a `status` key, so no
        // fixture can separate the two terms. The Mutation Type conjunct is
        // defence in depth against a payload shape that does not exist yet.
        use crate::store::testing::{FixtureMutation, insert_fixture_mutation};

        const TO_OPEN: &str = r#"{"status":"open"}"#;
        const TO_ACTIVE: &str = r#"{"status":"active"}"#;
        const TO_DONE: &str = r#"{"status":"done"}"#;
        const AN_EDIT: &str = r#"{"title":"T","body":""}"#;
        const REJECTION: &str = r#"{"detail":"rejected"}"#;

        let mut conn = open_memory();
        apply_through(&mut conn, 10, "2026-05-09T00:00:00.000Z").unwrap();
        insert_v10_item(&conn, "t1", "tk-1", "active", 1);

        let seeded = [
            // (sequence, mutation_type, payload, state, failure, operation)
            (1, "set_item_status", TO_OPEN, "pending", None, None),
            (
                2,
                "set_item_status",
                TO_ACTIVE,
                "failed",
                Some(REJECTION),
                None,
            ),
            (3, "set_item_status", TO_DONE, "pending", None, None),
            (4, "set_item_status", TO_OPEN, "applied", None, None),
            (5, "set_item_status", TO_OPEN, "skipped", None, None),
            (6, "set_item_status", TO_OPEN, "pending", None, Some("op-1")),
            (7, "update_ticket", AN_EDIT, "pending", None, None),
        ];
        for (sequence, mutation_type, payload_json, state, failure_json, operation) in seeded {
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence,
                    mutation_type,
                    item_id: "t1",
                    payload_json,
                    state,
                    failure_json,
                    promotion_operation_id: operation,
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
        }

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let rows: Vec<(i64, String, Option<String>, String)> = conn
            .prepare(
                "select sequence, state, failure_json, state_changed_at \
                   from mutations order by sequence",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (1, "cancelled".into(), None, SEEDED_AT.into()),
                (
                    2,
                    "cancelled".into(),
                    // `Preserve`, per the transition table's row for this edge:
                    // the earlier rejection's evidence outlives the withdrawal.
                    Some(REJECTION.into()),
                    SEEDED_AT.into()
                ),
                (3, "pending".into(), None, SEEDED_AT.into()),
                (4, "applied".into(), None, SEEDED_AT.into()),
                (5, "skipped".into(), None, SEEDED_AT.into()),
                (6, "pending".into(), None, SEEDED_AT.into()),
                (7, "pending".into(), None, SEEDED_AT.into()),
            ],
            "only non-closing, non-Promotion-Operation status Mutations withdraw, \
             and none of them takes a fresh state_changed_at"
        );
    }

    /// The `created_at` / `state_changed_at` every fixture seeds. Migration 011
    /// must leave the latter alone: it has no `Clock` seam to stamp a new one.
    const SEEDED_AT: &str = "2026-05-09T00:00:00.000Z";

    #[test]
    fn split_rebuild_copies_every_local_field_verbatim() {
        // The other half of an ADR-0028 rebuild: it retypes the table by
        // copying it, so a column dropped from the copy list, or two columns
        // transposed within it, is silent. The store keeps working and the
        // value is simply gone. Closing Reason (ADR-0023) is the one that
        // cannot be recovered — it is a Local Field the Repository Store is
        // the only copy of, so losing it here loses it for good.
        //
        // Every value below differs from every other, so a transposition
        // cannot hide behind two columns holding the same literal.
        //
        // This test pins ONE rebuild by hand-enumerated columns, and nothing
        // makes the next one arrive with its own. Every ADR-0028 rebuild needs
        // a copy-fidelity test, because the suite cannot otherwise tell a
        // lossy copy from a good one: the fixtures a migration test seeds are
        // not the rows a real store holds. Treat this as the template.
        //
        // `container_class` is asserted explicitly rather than left to the
        // CHECK that appears to guard it. That CHECK cannot catch a NULL here:
        // with `container_id` set and `container_class` NULL it evaluates
        // `false or (true and null)` = NULL, and SQLite fails a CHECK only on
        // FALSE. `pragma foreign_key_check` misses it too, since an FK with a
        // NULL component is not enforced — so the row would keep rendering its
        // Epic membership while having silently lost the FK that guards it.
        use crate::store::testing::{FixtureItem, insert_pre_split_fixture_item};

        let mut conn = open_memory();
        apply_through(&mut conn, 10, "2026-05-09T00:00:00.000Z").unwrap();
        insert_pre_split_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "tk-9",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_pre_split_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                body: "Body text",
                status: "done",
                container_id: Some("e1"),
                created_at: "2026-01-01T00:00:00.000Z",
                updated_at: "2026-02-02T00:00:00.000Z",
                created_seq: 7,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        // Not a `FixtureItem` field, and the column CHECK confines it to
        // `done` rows, which the fixture above already is.
        conn.execute(
            "update items set closing_reason = 'Fixed in PR #12' where id = 't1'",
            [],
        )
        .unwrap();

        apply_all(&mut conn, "2026-05-09T00:00:01.000Z").unwrap();

        let row: (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            i64,
            Option<String>,
        ) = conn
            .query_row(
                "select display_value, title, body, created_at, updated_at, \
                        closing_reason, created_seq, container_class \
                   from items where id = 't1'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "tk-1".to_owned(),
                "Subject".to_owned(),
                "Body text".to_owned(),
                "2026-01-01T00:00:00.000Z".to_owned(),
                "2026-02-02T00:00:00.000Z".to_owned(),
                Some("Fixed in PR #12".to_owned()),
                7,
                Some("epic".to_owned()),
            )
        );
    }

    /// The `items` indexes and trigger ADR-0028's rebuild recipe covers, as the
    /// store holds them, sorted. A name missing from the `in` list is invisible
    /// to every caller, so a new `items` object belongs here too.
    fn items_objects(conn: &Connection) -> Vec<String> {
        let mut names: Vec<String> = conn
            .prepare(
                "select name from sqlite_master \
                  where name in ('items_backend_unique', 'items_container_idx', \
                                 'items_id_class_unique', 'items_next_idx', \
                                 'items_no_escape_from_done')",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        names.sort();
        names
    }

    #[test]
    fn every_trigger_body_is_pinned() {
        // `items_objects` compares trigger *names*, so a migration that
        // recreates one from an older body keeps the name and passes it — and
        // the exceptions ADR-0046 and ADR-0047 rely on live only in these
        // bodies. Behavioural tests cover the conjuncts someone thought to
        // cover; this pins the text, so any body change (a dropped conjunct,
        // or a new clause that widens an exception) has to be accepted here
        // deliberately rather than slipping through as a passing suite.
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        let mut stmt = conn
            .prepare("select sql from sqlite_master where type = 'trigger' order by name")
            .unwrap();
        let bodies: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        insta::assert_snapshot!(bodies.join("\n\n"), @"
        CREATE TRIGGER active_backend_identity_not_owned_by_another_former_item_insert
        before insert on items
        when new.backend_kind is not null
         and exists (
            select 1
              from former_backend_identities
             where backend_kind = new.backend_kind
               and backend_key = new.backend_key
               and item_id <> new.id
         )
        begin
            select raise(abort, 'backend identity is owned by another Item');
        end

        CREATE TRIGGER active_backend_identity_not_owned_by_another_former_item_update
        before update of backend_kind, backend_key on items
        when new.backend_kind is not null
         and exists (
            select 1
              from former_backend_identities
             where backend_kind = new.backend_kind
               and backend_key = new.backend_key
               and item_id <> new.id
         )
        begin
            select raise(abort, 'backend identity is owned by another Item');
        end

        CREATE TRIGGER dependencies_no_cycle before insert on dependencies
        for each row when exists (
            with recursive reachable(id) as (
                select new.blocking_id
                union
                select dependencies.blocking_id
                  from dependencies, reachable
                 where dependencies.blocked_id = reachable.id
            )
            select 1 from reachable where id = new.blocked_id
        ) begin
            select raise(abort, 'dependency cycle');
        end

        CREATE TRIGGER former_backend_identity_not_owned_by_another_active_item
        before insert on former_backend_identities
        when exists (
            select 1
              from items
             where backend_kind = new.backend_kind
               and backend_key = new.backend_key
               and id <> new.item_id
        )
        begin
            select raise(abort, 'backend identity is owned by another Item');
        end

        CREATE TRIGGER former_backend_identity_ownership_is_immutable
        before update of backend_kind, backend_key, item_id on former_backend_identities
        when new.backend_kind <> old.backend_kind
          or new.backend_key <> old.backend_key
          or new.item_id <> old.item_id
        begin
            select raise(abort, 'former backend identity ownership is immutable');
        end

        CREATE TRIGGER items_no_escape_from_done before update of status on items
        for each row when old.status = 'done' and new.status != 'done'
          and not (
              old.origin = 'local'
              and new.origin = 'backend'
              and exists (
                  select 1
                    from former_backend_identities f
                   where f.item_id = new.id
                     and f.backend_kind = new.backend_kind
                     and f.backend_key = new.backend_key
              )
          )
          and not (
              exists (
                  select 1
                    from mutations m
                   where m.item_id = new.id
                     and m.item_class = new.item_class
                     and m.mutation_type = 'set_item_status'
                     and m.state = 'failed'
                     and json_extract(m.payload_json, '$.status') = 'done'
              )
          )
        begin
            select raise(abort, 'cannot leave done state');
        end
        ");
    }

    #[test]
    fn split_rebuild_preserves_every_items_object() {
        // An ADR-0028 rebuild recreates the table from scratch, so every index
        // and trigger has to be written out again. A forgotten one fails
        // silently — the store keeps working until some later insert loses its
        // uniqueness guard or escapes `done`. Assert the whole set by name.
        let mut conn = open_memory();
        apply_through(&mut conn, 10, "2026-05-09T00:00:00.000Z").unwrap();
        insert_v10_item(&conn, "t1", "tk-1", "active", 1);

        // Stops at 11: this test checks what migration 011's rebuild recreates.
        // Running to `apply_all` would fold in migration 012's drop of
        // `items_next_idx` (ADR-0045), so a forgotten index and a dropped one
        // would fail the same assertion.
        apply_through(&mut conn, 11, "2026-05-09T00:00:01.000Z").unwrap();

        assert_eq!(
            items_objects(&conn),
            vec![
                "items_backend_unique",
                "items_container_idx",
                "items_id_class_unique",
                "items_next_idx",
                "items_no_escape_from_done",
            ]
        );

        // The trigger is the one object with observable behaviour rather than a
        // plan effect, so it is worth firing as well as naming.
        conn.execute("update items set status = 'done' where id = 't1'", [])
            .unwrap();
        let escape = conn.execute("update items set status = 'open' where id = 't1'", []);
        assert!(
            escape.is_err(),
            "the rebuilt table must keep refusing to leave done (ADR-0006)"
        );

        let dangling = conn.execute(
            "update items set container_id = 'missing', container_class = 'epic' where id = 't1'",
            [],
        );
        assert!(
            dangling.is_err(),
            "the rebuilt table must retain its container foreign key"
        );
    }

    #[test]
    fn next_index_drop_keeps_every_other_items_object() {
        // ADR-0045 took `items_next_idx` off the set an ADR-0028 rebuild
        // recreates, so its absence is a contract now: a rebuild that copies an
        // older recreate list restores it and fails here. Naming the four
        // survivors too catches a rebuild that loses one of them on the way.
        let mut conn = open_memory();
        apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();

        assert_eq!(
            items_objects(&conn),
            vec![
                "items_backend_unique",
                "items_container_idx",
                "items_id_class_unique",
                "items_no_escape_from_done",
            ],
            "migration 012 drops items_next_idx and nothing else (ADR-0045)"
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

        // The failed migration rolled back: no probe table, version unchanged.
        let probe: Option<i64> = conn
            .query_row(
                "select 1 from sqlite_master where type='table' and name='fk_probe'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert!(probe.is_none(), "the failed rebuild must leave no trace");
        assert_eq!(
            current_version(&conn).unwrap(),
            i64::from(MAX_KNOWN_VERSION)
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
        assert_eq!(
            current_version(&conn).unwrap(),
            i64::from(MAX_KNOWN_VERSION)
        );

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
