//! Owns legacy source locks and reads through cutover and cleanup.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

use super::{
    Boundary, Error, Observer, Receipt, allowed, association, exists, fault, flush_dir, flush_file,
    regular,
};

/// Holds the Remote lock before source inspection or progress publication.
pub(super) struct LegacySource {
    path: PathBuf,
    remote: Option<File>,
}

impl LegacySource {
    pub(super) fn lock(path: &Path) -> Result<Self, Error> {
        Ok(Self {
            path: path.to_path_buf(),
            remote: lock_remote(path)?,
        })
    }

    pub(super) fn inspect(&self) -> Result<(), Error> {
        if !fs::symlink_metadata(&self.path)?.is_dir() {
            return Err(Error::Legacy);
        }
        regular(&self.path.join("tk.db"))?;
        super::super::recovery::inspect_database(&self.path.join("tk.db"))
            .map_err(|e| fault(e.to_string()))
    }

    pub(super) fn freeze(self) -> Result<Frozen, Error> {
        let path = self.path.join("tk.db");
        let path = path.as_path();
        regular(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if fs::metadata(path)?.nlink() != 1 {
                return Err(fault(
                    "legacy database has hard links; make an independent copy before migration",
                ));
            }
        }
        let file = File::open(path)?;
        let conn = sql(Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        ))?;
        sql(conn.busy_timeout(std::time::Duration::ZERO))?;
        sql(conn.execute_batch(
            "pragma locking_mode=exclusive; pragma journal_mode=delete; begin exclusive; commit;",
        ))?;
        let mode: String = sql(conn.query_row("pragma journal_mode", [], |row| row.get(0)))?;
        if mode != "delete" {
            return Err(fault("legacy database could not leave WAL mode"));
        }
        // Read-only inspectors may leave WAL sidecars behind. The successful
        // transition checkpointed the WAL; these files no longer own any data.
        for suffix in ["-wal", "-shm"] {
            let sidecar = path.with_file_name(format!("tk.db{suffix}"));
            if exists(&sidecar)? {
                regular(&sidecar)?;
                fs::remove_file(sidecar)?;
            }
        }
        Ok(Frozen {
            conn,
            file,
            source: self,
        })
    }

    fn resume_cleanup(self) -> Result<Cleanup, Error> {
        if exists(&self.path.join("tk.db"))? {
            Ok(Cleanup::Database(self.freeze()?))
        } else {
            Ok(Cleanup::DatabaseRemoved(self))
        }
    }
}

// Closing any descriptor for the database releases this process's POSIX
// locks. Field order closes SQLite before the fingerprint descriptor.
pub(super) struct Frozen {
    conn: Connection,
    file: File,
    source: LegacySource,
}

impl Frozen {
    pub(super) fn inventory(&self) -> Result<BTreeMap<PathBuf, String>, Error> {
        let source = &self.source.path;
        let database = &self.file;
        let remote = self.source.remote.as_ref();
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let name = PathBuf::from(entry.file_name());
            if name == Path::new("backups") && entry.file_type()?.is_dir() {
                for backup in fs::read_dir(entry.path())? {
                    let backup = backup?;
                    let name = name.join(backup.file_name());
                    files.insert(name, fingerprint(&backup.path())?);
                }
            } else if name == Path::new("tk.db") {
                regular(&entry.path())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let held = database.metadata()?;
                    let current = entry.metadata()?;
                    if held.dev() != current.dev()
                        || held.ino() != current.ino()
                        || current.nlink() != 1
                    {
                        return Err(fault("legacy database path changed while locked"));
                    }
                }
                files.insert(name, hash_file(database)?);
            } else if name == Path::new("remote.lock") {
                // Windows denies reads through a second handle while this lock is held.
                regular(&entry.path())?;
                let remote =
                    remote.ok_or_else(|| fault("legacy Remote lock appeared during migration"))?;
                files.insert(name, hash_file(remote)?);
            } else if allowed(&name) {
                files.insert(name, fingerprint(&entry.path())?);
            } else {
                return Err(fault(format!(
                    "unexpected legacy entry: {}",
                    entry.path().display()
                )));
            }
        }
        Ok(files)
    }

    pub(super) fn snapshot(
        &self,
        staged: &Path,
        files: &BTreeMap<PathBuf, String>,
    ) -> Result<(), Error> {
        let image = staged.join("tk.db");
        let image_text = image
            .to_str()
            .ok_or_else(|| fault("destination path is not UTF-8"))?;
        sql(self.conn.execute("vacuum into ?1", [image_text]))?;
        let destination = sql(Connection::open(&image))?;
        sql(destination.pragma_update(None, "journal_mode", "wal"))?;
        destination.close().map_err(|(_, e)| fault(e.to_string()))?;
        flush_file(&image)?;
        for name in files.keys().filter(|p| p.starts_with("backups")) {
            fs::copy(self.source.path.join(name), staged.join(name))?;
            flush_file(&staged.join(name))?;
        }
        Ok(())
    }
}

