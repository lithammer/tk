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
    use super::{association, repository};
    use crate::git::association as git;
    let root = association::stores_root(data_root)?;
    let common = association::canonical_common(paths)?;
    association::refuse_legacy(&common)?;
    let (id, _guard) = if let Some(id) = association::pointer(runner, cwd)? {
        (Some(id), None)
    } else {
        let guard = association::lock_init(&root)?;
        // Another init may have installed a pointer before this lock was acquired.
        (association::pointer(runner, cwd)?, Some(guard))
    };
    if let Some(id) = id {
        let path = association::validate(&root, &id, &common)?;
        let store = repository::open_database(&path, clock)?;
        configure_repository_store(store.conn())?;
        return Ok(Initialized::Existing(path));
    }
    let urls = git::remote_urls(runner, cwd).map_err(association::Error::from)?;
    association::refuse_recovery(&root, &common, &urls)?;
    let id = association::StoreId::generate(rng);
    let dir = association::reserve(&root, &id)?;
    let path = dir.join("tk.db");
    let mut conn = Connection::open(&path)?;
    configure_repository_store(&conn)?;
    super::migrations::apply_all(&mut conn, &clock.now_iso())?;
    seed_display_prefix(&conn, paths)?;
    conn.close().map_err(|(_, e)| e)?;
    let manifest = association::Manifest {
        version: 1,
        store_id: id,
        association: association::Association {
            git_common_dir: common,
        },
        evidence: association::Evidence {
            previous_git_common_dirs: Vec::new(),
            git_remote_urls: urls,
        },
    };
    association::publish(&dir, &manifest)?;
    git::install(runner, cwd, manifest.store_id.text()).map_err(association::Error::from)?;
    Ok(Initialized::Created(path))
}

/// Require WAL for an on-disk Repository Store (ADR-0053).
///
/// Read the journal mode back before reporting success. WAL is file state;
/// foreign keys and the busy timeout must be set on each connection.
/// An in-memory database cannot satisfy the WAL contract.
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
