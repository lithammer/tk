//! Resumable legacy Store cutover. The Git pointer selects authority (ADR-0053).
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::association::{self, Error, StoreId};
use crate::{git::association as git, proc::ProcRunner};

/// Durable boundaries exposed to the command harness, never environment policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    Recorded,
    Reserved,
    Staged,
    Validated,
    Published,
    Pointed,
    Cleanup,
    CleanedFile,
    Removed,
    Finished,
}

pub type Observer = fn(Boundary) -> std::io::Result<()>;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Progress {
    version: u32,
    store_id: StoreId,
    token: StoreId,
    common: PathBuf,
    root: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    progress: Progress,
    files: BTreeMap<PathBuf, String>,
}

/// A pointer can open a migrated Store during cleanup only with both receipts.
pub(super) fn access(root: &Path, common: &Path, id: &StoreId) -> Result<(), Error> {
    let dir = root.join(id.text());
    if exists(&common.join("tk"))? || exists(&dir.join("migration.json"))? {
        let progress = read_progress(root, common)?;
        if &progress.store_id != id {
            return Err(Error::Legacy);
        }
        receipt(&dir, &progress)?;
    }
    Ok(())
}

/// Pending migration images cannot be adopted as orphaned Stores.
pub(super) fn refuse_pending(dir: &Path) -> Result<(), Error> {
    if exists(&dir.join("migration.json"))? {
        return Err(fault(
            "migration is pending at its source Git Common Directory",
        ));
    }
    Ok(())
}

