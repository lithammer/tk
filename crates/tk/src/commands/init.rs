//! `tk init` — create the Repository Store at `<git-common-dir>/tk/tk.db`.
//!
//! Port of `src/commands/init.zig`. Preserves the discovery → classify →
//! pragmas → migrations → display_prefix sequence and the stable stderr
//! messages from `messages.zig` (ADR-0017).
//!
//! Slice 0 ships a flagless command: argument parsing and `--help` /
//! `--version` rendering happen upstream in [`crate::cli`] via clap-derive,
//! so [`run`] takes the parsed [`Args`] (currently empty) and proceeds
//! directly to the pipeline.

use std::fs;
use std::path::Path;

use clap::Args as ClapArgs;
use rusqlite::{Connection, OpenFlags};

use crate::cli::Deps;
use crate::git::discovery::{self, DiscoveredPaths, Outcome};
use crate::messages;
use crate::platform;
use crate::store::{display_prefix, migrations};

/// Flags for `tk init`. Slice 0 is flagless except for clap's auto-generated
/// `--help` / `-h`; future slices add knobs here.
#[derive(Debug, ClapArgs)]
pub struct Args {}

/// Classification of the SQLite file at `<git-common-dir>/tk/tk.db`.
///
/// `tk init` inspects before mutating so a foreign SQLite file can be refused
/// without changing its journal mode or application_id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum StoreKind {
    /// Newly created file with no schema yet.
    Fresh,
    /// Existing tk Repository Store (matching application_id).
    Ours,
    /// Existing SQLite file written by something else.
    Foreign,
}

/// Run `tk init` against the supplied `Deps`. Returns the process exit code.
///
/// Argument parsing and `--help` / `-h` rendering happen upstream in
/// [`crate::cli`] via clap-derive; this entrypoint receives a parsed [`Args`]
/// (currently empty) and proceeds directly to the discovery → classify →
/// pragmas → migrations → seed pipeline.
#[must_use]
pub fn run(deps: Deps<'_>, _args: Args) -> u8 {
    let outcome = discovery::discover_paths(deps.runner, deps.cwd);
    let paths = match outcome {
        Outcome::Ok(p) => p,
        other => {
            discovery::render_failure(deps.stderr, "init", &other);
            return 1;
        }
    };

    let tk_dir = paths.git_common_dir.join("tk");
    let dir_created = match ensure_tk_dir(&tk_dir) {
        Ok(c) => c,
        Err(err) => {
            let _ = writeln!(
                deps.stderr,
                "tk init: failed to create {}: {err}",
                tk_dir.display()
            );
            return 1;
        }
    };
    if dir_created {
        // Per ARCHITECTURE.md: only tighten permissions when we created the
        // directory ourselves. A pre-existing directory with broader perms
        // stays as-is.
        let _ = set_dir_mode_0700(&tk_dir);
    }

    let db_path = tk_dir.join("tk.db");
    let mut conn = match Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(err) => {
            let _ = writeln!(
                deps.stderr,
                "tk init: failed to open {}: {err}",
                db_path.display()
            );
            return 1;
        }
    };

    // Classify *before* enabling WAL or any other pragma that would mutate the
    // file. A foreign rollback-journal SQLite file at this path must stay
    // exactly as we found it when we refuse.
    let kind = match classify(&conn) {
        Ok(k) => k,
        Err(err) => {
            let _ = writeln!(
                deps.stderr,
                "tk init: failed to inspect {}: {err}",
                db_path.display()
            );
            return 1;
        }
    };
    let is_existing = match kind {
        StoreKind::Foreign => {
            let _ = writeln!(
                deps.stderr,
                "tk init: {} exists but is {}",
                db_path.display(),
                messages::INIT_REFUSE_FOREIGN
            );
            return 1;
        }
        StoreKind::Ours => true,
        StoreKind::Fresh => false,
    };

    if let Err(err) = configure_for_ticket_store(&conn) {
        let _ = writeln!(
            deps.stderr,
            "tk init: failed to configure {}: {err}",
            db_path.display()
        );
        return 1;
    }

    let now_iso = deps.clock.now_iso();
    if let Err(err) = migrations::apply_all(&mut conn, &now_iso) {
        match err {
            migrations::ApplyError::StoreFromFutureVersion => {
                let _ = writeln!(
                    deps.stderr,
                    "tk init: {} was created by a {}",
                    db_path.display(),
                    messages::INIT_REFUSE_FUTURE_VERSION
                );
            }
            migrations::ApplyError::Sqlite(sqlite_err) => {
                let _ = writeln!(deps.stderr, "tk init: migration failed: {sqlite_err}");
            }
        }
        return 1;
    }

    if let Err(err) = seed_display_prefix(&conn, &paths) {
        let _ = writeln!(deps.stderr, "tk init: failed to seed display_prefix: {err}");
        return 1;
    }

    let prefix = if is_existing {
        messages::INIT_SUCCESS_EXISTING
    } else {
        messages::INIT_SUCCESS_FRESH
    };
    let _ = writeln!(deps.stdout, "{prefix}{}", db_path.display());
    0
}

