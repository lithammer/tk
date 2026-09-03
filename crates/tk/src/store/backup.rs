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
/// A lossy migration is noticed long after the upgrade that caused it, so the
/// window has to outlive several upgrades; the store is small enough that ten
/// costs little. What the number must be is bounded, not exact.
const BACKUPS_KEPT: usize = 10;

/// Why a Store Backup could not be written (ADR-0048).
///
/// Carries the directory, which every underlying `std::io::Error` drops: it is
/// a location this module invents and the manual alone names, so a user who
/// has just had every command start refusing has nothing else to go on.
#[derive(Debug, Error)]
#[error("{}: {source}", dir.display())]
pub struct BackupError {
    dir: PathBuf,
    #[source]
    source: Fault,
}

/// The fault behind a [`BackupError`], kept typed so a full disk stays
/// distinguishable from an unwritable directory.
#[derive(Debug, Error)]
enum Fault {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Write one Store Backup of `conn` before its pending migrations run.
///
/// A connection with no file to copy is a no-op, so callers need not ask
/// whether one is worth taking.
pub fn take(conn: &Connection, now_iso: &str) -> Result<(), BackupError> {
    let Some(dir) = backup_dir(conn) else {
        return Ok(());
    };
    write_into(conn, &dir, now_iso).map_err(|source| BackupError { dir, source })
}

/// The whole backup sequence, reporting the bare fault. [`take`] attaches the
/// directory once here rather than at each of the six fallible steps.
fn write_into(conn: &Connection, dir: &Path, now_iso: &str) -> Result<(), Fault> {
    // Tighten only a directory tk just created, as `tk init` does for `tk/`
    // (ARCHITECTURE.md). `create_dir_all` would also succeed on one the user
    // made or symlinked elsewhere, and `chmod` follows symlinks.
    match fs::create_dir(dir) {
        Ok(()) => {
            let _ = platform::set_dir_mode_0700(dir);
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }

    let partial = partial_path(dir);
    // `VACUUM INTO` refuses a non-empty target, and a `SIGKILL` mid-vacuum
    // never reaches the cleanup below, so clear a leftover from an earlier
    // process that held this pid before writing.
    if let Err(err) = fs::remove_file(&partial)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return Err(err.into());
    }

    let named = vacuum_into_place(conn, &partial, now_iso).inspect_err(|_| {
        // Pruning will not collect this: it does not match a Store Backup's
        // name. Left behind, it would sit there until some later run drew the
        // same pid, so drop it here.
        let _ = fs::remove_file(&partial);
    })?;

    prune(dir, &named);
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

/// Vacuum into `partial`, name it from what it actually holds, move it into
/// place, and return where it landed.
fn vacuum_into_place(conn: &Connection, partial: &Path, now_iso: &str) -> Result<PathBuf, Fault> {
    // `Connection::path` yields `None` for a non-UTF-8 store rather than the
    // bytes, so `backup_dir` has already turned that case away.
    let target = partial
        .to_str()
        .expect("backup path is Connection::path joined with ASCII");
    // `VACUUM INTO` takes an expression, so the path binds as a parameter and
    // needs no quoting. It must run outside a transaction, which is why the
    // backup precedes the per-migration transactions rather than joining one.
    conn.execute("vacuum into ?1", [target])?;

    // Name the file from the version it actually contains, not from the one
    // sampled before the vacuum: a concurrent migrator can commit in between
    // (ADR-0024), and a label read out of the file cannot be wrong.
    // `schema_migrations` is authoritative; `pragma user_version` mirrors it.
    //
    // The probe connection must close before the rename: Windows refuses to
    // rename a file with a live handle. Keeping it a statement temporary is
    // what closes it here — do not hoist it to a binding.
    let version = migrations::current_version(&Connection::open(partial)?)?;
    // Sibling of the working file, which already sits in the backup directory.
    let named = partial.with_file_name(format!("{}-v{version:03}.db", file_stamp(now_iso)));
    fs::rename(partial, &named)?;
    Ok(named)
}

/// Delete all but the newest [`BACKUPS_KEPT`] Store Backups in `dir`, never
/// touching `keep`.
///
/// Only files matching the generated name are considered, so the hand-made
/// copies users keep beside their store are never deleted. The timestamp
/// leads that name and is fixed-width, so sorting by name sorts by age.
///
/// `keep` is the backup protecting the migration about to run, and age here is
/// the caller's clock: a clock that jumped backwards past the newest
/// `BACKUPS_KEPT` stamps would otherwise sort the fresh backup into the delete
/// slice and silently remove the very copy this run exists to make.
///
/// Infallible by design: that backup is already on disk, and one file that
/// will not delete must not strand every older one behind it.
fn prune(dir: &Path, keep: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path != keep && is_backup_name(path.file_name()?.to_str()?)).then_some(path)
        })
        .collect();
    // `keep` occupies one of the `BACKUPS_KEPT` slots, so only that many less
    // one may survive among the rest.
    let survivors = BACKUPS_KEPT - 1;
    if found.len() <= survivors {
        return;
    }

    // Every path shares `dir`, so `Path`'s component-wise ordering decides on
    // the filename alone — which the leading fixed-width stamp makes an age.
    found.sort();
    for path in &found[..found.len() - survivors] {
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

    /// Retention selects by filename, and the filename carries the clock, so a
    /// clock that jumped backwards sorts this run's own backup oldest. It must
    /// still survive: it is the copy protecting the migration about to run, and
    /// `prune` is infallible, so losing it would be silent.
    #[test]
    fn a_backwards_clock_cannot_prune_this_run_s_own_backup() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 4);
        // Every existing backup stamped a year after `NOW`.
        let dir = store.tk_dir().join("backups");
        fs::create_dir_all(&dir).unwrap();
        for n in 0..BACKUPS_KEPT + 2 {
            let name = format!("2027-08-{:02}T00-00-00.000Z-v004.db", n + 1);
            fs::write(dir.join(&name), b"backup from the future").unwrap();
        }

        migrations::apply_all(&mut conn, NOW).unwrap();

        let kept = backups(&store);
        assert!(
            kept.contains(&"2026-09-03T13-45-00.123Z-v004.db".to_owned()),
            "the fresh backup sorts oldest under a backwards clock and must \
             still survive its own prune: {kept:?}"
        );
        assert_eq!(kept.len(), BACKUPS_KEPT, "retention is still bounded");
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

    /// A backup is owed to every upgrade, not only to an ADR-0028 rebuild.
    /// Migrations 12 through 16 are all `ForeignKeys::On`, so tying the backup
    /// to a rebuild would leave this upgrade unprotected.
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

    /// An open transaction is the one `VACUUM INTO` refusal a test can provoke
    /// on demand. Asserts on the raw directory rather than [`backups`], since
    /// a working file is what that helper hides.
    ///
    /// This pins the refusal reaching the caller, and nothing being added to
    /// the directory. It does not reach the cleanup: SQLite rejects the
    /// transaction before attaching the output, so no working file exists to
    /// remove. Only a crash or an I/O fault mid-vacuum leaves one.
    #[test]
    fn a_failed_vacuum_leaves_nothing_behind() {
        let store = TmpStore::new("tk");
        let conn = seed_store_at_version(&store, 4);
        conn.execute_batch("begin").unwrap();

        let err = take(&conn, NOW).expect_err("the vacuum must fail");
        assert!(
            matches!(err.source, Fault::Sqlite(_)),
            "expected the vacuum's own refusal, got {err:?}"
        );
        assert!(
            err.to_string().contains("backups"),
            "the diagnostic must name the directory the user has never seen: {err}"
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
    fn pruning_keeps_only_the_newest_backups() {
        let store = TmpStore::new("tk");
        let mut conn = seed_store_at_version(&store, 4);
        let seeded = seed_backups(&store, 12);

        migrations::apply_all(&mut conn, NOW).unwrap();

        let kept = backups(&store);
        assert_eq!(
            kept.len(),
            BACKUPS_KEPT,
            "retention is bounded at BACKUPS_KEPT"
        );
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
            "one under the limit, plus this run's own backup, is exactly the limit"
        );
    }

    /// The window is a recorded decision, not an implementation detail: three
    /// documents state it, and every other assertion here tracks the constant
    /// symbolically, so without this nothing fails when the value moves.
    #[test]
    fn retention_is_the_recorded_ten() {
        assert_eq!(
            BACKUPS_KEPT, 10,
            "ADR-0048, ARCHITECTURE.md and man/tk.1 all state ten; changing the \
             window is an ADR amendment, not a constant edit"
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
