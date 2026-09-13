//! Manifest discovery and explicit Store Association recovery (ADR-0053).
use std::fs;
use std::path::Path;

use super::association::{self, Error, Manifest, StoreId};
use crate::git::association as git;
use crate::proc::ProcRunner;

pub struct Candidate {
    pub id: String,
    pub facts: Vec<Fact>,
    manifest: Result<Manifest, Error>,
    pub available: Result<(), String>,
    rank: usize,
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
            candidates.push(Candidate {
                id,
                facts,
                manifest,
                available: Ok(()),
                rank,
            });
        }
    }
    for id in pointers {
        if StoreId::try_from(id.clone()).is_ok() && !candidates.iter().any(|c| &c.id == id) {
            candidates.push(Candidate {
                id: id.clone(),
                facts: vec![Fact::Referenced, Fact::MissingStore],
                manifest: Err(Error::Manifest),
                available: Err("missing Store; restore it from backup".into()),
                rank: 0,
            });
        }
    }
    candidates.sort_by(|a, b| (a.rank, &a.id).cmp(&(b.rank, &b.id)));
    for candidate in &mut candidates {
        candidate.available = candidate
            .manifest
            .as_ref()
            .map_err(ToString::to_string)
            .and_then(|m| {
                let dir = root.join(&candidate.id);
                let _guard = association::lock_store(&dir, false).map_err(|e| e.to_string())?;
                released(runner, m, common).map_err(|e| e.to_string())?;
                inspect_database(&dir.join("tk.db")).map_err(|e| e.to_string())
            });
    }
    Ok(candidates)
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
    Ok(())
}
