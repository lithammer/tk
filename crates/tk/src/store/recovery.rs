//! Manifest discovery and Store Association recovery (ADR-0053).
use std::fs;
use std::path::Path;

use super::association::{self, Error, Manifest, StoreId};
use crate::git::association as git;
use crate::proc::ProcRunner;

pub struct Candidate {
    pub id: String,
    pub facts: Vec<Fact>,
    pub available: Result<(), String>,
}

pub enum Fact {
    Referenced,
    CurrentPath(std::path::PathBuf),
    HistoricalPath(std::path::PathBuf),
    RemoteUrl(String),
    MissingStore,
}

pub struct Report {
    pub candidates: Vec<Candidate>,
    pub pointer_error: Option<Error>,
}

/// Rank manifest evidence, then inspect only shortlisted databases without migration.
pub(super) fn discover(
    runner: &dyn ProcRunner,
    root: &Path,
    pointers: &[String],
    common: &Path,
    urls: &[String],
) -> Result<Vec<Candidate>, Error> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if id == ".migrations" {
            continue;
        }
        let manifest = association::read_manifest(&entry.path());
        let mut facts = Vec::new();
        let mut rank = usize::MAX;
        if pointers.contains(&id) {
            rank = 0;
            facts.push(Fact::Referenced);
        }
        if let Ok(m) = &manifest {
            if m.association.git_common_dir == common {
                rank = rank.min(1);
                facts.push(Fact::CurrentPath(common.to_path_buf()));
            }
            for path in &m.evidence.previous_git_common_dirs {
                if path == common {
                    rank = rank.min(2);
                    facts.push(Fact::HistoricalPath(path.clone()));
                }
            }
            for url in &m.evidence.git_remote_urls {
                if urls.contains(url) {
                    rank = rank.min(3);
                    facts.push(Fact::RemoteUrl(url.clone()));
                }
            }
        }
        if !facts.is_empty() || manifest.is_err() {
            candidates.push((rank, id, facts, manifest));
        }
    }
    for id in pointers {
        if StoreId::try_from(id.clone()).is_ok()
            && !candidates
                .iter()
                .any(|(_, candidate_id, _, _)| candidate_id == id)
        {
            candidates.push((
                0,
                id.clone(),
                vec![Fact::Referenced, Fact::MissingStore],
                Err(Error::Manifest),
            ));
        }
    }
    candidates
        .sort_by(|(rank_a, id_a, _, _), (rank_b, id_b, _, _)| (rank_a, id_a).cmp(&(rank_b, id_b)));
    Ok(candidates
        .into_iter()
        .map(|(_, id, facts, manifest)| {
            let available = manifest.map_err(|e| e.to_string()).and_then(|manifest| {
                let dir = root.join(&id);
                let _guard = association::lock_store(&dir, false).map_err(|e| e.to_string())?;
                released(runner, &manifest, common).map_err(|e| e.to_string())?;
                inspect_database(&dir.join("tk.db")).map_err(|e| e.to_string())
            });
            Candidate {
                id,
                facts,
                available,
            }
        })
        .collect())
}

/// Missing ancestors are not proof of release: the volume may be unavailable.
/// A readable parent directory can confirm the final component is absent.
pub(super) fn released(
    runner: &dyn ProcRunner,
    manifest: &Manifest,
    common: &Path,
) -> Result<(), Error> {
    let former = &manifest.association.git_common_dir;
    if former == common {
        return Ok(());
    }
    match fs::symlink_metadata(former) {
        Ok(metadata) if metadata.is_dir() => {
            let pointers = git::former_pointers(runner, former).map_err(|_| Error::Ownership)?;
            if pointers.iter().any(|id| id == manifest.store_id.text()) {
                return Err(Error::AssociationMismatch);
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = former.parent().ok_or(Error::Ownership)?;
            let entries = fs::read_dir(parent).map_err(|_| Error::Ownership)?;
            for entry in entries {
                if entry.map_err(|_| Error::Ownership)?.file_name()
                    == former.file_name().ok_or(Error::Ownership)?
                {
                    return Err(Error::Ownership);
                }
            }
            Ok(())
        }
        _ => Err(Error::Ownership),
    }
}

/// Shortlisted databases are inspected read-only, without running migrations.
pub(super) fn inspect_database(path: &Path) -> Result<(), super::repository::OpenError> {
    inspect_connection(path).map(|_| ())
}

/// Hold the exclusive Store lock through inspection and repair to exclude
/// writes and new Store Backups. Read stored images without migrations.
pub(super) fn vacant(dir: &Path) -> Result<bool, super::repository::OpenError> {
    if !vacant_database(&dir.join("tk.db"))? {
        return Ok(false);
    }
    for entry in fs::read_dir(dir.join("backups")).map_err(Error::from)? {
        let entry = entry.map_err(Error::from)?;
        if !entry.file_type().map_err(Error::from)?.is_file() || !vacant_database(&entry.path())? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn inspect_connection(path: &Path) -> Result<rusqlite::Connection, super::repository::OpenError> {
    use super::{migrations, repository::OpenError};
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let app_id: i64 = conn.query_row("pragma application_id", [], |r| r.get(0))?;
    if app_id != i64::from(migrations::APPLICATION_ID) {
        return Err(OpenError::NotRepositoryStore);
    }
    let version = migrations::current_version(&conn)?;
    if version > i64::from(migrations::MAX_KNOWN_VERSION) {
        return Err(OpenError::FromFutureVersion);
    }
    if version == 0 {
        return Err(OpenError::NotRepositoryStore);
    }
    let check: String = conn.query_row("pragma quick_check", [], |r| r.get(0))?;
    if check != "ok" || conn.prepare("pragma foreign_key_check")?.exists([])? {
        return Err(OpenError::NotRepositoryStore);
    }
    Ok(conn)
}

fn vacant_database(path: &Path) -> Result<bool, super::repository::OpenError> {
    use super::{migrations, repository::OpenError};
    let conn = inspect_connection(path)?;
    let version = migrations::current_version(&conn)?;
    let mirrored: i64 = conn.query_row("pragma user_version", [], |row| row.get(0))?;
    let recorded: i64 = conn.query_row(
        "select count(*) from schema_migrations where version between 1 and ?1",
        [version],
        |row| row.get(0),
    )?;
    if mirrored != version || recorded != version {
        return Err(OpenError::NotRepositoryStore);
    }
    let tables = conn
        .prepare("select name from sqlite_schema where type = 'table'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for (name, since) in [
        ("schema_migrations", 1),
        ("sequences", 1),
        ("store_config", 1),
        ("items", 1),
        ("item_ids", 1),
        ("dependencies", 1),
        ("external_blockers", 1),
        ("mutations", 1),
        ("remotes", 1),
        ("sync_cursors", 1),
        ("former_backend_identities", 13),
        ("plan_members", 17),
    ] {
        if version >= since && !tables.iter().any(|table| table == name) {
            return Err(OpenError::NotRepositoryStore);
        }
    }
    for table in tables {
        let predicate = match table.as_str() {
            "schema_migrations" => "version < 1",
            "sequences" => {
                "value != 0 or name not in ('item_created_seq', 'display_seq', 'mutation_seq')"
            }
            "store_config" => "key != 'display_prefix'",
            _ => "1",
        };
        let quoted = table.replace('"', "\"\"");
        if conn
            .prepare(&format!(
                "select 1 from \"{quoted}\" where {predicate} limit 1"
            ))?
            .exists([])?
        {
            return Ok(false);
        }
    }
    Ok(true)
}
