//! Store Backups: a compacted copy of the Repository Store taken before a
//! schema migration runs (ADR-0048).
//!
//! The migration runner owns the *decision* — it is the only place that knows
//! a migration is about to run — and this module owns the *mechanism*: where
//! the copy goes, what it is called, how many survive, and what a failure to
//! write one means. Nothing here knows what a migration does.
//!
//! A Store Backup is readable evidence of what the store held before an
//! upgrade, not an undo. Restoring one leaves it to be migrated forward again
//! (ADR-0024), which is why this module writes copies and offers no restore.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use thiserror::Error;

use crate::platform;
use crate::store::migrations;

/// How many Store Backups survive a prune (ADR-0048).
///
/// Ten rather than three: a lossy migration is noticed late, and the migration
/// list moves fast enough that three is a window of days for anyone building
/// from source. Being bounded is the property that matters.
const BACKUPS_KEPT: usize = 10;

/// Why a Store Backup could not be written (ADR-0048).
///
/// Kept distinct from the migration's own SQLite faults so the command layer
/// can say the upgrade was refused for want of a backup rather than blaming
/// the migration SQL.
#[derive(Debug, Error)]
pub enum BackupError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Write one Store Backup of `conn` before its pending migrations run.
///
/// A connection with no file to copy is a no-op. Callers decide *whether* a
/// backup is owed; this decides everything about how one is made.
pub fn write_pre_migration(conn: &Connection, now_iso: &str) -> Result<(), BackupError> {
    let Some(dir) = backup_dir(conn) else {
        return Ok(());
    };
    fs::create_dir_all(&dir)?;
    // Always tk's own directory, so ARCHITECTURE.md's "only tighten what we
    // created" rule holds unconditionally here. Best-effort, as in `tk init`.
    let _ = platform::set_dir_mode_0700(&dir);

    let partial = partial_path(&dir);
    // `VACUUM INTO` refuses a non-empty target, and a `SIGKILL` mid-vacuum
    // never reaches the cleanup below, so clear a leftover from an earlier
    // process that held this pid before writing.
    if let Err(err) = fs::remove_file(&partial)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return Err(err.into());
    }

    vacuum_into_place(conn, &partial, &dir, now_iso).inspect_err(|_| {
        // Pruning will not collect this: it does not match a Store Backup's
        // name. Left behind, it would sit there until some later run drew the
        // same pid, so drop it here.
        let _ = fs::remove_file(&partial);
    })?;

    // Best-effort: the backup protecting this upgrade is already on disk, so
    // refusing the migration because an old one could not be deleted would
    // trade the protection away for tidiness.
    prune(&dir);
    Ok(())
}

/// Working name a backup is vacuumed to before it is renamed into place.
///
/// Unique per PID namespace, which is the scope of the concurrency ADR-0024
/// designs for. A clock cannot serve here: the injectable test clock is
/// pinned, so two runs would agree on the name.
fn partial_path(dir: &Path) -> PathBuf {
    dir.join(format!(".partial-{}.db", std::process::id()))
}

/// Directory Store Backups are written to, or `None` when the connection has
/// no file to copy.
///
/// Derived from the connection rather than passed in (ADR-0048).
/// `Connection::path` yields an empty string for an in-memory database, so
/// every unit test that opens `:memory:` writes no backup by construction.
fn backup_dir(conn: &Connection) -> Option<PathBuf> {
    let path = conn.path().filter(|path| !path.is_empty())?;
    Some(Path::new(path).parent()?.join("backups"))
}

