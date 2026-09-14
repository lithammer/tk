//! Resumable legacy Store cutover. The Git pointer selects authority (ADR-0053).
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::association::{self, Error, StoreId};
use crate::{git::association as git, proc::ProcRunner};

mod source;

/// Migration checkpoints exposed to the command harness.
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

/// An error stops migration at the current checkpoint.
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

/// Opening during cleanup requires matching source progress and destination receipt.
pub(super) fn access(root: &Path, common: &Path, id: &StoreId) -> Result<(), Error> {
    let dir = root.join(id.text());
    if exists(&common.join("tk"))? || exists(&dir.join("migration.json"))? {
        let progress = read_progress(root, common)?;
        if &progress.store_id != id {
            return Err(Error::Legacy);
        }
        read_receipt(&dir, &progress)?;
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

/// Requires init.lock; old tk processes must stay stopped through cleanup.
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
    let legacy = source::LegacySource::lock(&source)?;
    let record = common.join("tk-migration.json");
    let pointers = git::pointers(runner, cwd)?;
    let progress = if exists(&record)? {
        read_progress(root, common)?
    } else {
        if !pointers.is_empty() {
            return Err(Error::Legacy);
        }
        legacy.inspect()?;
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
        let receipt = read_receipt(&dir, &progress)?;
        flush_pointer(common)?;
        drop(legacy);
        source::cleanup(&source, &receipt, observe)?;
        finish(&record, &dir, observe)?;
        return Ok(dir.join("tk.db"));
    }
    if !pointers.is_empty() {
        return Err(fault("Git pointer disagrees with migration progress"));
    }
    legacy.inspect()?;
    let frozen = legacy.freeze()?;
    let files = frozen.inventory()?;
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
        read_receipt(prior, &progress)?;
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
    frozen.snapshot(&staged, &receipt.files)?;
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
    if frozen.inventory()? != receipt.files {
        return Err(fault("legacy files changed during staging"));
    }
    git::install(runner, cwd, manifest.store_id.text())?;
    association::validate(root, &manifest.store_id, common)?;
    flush_pointer(common)?;
    observe(Boundary::Pointed)?;
    drop(frozen);
    source::cleanup(&source, &receipt, observe)?;
    finish(&record, &dir, observe)?;
    Ok(dir.join("tk.db"))
}

pub(super) fn pending(common: &Path) -> Result<bool, Error> {
    Ok(exists(&common.join("tk"))? || exists(&common.join("tk-migration.json"))?)
}

fn fault(message: impl Into<String>) -> Error {
    Error::Migration(message.into())
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

fn read_receipt(dir: &Path, progress: &Progress) -> Result<Receipt, Error> {
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
    fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn flush_pointer(common: &Path) -> Result<(), Error> {
    regular(&common.join("config"))?;
    flush_file(&common.join("config"))?;
    flush_dir(common)
}