/// Cleanup may resume after the database was deleted but before its directory was.
enum Cleanup {
    Database(Frozen),
    DatabaseRemoved(LegacySource),
}

impl Cleanup {
    fn inventory(&self) -> Result<BTreeMap<PathBuf, String>, Error> {
        match self {
            Self::Database(source) => source.inventory(),
            Self::DatabaseRemoved(source) => {
                // The database is removed last; only an empty directory may remain.
                let entries = fs::read_dir(&source.path)?.collect::<Result<Vec<_>, _>>()?;
                if !entries.is_empty() {
                    return Err(fault("legacy database is missing but other files remain"));
                }
                Ok(BTreeMap::new())
            }
        }
    }
}

pub(super) fn cleanup(source: &Path, receipt: &Receipt, observe: Observer) -> Result<(), Error> {
    if !exists(source)? {
        return Ok(());
    }
    if !fs::symlink_metadata(source)?.is_dir() {
        return Err(fault("legacy directory was replaced; cleanup refused"));
    }
    let remaining = LegacySource::lock(source)?.resume_cleanup()?;
    let current = remaining.inventory()?;
    if current
        .iter()
        .any(|(name, hash)| receipt.files.get(name) != Some(hash))
    {
        return Err(fault(
            "legacy files changed after cutover; preserve both Stores and restore manually",
        ));
    }
    observe(Boundary::Cleanup)?;
    // SQLite's Windows VFS denies delete sharing. Old processes must remain
    // stopped until cleanup finishes, including this close-to-delete gap.
    drop(remaining);
    for name in current
        .keys()
        .filter(|name| name.as_path() != Path::new("tk.db"))
    {
        fs::remove_file(source.join(name))?;
        observe(Boundary::CleanedFile)?;
    }
    if exists(&source.join("backups"))? {
        fs::remove_dir(source.join("backups"))?;
    }
    if exists(&source.join("tk.db"))? {
        fs::remove_file(source.join("tk.db"))?;
    }
    fs::remove_dir(source)?;
    flush_dir(source.parent().unwrap())?;
    observe(Boundary::Removed)?;
    Ok(())
}

fn fingerprint(path: &Path) -> Result<String, Error> {
    regular(path)?;
    hash_file(&File::open(path)?)
}

fn hash_file(mut file: &File) -> Result<String, Error> {
    file.rewind()?;
    let mut hash = Sha256::new();
    let mut buf = [0; 16384];
    loop {
        let len = file.read(&mut buf)?;
        if len == 0 {
            break;
        }
        hash.update(&buf[..len]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn lock_remote(source: &Path) -> Result<Option<File>, Error> {
    let path = source.join("remote.lock");
    if !exists(&path)? {
        return Ok(None);
    }
    regular(&path)?;
    association::lock_file(&path, true).map(Some)
}

fn sql<T>(result: rusqlite::Result<T>) -> Result<T, Error> {
    result.map_err(|e| fault(e.to_string()))
}