/// Run under init.lock. Quiescence must cover old binaries and Windows cleanup.
pub(super) fn migrate(
    runner: &dyn ProcRunner,
    cwd: &Path,
    root: &Path,
    common: &Path,
    rng: &mut dyn rand::Rng,
    observe: Observer,
) -> Result<PathBuf, Error> {
    let _lock = association::lock_file(&common.join("tk-migration.lock"), true)?;
    let source = common.join("tk");
    let legacy_guard = lock_remote(&source)?;
    let record = common.join("tk-migration.json");
    let pointers = git::pointers(runner, cwd)?;
    let progress = if exists(&record)? {
        read_progress(root, common)?
    } else {
        if !pointers.is_empty() {
            return Err(Error::Legacy);
        }
        inspect_source(&source)?;
        let progress = Progress {
            version: 1,
            store_id: StoreId::generate(rng),
            token: StoreId::generate(rng),
            common: common.to_path_buf(),
            root: fs::canonicalize(root)?,
        };
        if exists(&root.join(progress.store_id.text()))? || exists(&stage(&progress))? {
            return Err(Error::Collision);
        }
        write_json(&record, &progress)?;
        flush_dir(common)?;
        progress
    };
    observe(Boundary::Recorded)?;
    let dir = root.join(progress.store_id.text());
    if pointers == [progress.store_id.text()] {
        let _guard = association::lock_store(&dir, true)?;
        association::validate(root, &progress.store_id, common)?;
        super::recovery::inspect_database(&dir.join("tk.db")).map_err(|e| fault(e.to_string()))?;
        if !exists(&source)? && !exists(&dir.join("migration.json"))? {
            finish(&record, &dir, observe)?;
            return Ok(dir.join("tk.db"));
        }
        let receipt = receipt(&dir, &progress)?;
        flush_pointer(common)?;
        drop(legacy_guard);
        cleanup(&source, &receipt, observe)?;
        finish(&record, &dir, observe)?;
        return Ok(dir.join("tk.db"));
    }
    if !pointers.is_empty() {
        return Err(fault("Git pointer disagrees with migration progress"));
    }
    inspect_source(&source)?;
    let mut frozen = freeze(&source.join("tk.db"))?;
    let files = inventory(&source, &mut frozen.file)?;
    let staged = stage(&progress);
    for prior in [&staged, &dir] {
        if !exists(prior)? {
            continue;
        }
        if prior == &staged
            && fs::symlink_metadata(prior)?.is_dir()
            && !exists(&prior.join("migration.json"))?
        {
            let entries = fs::read_dir(prior)?.collect::<Result<Vec<_>, _>>()?;
            if entries.iter().all(|e| {
                e.file_name() == "migration.pending" && e.file_type().is_ok_and(|t| t.is_file())
            }) {
                for entry in entries {
                    fs::remove_file(entry.path())?;
                }
                fs::remove_dir(prior)?;
                continue;
            }
        }
        receipt(prior, &progress)?;
        if prior == &dir {
            let guard = association::lock_store(prior, true)?;
            drop(guard);
        }
        // No pointer has ever made this image authoritative. The source
        // is locked and intact; retries must replace stale snapshots.
        fs::remove_dir_all(prior)?;
        flush_dir(prior.parent().unwrap())?;
    }
    association::create_private_dirs(staged.parent().unwrap())?;
    if !fs::symlink_metadata(staged.parent().unwrap())?.is_dir() {
        return Err(fault("migration staging directory is not a directory"));
    }
    fs::create_dir(&staged)?;
    crate::platform::set_dir_mode_0700(&staged)?;
    observe(Boundary::Reserved)?;
    let receipt = Receipt { progress, files };
    write_json(&staged.join("migration.json"), &receipt)?;
    let backup_dir = staged.join("backups");
    fs::create_dir(&backup_dir)?;
    crate::platform::set_dir_mode_0700(&backup_dir)?;
    let image = staged.join("tk.db");
    let image_text = image
        .to_str()
        .ok_or_else(|| fault("destination path is not UTF-8"))?;
    sql(frozen.conn.execute("vacuum into ?1", [image_text]))?;
    let destination = sql(Connection::open(&image))?;
    sql(destination.pragma_update(None, "journal_mode", "wal"))?;
    destination.close().map_err(|(_, e)| fault(e.to_string()))?;
    flush_file(&image)?;
    for name in receipt.files.keys().filter(|p| p.starts_with("backups")) {
        fs::copy(source.join(name), staged.join(name))?;
        flush_file(&staged.join(name))?;
    }
    observe(Boundary::Staged)?;
    super::recovery::inspect_database(&image).map_err(|e| fault(e.to_string()))?;
    for entry in fs::read_dir(&backup_dir)? {
        super::recovery::inspect_database(&entry?.path()).map_err(|e| fault(e.to_string()))?;
    }
    let manifest = association::Manifest {
        version: 1,
        store_id: receipt.progress.store_id.clone(),
        association: association::Association {
            git_common_dir: common.to_path_buf(),
        },
        evidence: association::Evidence {
            previous_git_common_dirs: Vec::new(),
            git_remote_urls: git::remote_urls(runner, cwd)?,
        },
    };
    association::publish(&staged, &manifest)?;
    association::read_manifest(&staged)?;
    flush_dir(&backup_dir)?;
    flush_dir(&staged)?;
    observe(Boundary::Validated)?;
    fs::rename(&staged, &dir)?;
    flush_dir(staged.parent().unwrap())?;
    // The data root may have been created by this init. Persist its ancestor
    // entries as well as the Store before deleting the only legacy copy.
    for ancestor in root.ancestors() {
        flush_dir(ancestor)?;
    }
    association::validate(root, &manifest.store_id, common)?;
    let _guard = association::lock_store(&dir, true)?;
    observe(Boundary::Published)?;
    if inventory(&source, &mut frozen.file)? != receipt.files {
        return Err(fault("legacy files changed during staging"));
    }
    git::install(runner, cwd, manifest.store_id.text())?;
    association::validate(root, &manifest.store_id, common)?;
    flush_pointer(common)?;
    observe(Boundary::Pointed)?;
    drop(frozen);
    drop(legacy_guard);
    cleanup(&source, &receipt, observe)?;
    finish(&record, &dir, observe)?;
    Ok(dir.join("tk.db"))
}

pub(super) fn pending(common: &Path) -> Result<bool, Error> {
    Ok(exists(&common.join("tk"))? || exists(&common.join("tk-migration.json"))?)
}

fn fault(message: impl Into<String>) -> Error {
    Error::Migration(message.into())
}

fn sql<T>(result: rusqlite::Result<T>) -> Result<T, Error> {
    result.map_err(|e| fault(e.to_string()))
}

