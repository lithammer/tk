//! Fresh Repository Store creation (ADR-0053).
use super::display_prefix;
use crate::git::discovery::DiscoveredPaths;
use rusqlite::Connection;
use std::path::PathBuf;

/// Init reports whether it published a Store or opened the existing association.
pub enum Initialized {
    /// A new Store was published and its Git pointer installed.
    Created(PathBuf),
    /// The current Store Association and database validated successfully.
    Existing(PathBuf),
}

/// Validate a healthy Store or publish a fresh one before installing its pointer.
pub fn initialize(
    runner: &dyn crate::proc::ProcRunner,
    cwd: &std::path::Path,
    clock: &dyn crate::clock::Clock,
    rng: &mut dyn rand::Rng,
    data_root: Option<&std::path::Path>,
    paths: &DiscoveredPaths,
) -> Result<Initialized, super::repository::OpenError> {
    use super::{association as a, repository};
    use crate::git::association as git;
    let root = a::stores_root(data_root)?;
    let common = a::canonical_common(paths)?;
    a::refuse_legacy(&common)?;
    let (id, _guard) = if let Some(id) = a::pointer(runner, cwd)? {
        (Some(id), None)
    } else {
        let guard = a::lock_init(&root)?;
        // Another init may have installed a pointer before this lock was acquired.
        (a::pointer(runner, cwd)?, Some(guard))
    };
    if let Some(id) = id {
        let path = a::validate(&root, &id, &common)?;
        let store = repository::open_database(&path, clock)?;
        configure_repository_store(store.conn())?;
        return Ok(Initialized::Existing(path));
    }
    let urls = git::remote_urls(runner, cwd).map_err(a::Error::from)?;
    a::refuse_recovery(&root, &common, &urls)?;
    let id = a::StoreId::generate(rng);
    let dir = a::reserve(&root, &id)?;
    let path = dir.join("tk.db");
    let mut conn = Connection::open(&path)?;
    configure_repository_store(&conn)?;
    super::migrations::apply_all(&mut conn, &clock.now_iso())?;
    seed_display_prefix(&conn, paths)?;
    conn.close().map_err(|(_, e)| e)?;
    let manifest = a::Manifest {
        version: 1,
        store_id: id.clone(),
        association: a::Association {
            git_common_dir: common,
        },
        evidence: a::Evidence {
            previous_git_common_dirs: Vec::new(),
            git_remote_urls: urls,
        },
    };
    a::publish(&dir, &manifest)?;
    git::install(runner, cwd, id.text()).map_err(a::Error::from)?;
    Ok(Initialized::Created(path))
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
fn configure_repository_store(conn: &Connection) -> Result<(), rusqlite::Error> {
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

/// A fresh Store derives its Display ID prefix from the repository basename.
fn seed_display_prefix(conn: &Connection, paths: &DiscoveredPaths) -> Result<(), rusqlite::Error> {
    let basename = paths
        .toplevel
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let prefix = display_prefix::derive(basename);
    conn.execute(
        "insert into store_config(key, value) values ('display_prefix', ?1)",
        rusqlite::params![prefix],
    )?;
    Ok(())
}
