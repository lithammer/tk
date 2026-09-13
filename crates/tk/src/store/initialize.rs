//! Repository Store creation and explicit recovery (ADR-0053).
use super::display_prefix;
use crate::git::discovery::DiscoveredPaths;
use rusqlite::Connection;
use std::path::PathBuf;

/// Init reports whether it published a Store or opened the existing association.
pub enum Initialized {
    /// A new Store was published and its Git pointer installed.
    Created {
        path: PathBuf,
        missing: Vec<String>,
    },
    /// The current Store Association and database validated successfully.
    Existing(PathBuf),
    /// The manifest and repository-local pointer now name this association.
    Attached(PathBuf),
    Recovery(super::recovery::Report),
}

#[derive(Clone, Copy)]
pub enum Mode<'a> {
    Plain,
    Attach(&'a str),
    New,
}

/// Hold the lifecycle lock through inspection, publication, and pointer writes.
pub fn initialize(
    runner: &dyn crate::proc::ProcRunner,
    cwd: &std::path::Path,
    clock: &dyn crate::clock::Clock,
    rng: &mut dyn rand::Rng,
    data_root: Option<&std::path::Path>,
    paths: &DiscoveredPaths,
    mode: Mode<'_>,
) -> Result<Initialized, super::repository::OpenError> {
    use super::{association, repository};
    use crate::git::association as git;
    let root = association::stores_root(data_root)?;
    let common = association::canonical_common(paths)?;
    association::refuse_legacy(&common)?;
    let _guard = association::lock_init(&root)?;
    let pointers = git::pointers(runner, cwd).map_err(association::Error::from)?;
    let pointer = association::parse_pointer(&pointers);
    if let Ok(Some(id)) = &pointer {
        if association::validate(&root, id, &common).is_ok() {
            let guard = association::lock_store(&root.join(id.text()), false)?;
            let path = association::validate(&root, id, &common)?;
            if super::recovery::inspect_database(&path).is_ok() {
                if !matches!(mode, Mode::Plain) {
                    return Err(association::Error::Healthy.into());
                }
                let store = repository::open_database(&path, clock)?;
                configure_repository_store(store.conn())?;
                drop(store);
                drop(guard);
                return Ok(Initialized::Existing(path));
            }
        }
    }
    let urls = git::remote_urls(runner, cwd).map_err(association::Error::from)?;
    if let Mode::Attach(id) = mode {
        let id = association::StoreId::try_from(id.to_owned())?;
        let dir = root.join(id.text());
        let mut manifest = association::read_manifest(&dir)?;
        let _store_guard = association::lock_store(&dir, true)?;
        super::recovery::released(runner, &manifest, &common)?;
        super::recovery::inspect_database(&dir.join("tk.db"))?;
        if manifest.association.git_common_dir != common {
            manifest
                .evidence
                .previous_git_common_dirs
                .push(manifest.association.git_common_dir.clone());
        }
        manifest.association.git_common_dir = common;
        manifest.evidence.previous_git_common_dirs.sort();
        manifest.evidence.previous_git_common_dirs.dedup();
        manifest.evidence.git_remote_urls.extend(urls);
        manifest.evidence.git_remote_urls.sort();
        manifest.evidence.git_remote_urls.dedup();
        association::replace(&dir, &manifest, rng)?;
        git::replace(runner, cwd, id.text()).map_err(association::Error::from)?;
        association::validate(&root, &id, &manifest.association.git_common_dir)?;
        return Ok(Initialized::Attached(dir.join("tk.db")));
    }
    if matches!(mode, Mode::Plain) {
        let candidates = super::recovery::discover(runner, &root, &pointers, &common, &urls)?;
        if !pointers.is_empty() || !candidates.is_empty() {
            return Ok(Initialized::Recovery(super::recovery::Report {
                candidates,
                pointer_error: pointer.err(),
            }));
        }
    }
    let missing: Vec<_> = pointers
        .iter()
        .filter(|id| {
            association::StoreId::try_from((*id).clone()).is_ok()
                && std::fs::symlink_metadata(root.join(id))
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        })
        .cloned()
        .collect();
    let id = association::StoreId::generate(rng);
    if pointers.iter().any(|previous| previous == id.text()) {
        return Err(association::Error::Collision.into());
    }
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
    if matches!(mode, Mode::New) {
        git::replace(runner, cwd, manifest.store_id.text()).map_err(association::Error::from)?;
    } else {
        git::install(runner, cwd, manifest.store_id.text()).map_err(association::Error::from)?;
    }
    Ok(Initialized::Created { path, missing })
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