/// Vacuum into `partial`, name it from what it actually holds, and move it
/// into place.
fn vacuum_into_place(
    conn: &Connection,
    partial: &Path,
    dir: &Path,
    now_iso: &str,
) -> Result<(), BackupError> {
    // `Connection::path` yields `None` rather than invalid UTF-8, so a store
    // whose path is not UTF-8 never reaches here — `backup_dir` returns `None`
    // and no backup is attempted. Everything below it is that `&str` joined
    // with ASCII, so the borrow back cannot fail.
    let target = partial
        .to_str()
        .expect("backup path derives from Connection::path, which is UTF-8");
    // `VACUUM INTO` takes an expression, so the path binds as a parameter and
    // needs no quoting. It must run outside a transaction, which is why the
    // backup precedes the per-migration transactions rather than joining one.
    conn.execute("vacuum into ?1", [target])?;

    // Name the file from the version it actually contains, not from the one
    // sampled before the vacuum: a concurrent migrator can commit in between
    // (ADR-0024), and a label read out of the file cannot be wrong.
    // `schema_migrations` is authoritative; `pragma user_version` mirrors it.
    let version = migrations::current_version(&Connection::open(partial)?)?;
    let named = dir.join(format!("{}-v{version:03}.db", file_stamp(now_iso)));
    fs::rename(partial, &named)?;
    Ok(())
}

/// Delete all but the newest [`BACKUPS_KEPT`] Store Backups in `dir`.
///
/// Only files matching the generated name are considered, so the hand-made
/// copies users keep beside their store are never deleted. The timestamp
/// leads that name and is fixed-width, so sorting by name sorts by age.
///
/// Infallible by design: the backup protecting the current upgrade is already
/// on disk by the time this runs, and one file that will not delete must not
/// strand every older one behind it.
fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            is_backup_name(path.file_name()?.to_str()?).then_some(path)
        })
        .collect();
    if found.len() <= BACKUPS_KEPT {
        return;
    }

    // Every path shares `dir`, so `Path`'s component-wise ordering decides on
    // the filename alone — which the leading fixed-width stamp makes an age.
    found.sort();
    for path in &found[..found.len() - BACKUPS_KEPT] {
        let _ = fs::remove_file(path);
    }
}

/// Whether `name` is a Store Backup this module wrote.
///
/// Matches the `<stamp>-v<version>.db` shape [`vacuum_into_place`] produces,
/// and nothing else — not the working file, not the store, and not a hand-made
/// copy such as `tk.db.bak`.
fn is_backup_name(name: &str) -> bool {
    let Some(rest) = name.strip_suffix(".db") else {
        return false;
    };
    let Some((stamp, version)) = rest.rsplit_once("-v") else {
        return false;
    };
    !stamp.is_empty() && version.len() >= 3 && version.bytes().all(|b| b.is_ascii_digit())
}

/// Turn an ISO timestamp into a filename component.
///
/// Only `:` is illegal in a Windows filename, so the milliseconds survive and
/// the stamp stays fixed-width — which is what lets a plain lexicographic sort
/// of the directory run in chronological order.
fn file_stamp(now_iso: &str) -> String {
    now_iso.replace(':', "-")
}