/// Classify an opened SQLite connection as fresh, ours, or foreign.
///
/// Real SQLite errors (`SQLITE_IOERR`, `SQLITE_CORRUPT`, `SQLITE_BUSY`, …) are
/// propagated — they MUST NOT collapse to "looks fresh" because the caller
/// guarantees a foreign or corrupt file is refused before any pragma mutates
/// the on-disk header.
pub fn classify(conn: &Connection) -> Result<StoreKind, rusqlite::Error> {
    let app_id: i64 = conn.query_row("pragma application_id", [], |r| r.get(0))?;
    if app_id == i64::from(migrations::APPLICATION_ID) {
        return Ok(StoreKind::Ours);
    }
    let table_count: i64 = conn.query_row("select count(*) from sqlite_master", [], |r| r.get(0))?;
    if app_id == 0 && table_count == 0 {
        Ok(StoreKind::Fresh)
    } else {
        Ok(StoreKind::Foreign)
    }
}

/// Apply connection and file pragmas required by the Repository Store.
///
/// `journal_mode` persists in the file header; `foreign_keys` and
/// `busy_timeout` are connection-scoped and have to be set on every open.
///
/// SQLite silently downgrades `journal_mode = wal` to `delete` or `memory` on
/// filesystems that don't support the shared-memory mmap WAL requires (some
/// network mounts, certain Docker overlays). Read the mode back and refuse
/// rather than ship a store whose durability contract doesn't match what
/// `tk init` advertised on stdout.
///
/// **Contract**: this helper is for on-disk Repository Stores. A `:memory:`
/// connection cannot use WAL and will be refused here — that's deliberate, so
/// tests that need an in-memory store skip this helper and apply the matching
/// pragmas directly (see `tests` modules in `store::migrations`).
fn configure_for_ticket_store(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.pragma_update(None, "journal_mode", "wal")?;
    let mode: String = conn.query_row("pragma journal_mode", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "journal_mode could not be set to wal (current: {mode})"
            )),
        ));
    }
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("pragma foreign_keys = on")?;
    Ok(())
}

/// Seed `store_config.display_prefix` from the repository basename when the
/// store does not already carry an explicit prefix.
///
/// `store_config.key` is the primary key, so `INSERT OR IGNORE` collapses the
/// select-then-insert dance into a single atomic write and preserves
/// idempotency without swallowing transient SQLite errors (the earlier
/// `.ok()` on the select did the wrong thing on `SQLITE_BUSY` / I/O errors).
fn seed_display_prefix(conn: &Connection, paths: &DiscoveredPaths) -> Result<(), rusqlite::Error> {
    let basename = paths
        .toplevel
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let prefix = display_prefix::derive(basename);
    conn.execute(
        "insert or ignore into store_config(key, value) values ('display_prefix', ?1)",
        rusqlite::params![prefix],
    )?;
    Ok(())
}

/// Create the `tk/` directory under `<git-common-dir>` if it does not already
/// exist. Returns `true` when we created it, `false` when it was already
/// present.
fn ensure_tk_dir(path: &Path) -> std::io::Result<bool> {
    match fs::create_dir(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => {
            // Mirror Zig's createDirPath: try to create parents too.
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|_| err)?;
                match fs::create_dir(path) {
                    Ok(()) => Ok(true),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                    Err(e) => Err(e),
                }
            } else {
                Err(err)
            }
        }
    }
}