fn exists(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn read_progress(root: &Path, common: &Path) -> Result<Progress, Error> {
    let path = common.join("tk-migration.json");
    regular(&path)?;
    let progress: Progress = serde_json::from_slice(&fs::read(path)?)
        .map_err(|_| fault("migration progress is invalid; restore metadata manually"))?;
    if progress.version != 1
        || progress.common != common
        || progress.root != fs::canonicalize(root)?
    {
        return Err(fault(
            "migration progress disagrees with the current locations",
        ));
    }
    Ok(progress)
}

fn receipt(dir: &Path, progress: &Progress) -> Result<Receipt, Error> {
    if !fs::symlink_metadata(dir)?.is_dir() {
        return Err(fault("migration destination is not a directory"));
    }
    regular(&dir.join("migration.json"))?;
    let receipt: Receipt = serde_json::from_slice(&fs::read(dir.join("migration.json"))?)
        .map_err(|_| fault("migration receipt is invalid; restore metadata manually"))?;
    if receipt.progress != *progress || receipt.files.keys().any(|name| !allowed(name)) {
        return Err(fault("destination does not belong to this migration"));
    }
    Ok(receipt)
}

fn stage(progress: &Progress) -> PathBuf {
    progress
        .root
        .join(".migrations")
        .join(progress.store_id.text())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| fault(e.to_string()))?;
    let pending = path.with_extension("pending");
    if exists(&pending)? {
        regular(&pending)?;
        fs::remove_file(&pending)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&pending)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    if exists(path)? {
        return Err(fault("migration metadata already exists"));
    }
    fs::rename(pending, path)?;
    flush_dir(path.parent().unwrap())?;
    Ok(())
}

fn regular(path: &Path) -> Result<(), Error> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(fault(format!(
            "expected a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn allowed(name: &Path) -> bool {
    matches!(
        name.to_str(),
        Some("tk.db" | "remote.lock" | "tk.db-journal")
    ) || (name.parent() == Some(Path::new("backups")) && name.file_name().is_some())
}

fn inspect_source(source: &Path) -> Result<(), Error> {
    if !fs::symlink_metadata(source)?.is_dir() {
        return Err(Error::Legacy);
    }
    regular(&source.join("tk.db"))?;
    super::recovery::inspect_database(&source.join("tk.db")).map_err(|e| fault(e.to_string()))
}

// Closing any descriptor for the database releases this process's POSIX
// locks. Keep the fingerprint descriptor until SQLite has closed its own.
struct Frozen {
    conn: Connection,
    file: File,
}

fn freeze(path: &Path) -> Result<Frozen, Error> {
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
    Ok(Frozen { conn, file })
}

fn inventory(source: &Path, database: &mut File) -> Result<BTreeMap<PathBuf, String>, Error> {
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
            database.rewind()?;
            files.insert(name, hash_file(database)?);
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

fn fingerprint(path: &Path) -> Result<String, Error> {
    regular(path)?;
    hash_file(&mut File::open(path)?)
}

fn hash_file(file: &mut File) -> Result<String, Error> {
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

fn cleanup(source: &Path, receipt: &Receipt, observe: Observer) -> Result<(), Error> {
    if !exists(source)? {
        return Ok(());
    }
    if !fs::symlink_metadata(source)?.is_dir() {
        return Err(fault("legacy directory was replaced; cleanup refused"));
    }
    let remote = lock_remote(source)?;
    let mut frozen = if exists(&source.join("tk.db"))? {
        Some(freeze(&source.join("tk.db"))?)
    } else {
        None
    };
    let current = if let Some(frozen) = &mut frozen {
        inventory(source, &mut frozen.file)?
    } else {
        // The database is removed last; only an empty directory may remain.
        let entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
        if !entries.is_empty() {
            return Err(fault("legacy database is missing but other files remain"));
        }
        BTreeMap::new()
    };
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
    drop(frozen);
    drop(remote);
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

fn finish(record: &Path, dir: &Path, observe: Observer) -> Result<(), Error> {
    if exists(&dir.join("migration.json"))? {
        fs::remove_file(dir.join("migration.json"))?;
    }
    flush_dir(dir)?;
    observe(Boundary::Finished)?;
    fs::remove_file(record)?;
    flush_dir(record.parent().unwrap())?;
    Ok(())
}

fn flush_file(path: &Path) -> Result<(), Error> {
    OpenOptions::new().write(true).open(path)?.sync_all()?;
    Ok(())
}

fn flush_dir(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn flush_pointer(common: &Path) -> Result<(), Error> {
    regular(&common.join("config"))?;
    flush_file(&common.join("config"))?;
    flush_dir(common)
}

fn lock_remote(source: &Path) -> Result<Option<File>, Error> {
    let path = source.join("remote.lock");
    if !exists(&path)? {
        return Ok(None);
    }
    regular(&path)?;
    association::lock_file(&path, true).map(Some)
}