// ---- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::migrations::{ALL_MIGRATIONS, ForeignKeys, MAX_KNOWN_VERSION};
    use crate::store::testing::{
        FixtureItem, TmpStore, insert_fixture_item, seed_store_at_version,
    };

    const NOW: &str = "2026-09-03T13:45:00.123Z";

    /// Store Backups present in `store`, sorted, excluding any working file.
    fn backups(store: &TmpStore) -> Vec<String> {
        let Ok(entries) = fs::read_dir(store.tk_dir().join("backups")) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with(".partial-"))
            .collect();
        names.sort();
        names
    }

    fn seed_item(conn: &Connection, id: &str, seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display: id,
                title: "Seeded before the upgrade",
                body: "body that a lossy rebuild would drop",
                created_seq: seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    /// Seed `count` Store Backups with ascending stamps, oldest first.
    fn seed_backups(store: &TmpStore, count: usize) -> Vec<String> {
        let dir = store.tk_dir().join("backups");
        fs::create_dir_all(&dir).unwrap();
        (0..count)
            .map(|n| {
                let name = format!("2026-08-{:02}T00-00-00.000Z-v004.db", n + 1);
                fs::write(dir.join(&name), b"older backup").unwrap();
                name
            })
            .collect()
    }

    /// The shapes a user actually leaves beside their store by hand.
    const HAND_MADE: [&str; 4] = [
        "tk.db.bak",
        "ticket.db.bak",
        "tk.db.bak-before-prefix-20260521102714",
        "notes.txt",
    ];

    #[test]
    fn creating_a_store_writes_no_backup() {
        let store = TmpStore::new("tk");
        // Not `seed_store_at_version`: at version 0 there is no schema to seed
        // a Display Prefix into, which is exactly the state under test.
        fs::create_dir_all(store.tk_dir()).unwrap();
        let mut conn = Connection::open(store.db_path()).unwrap();
        migrations::apply_all(&mut conn, NOW).unwrap();

        assert!(
            backups(&store).is_empty(),
            "a store being created has no earlier state to protect"
        );
    }

    #[test]
    fn a_current_store_writes_no_backup() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, MAX_KNOWN_VERSION);
        migrations::apply_all(&mut conn, NOW).unwrap();

        assert!(
            backups(&store).is_empty(),
            "no migration ran, so there was nothing to back up before"
        );
    }

    #[test]
    fn an_in_memory_store_has_no_backup_directory() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(
            backup_dir(&conn).is_none(),
            "an in-memory store has no file to copy, so unit tests write nothing"
        );
    }

    #[test]
    fn upgrading_a_populated_store_writes_one_backup() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 4);
        seed_item(&conn, "t1", 1);
        migrations::apply_all(&mut conn, NOW).unwrap();

        assert_eq!(
            backups(&store),
            vec!["2026-09-03T13-45-00.123Z-v004.db"],
            "one backup per run, named for the version it holds"
        );
    }

    /// The regression that dropping ADR-0028's `ForeignKeys::Off` gate exists
    /// to fix: migrations 12 through 16 are all `ForeignKeys::On`, so a
    /// rebuild-gated backup would skip this upgrade entirely.
    #[test]
    fn an_upgrade_with_no_rebuild_pending_is_still_backed_up() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 11);

        assert!(
            ALL_MIGRATIONS
                .iter()
                .filter(|mig| mig.version > 11)
                .all(|mig| mig.foreign_keys == ForeignKeys::On),
            "fixture assumption: nothing after 11 is a rebuild"
        );

        migrations::apply_all(&mut conn, NOW).unwrap();
        assert_eq!(backups(&store), vec!["2026-09-03T13-45-00.123Z-v011.db"]);
    }

    #[test]
    fn a_backup_holds_the_rows_as_they_were_before_the_upgrade() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 4);
        seed_item(&conn, "t1", 1);
        seed_item(&conn, "t2", 2);
        migrations::apply_all(&mut conn, NOW).unwrap();

        let path = store.tk_dir().join("backups").join(&backups(&store)[0]);
        let backup = Connection::open(path).unwrap();

        assert_eq!(
            migrations::current_version(&backup).unwrap(),
            4,
            "the label must match what the file actually holds"
        );
        let rows: i64 = backup
            .query_row("select count(*) from items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2);
        let body: String = backup
            .query_row("select body from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(body, "body that a lossy rebuild would drop");
    }

    /// Fail closed: an upgrade that cannot be backed up does not happen. The
    /// store must be left exactly as it was, not half-migrated.
    #[test]
    fn a_backup_that_cannot_be_written_refuses_the_migration() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 4);
        // A plain file where the directory belongs, so `create_dir_all` fails
        // for a reason that does not depend on running as an unprivileged user.
        fs::write(store.tk_dir().join("backups"), b"not a directory").unwrap();

        let err = migrations::apply_all(&mut conn, NOW).expect_err("the migration must be refused");
        assert!(
            matches!(err, migrations::ApplyError::Backup(_)),
            "expected a backup failure, got {err:?}"
        );
        assert_eq!(
            migrations::current_version(&conn).unwrap(),
            4,
            "a refused upgrade leaves the schema untouched"
        );
    }

    /// A `SIGKILL` mid-vacuum cannot run the cleanup path, and `VACUUM INTO`
    /// refuses a non-empty target, so a leftover working file from a dead
    /// process holding this pid must not wedge every later upgrade.
    #[test]
    fn a_leftover_working_file_does_not_wedge_the_backup() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 4);

        let dir = store.tk_dir().join("backups");
        fs::create_dir_all(&dir).unwrap();
        fs::write(partial_path(&dir), b"torn image from a killed process").unwrap();

        migrations::apply_all(&mut conn, NOW).unwrap();
        assert_eq!(backups(&store), vec!["2026-09-03T13-45-00.123Z-v004.db"]);
    }

    /// Drives the vacuum itself to failure — an open transaction is the one
    /// refusal `VACUUM INTO` can be provoked into deterministically — and
    /// asserts on the raw directory, not the filtered helper, because a
    /// working file is exactly what the filter hides.
    ///
    /// What this pins is that the refusal reaches the caller and that the
    /// failed path adds nothing to the directory. It does not exercise the
    /// cleanup itself: SQLite rejects the transaction before it attaches the
    /// output, so no working file is ever created here. The torn image that
    /// cleanup exists for comes from a crash or an I/O fault mid-vacuum,
    /// which no test can produce on demand.
    #[test]
    fn a_failed_vacuum_leaves_nothing_behind() {
        let store = TmpStore::new("tk");
        let conn = seed_store_at_version(&store, 4);
        conn.execute_batch("begin").unwrap();

        let err = write_pre_migration(&conn, NOW).expect_err("the vacuum must fail");
        assert!(
            matches!(err, BackupError::Sqlite(_)),
            "expected the vacuum's own refusal, got {err:?}"
        );

        let left: Vec<_> = fs::read_dir(store.tk_dir().join("backups"))
            .expect("the directory is created before the vacuum runs")
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(
            left.is_empty(),
            "a failed backup must leave neither a working file nor a named one: {left:?}"
        );
    }

    #[test]
    fn pruning_keeps_the_newest_ten() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 4);
        let seeded = seed_backups(&store, 12);

        migrations::apply_all(&mut conn, NOW).unwrap();

        let kept = backups(&store);
        assert_eq!(kept.len(), BACKUPS_KEPT, "retention is bounded at ten");
        assert!(
            kept.contains(&"2026-09-03T13-45-00.123Z-v004.db".to_owned()),
            "the backup this run just wrote must survive its own prune"
        );
        assert!(
            !kept.contains(&seeded[0]) && !kept.contains(&seeded[1]),
            "the two oldest go first: the stamp leads the name, so sorting sorts by age"
        );
        assert!(kept.contains(&seeded[11]), "the newest seeded backup stays");
    }

    #[test]
    fn pruning_leaves_hand_made_copies_alone() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 4);
        seed_backups(&store, 12);

        let dir = store.tk_dir().join("backups");
        for stray in HAND_MADE {
            fs::write(dir.join(stray), b"hand made").unwrap();
        }

        migrations::apply_all(&mut conn, NOW).unwrap();

        for stray in HAND_MADE {
            assert!(
                dir.join(stray).exists(),
                "pruning inside .git must only ever delete what it wrote: {stray} is gone"
            );
        }
    }

    #[test]
    fn a_run_under_the_limit_prunes_nothing() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 4);
        seed_backups(&store, BACKUPS_KEPT - 1);

        migrations::apply_all(&mut conn, NOW).unwrap();

        assert_eq!(
            backups(&store).len(),
            BACKUPS_KEPT,
            "nine seeded plus this run's own backup is exactly the limit"
        );
    }

    #[test]
    fn only_the_generated_name_counts_as_a_backup() {
        assert!(is_backup_name("2026-09-03T13-45-00.123Z-v004.db"));
        assert!(is_backup_name("2026-09-03T13-45-00.123Z-v1024.db"));

        assert!(!is_backup_name("tk.db"));
        assert!(!is_backup_name(".partial-4242.db"));
        assert!(!is_backup_name("2026-09-03T13-45-00.123Z-v04.db"));
        assert!(!is_backup_name("2026-09-03T13-45-00.123Z-vabc.db"));
        assert!(!is_backup_name("-v004.db"));
        for stray in HAND_MADE {
            assert!(!is_backup_name(stray), "{stray} is not ours to delete");
        }
    }

    #[test]
    fn the_file_stamp_drops_only_the_windows_illegal_character() {
        assert_eq!(file_stamp(NOW), "2026-09-03T13-45-00.123Z");
        assert!(
            !file_stamp(NOW).contains(':'),
            "':' is illegal in a Windows filename"
        );
        assert!(
            file_stamp(NOW).contains(".123"),
            "milliseconds stay: the pinned test clock cannot uniquify a name"
        );
    }
}