/// Tighten a freshly-created Repository Store directory on platforms that
/// expose Unix-style permissions.
fn set_dir_mode_0700(path: &Path) -> std::io::Result<()> {
    if platform::IS_WINDOWS {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(path, perms)?;
    }
    let _ = path; // keep `path` used on non-unix non-windows targets.
    Ok(())
}

// ---- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rusqlite::Connection;
    use std::path::PathBuf;

    /// Materialize a fake `.git` repository under a tempdir, suitable for
    /// `tk init` to point at via a faked `git rev-parse` response.
    struct TmpStore {
        // Held to keep the tempdir alive across the test body; the path is
        // accessed through `toplevel` / `common_dir`.
        #[allow(dead_code)]
        tmp: tempfile::TempDir,
        toplevel: PathBuf,
        common_dir: PathBuf,
    }

    impl TmpStore {
        fn new(repo_name: &str) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let toplevel = tmp.path().join(repo_name);
            let common = toplevel.join(".git");
            std::fs::create_dir_all(&common).unwrap();
            TmpStore {
                tmp,
                toplevel,
                common_dir: common,
            }
        }

        fn rev_parse_stdout(&self) -> Vec<u8> {
            format!(
                "{}\n{}\n",
                self.common_dir.display(),
                self.toplevel.display()
            )
            .into_bytes()
        }

        fn db_path(&self) -> PathBuf {
            self.common_dir.join("tk").join("tk.db")
        }
    }

    struct Harness<'a> {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
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
                runner: FakeRunner::new(),
                clock: FakeClock::new(1_778_457_600_000),
                rng: StdRng::seed_from_u64(0),
                cwd,
            }
        }

        fn deps(&mut self) -> Deps<'_> {
            Deps {
                stdout: &mut self.stdout,
                stderr: &mut self.stderr,
                runner: &self.runner,
                clock: &self.clock,
                rng: &mut self.rng,
                cwd: self.cwd,
            }
        }
    }

    #[test]
    fn returns_exit_1_with_diagnostic_when_not_in_a_git_repo() {
        let cwd = std::env::current_dir().unwrap();
        let mut h = Harness::new(&cwd);
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 128,
                stdout: Vec::new(),
                stderr: b"fatal: not a git repository (or any of the parent directories): .git\n"
                    .to_vec(),
            },
        );

        let code = run(h.deps(), Args {});
        assert_eq!(code, 1);
        assert!(h.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&h.stderr);
        assert!(stderr.contains("git repository"), "stderr = {stderr:?}");
    }

    #[test]
    fn empty_stderr_git_failure_falls_back_to_default() {
        let cwd = std::env::current_dir().unwrap();
        let mut h = Harness::new(&cwd);
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 128,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
        let code = run(h.deps(), Args {});
        assert_eq!(code, 1);
        let stderr = String::from_utf8_lossy(&h.stderr);
        assert!(stderr.contains(messages::GIT_OUTSIDE_DEFAULT));
    }

    // `--help` and `--version` rendering moved to clap-derive in cli.rs;
    // covered by cli-level tests or scenario fixtures rather than per-command.

    #[test]
    fn success_creates_store_applies_migrations_seeds_prefix() {
        let store = TmpStore::new("my-test-repo");
        let cwd = std::env::current_dir().unwrap();
        let mut h = Harness::new(&cwd);
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );

        let code = run(h.deps(), Args {});
        assert_eq!(code, 0);
        let stdout = String::from_utf8_lossy(&h.stdout);
        assert!(stdout.contains(messages::INIT_SUCCESS_FRESH));
        assert!(h.stderr.is_empty());

        let conn = Connection::open(store.db_path()).unwrap();
        let journal: String = conn
            .query_row("pragma journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal, "wal");

        let prefix: String = conn
            .query_row(
                "select value from store_config where key = 'display_prefix'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prefix, "my-test-repo");

        drop(store); // keep tmp alive until here
    }

    #[test]
    fn idempotent_on_second_run_preserves_externally_set_prefix() {
        let store = TmpStore::new("my-test-repo");
        let cwd = std::env::current_dir().unwrap();

        // First run.
        {
            let mut h = Harness::new(&cwd);
            h.runner.expect(
                &["git", "rev-parse"],
                RunOutput {
                    exit_code: 0,
                    stdout: store.rev_parse_stdout(),
                    stderr: Vec::new(),
                },
            );
            assert_eq!(run(h.deps(), Args {}), 0);
        }

        // Overwrite prefix to assert init's idempotency doesn't clobber it.
        {
            let conn = Connection::open(store.db_path()).unwrap();
            conn.execute(
                "update store_config set value = ?1 where key = 'display_prefix'",
                rusqlite::params!["sentinel-value"],
            )
            .unwrap();
        }

        // Second run.
        {
            let mut h = Harness::new(&cwd);
            h.runner.expect(
                &["git", "rev-parse"],
                RunOutput {
                    exit_code: 0,
                    stdout: store.rev_parse_stdout(),
                    stderr: Vec::new(),
                },
            );
            let code = run(h.deps(), Args {});
            assert_eq!(code, 0);
            let stdout = String::from_utf8_lossy(&h.stdout);
            assert!(stdout.contains(messages::INIT_SUCCESS_EXISTING));
        }

        let conn = Connection::open(store.db_path()).unwrap();
        let prefix: String = conn
            .query_row(
                "select value from store_config where key = 'display_prefix'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prefix, "sentinel-value");
        drop(store);
    }

    #[test]
    fn classify_fresh_db_is_fresh() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(classify(&conn).unwrap(), StoreKind::Fresh);
    }

    #[test]
    fn classify_after_apply_all_is_ours() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        assert_eq!(classify(&conn).unwrap(), StoreKind::Ours);
    }

    #[test]
    fn classify_foreign_tables_is_foreign() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table other_app(x integer)")
            .unwrap();
        assert_eq!(classify(&conn).unwrap(), StoreKind::Foreign);
    }

    #[test]
    fn surfaces_foreign_diagnostic_on_stderr() {
        let store = TmpStore::new("my-test-repo");
        std::fs::create_dir_all(store.common_dir.join("tk")).unwrap();

        // Plant a SQLite file with foreign tables at the expected DB path.
        {
            let conn = Connection::open(store.db_path()).unwrap();
            conn.execute_batch("create table other_app(x integer)")
                .unwrap();
        }

        let cwd = std::env::current_dir().unwrap();
        let mut h = Harness::new(&cwd);
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );

        let code = run(h.deps(), Args {});
        assert_eq!(code, 1);
        let stderr = String::from_utf8_lossy(&h.stderr);
        assert!(stderr.contains(messages::INIT_REFUSE_FOREIGN));
        drop(store);
    }

    #[test]
    fn rejects_a_store_created_by_a_future_tk_version() {
        let store = TmpStore::new("my-test-repo");
        std::fs::create_dir_all(store.common_dir.join("tk")).unwrap();
        {
            let mut conn = Connection::open(store.db_path()).unwrap();
            migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
            conn.execute(
                "insert into schema_migrations(version, applied_at) values (?1, ?2)",
                rusqlite::params![999_i64, "2099-01-01T00:00:00.000Z"],
            )
            .unwrap();
        }

        let cwd = std::env::current_dir().unwrap();
        let mut h = Harness::new(&cwd);
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
        let code = run(h.deps(), Args {});
        assert_eq!(code, 1);
        let stderr = String::from_utf8_lossy(&h.stderr);
        assert!(stderr.contains(messages::INIT_REFUSE_FUTURE_VERSION));
        drop(store);
    }

    #[test]
    fn surfaces_unparseable_rev_parse_output() {
        let cwd = std::env::current_dir().unwrap();
        let mut h = Harness::new(&cwd);
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
        let code = run(h.deps(), Args {});
        assert_eq!(code, 1);
        let stderr = String::from_utf8_lossy(&h.stderr);
        assert!(stderr.contains(messages::GIT_UNPARSEABLE));
    }
}
