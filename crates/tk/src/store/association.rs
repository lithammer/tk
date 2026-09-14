//! Durable Repository Store identity and association validation (ADR-0053).
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::git::{association as git, discovery::DiscoveredPaths};
use crate::proc::ProcRunner;
use serde::{Deserialize, Serialize};

/// Opaque 128-bit Repository Store identity, encoded as 32 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StoreId(String);

impl StoreId {
    /// Draw a Store identity using the same entropy source as Item identities.
    pub fn generate(rng: &mut dyn rand::Rng) -> Self {
        Self(super::repository::create::generate_internal_id(rng))
    }
    /// The exact directory and Git config spelling of the Store ID.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for StoreId {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        if value.len() == 32
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(value))
        } else {
            Err(Error::InvalidPointer)
        }
    }
}
impl From<StoreId> for String {
    fn from(id: StoreId) -> Self {
        id.0
    }
}

/// Manifest v1 owns Store identity and association; SQLite owns domain data.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub store_id: StoreId,
    pub association: Association,
    pub evidence: Evidence,
}
/// The one repository allowed to open this Store.
#[derive(Debug, Serialize, Deserialize)]
pub struct Association {
    pub git_common_dir: PathBuf,
}
/// Historical hints never authorize ordinary Store access.
#[derive(Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub previous_git_common_dirs: Vec<PathBuf>,
    pub git_remote_urls: Vec<String>,
}

/// Association failures refuse access before opening SQLite.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("platform-local data directory is unavailable or is not absolute")]
    DataRoot,
    #[error("repository-local tk.storeId must be exactly 32 lowercase hexadecimal characters")]
    InvalidPointer,
    #[error("multiple repository-local tk.storeId values; restore a single valid Store pointer")]
    DuplicatePointer,
    #[error(
        "legacy Repository Store data exists; stop all tk processes, then run 'tk init'; data was preserved"
    )]
    Legacy,
    #[error(
        "legacy Repository Store migration: {0}; keep old tk processes stopped and retry 'tk init'; data was preserved"
    )]
    Migration(String),
    #[error("Store evidence requires recovery; run 'tk init'; data was preserved")]
    Recovery,
    #[error("current repository has a healthy Store Association; attach/new refused")]
    Healthy,
    #[error("Store ownership is live or unknown; data was preserved")]
    Ownership,
    #[error(
        "Repository Store manifest is invalid or belongs to a newer tk version; restore metadata manually; data was preserved"
    )]
    Manifest,
    #[error("Repository Store belongs to another Git Common Directory; data was preserved")]
    AssociationMismatch,
    #[error("Store ID collision; the existing directory was preserved")]
    Collision,
    #[error("another Store lifecycle operation is running; retry when it finishes")]
    Busy,
    #[error(transparent)]
    Git(#[from] git::ConfigError),
    #[error("Repository Store filesystem error: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolve the platform root without a fallback or a tk-specific override.
pub fn stores_root(data_root: Option<&Path>) -> Result<PathBuf, Error> {
    Ok(data_root
        .filter(|p| p.is_absolute())
        .ok_or(Error::DataRoot)?
        .join("tk")
        .join("stores"))
}

/// Canonical filesystem identity is the association key, including symlink aliases.
pub fn canonical_common(paths: &DiscoveredPaths) -> Result<PathBuf, Error> {
    Ok(fs::canonicalize(&paths.git_common_dir)?)
}

/// Read the sole authoritative pointer without selecting a substitute Store.
pub fn pointer<R: ProcRunner + ?Sized>(runner: &R, cwd: &Path) -> Result<Option<StoreId>, Error> {
    parse_pointer(&git::pointers(runner, cwd)?)
}

pub(super) fn parse_pointer(values: &[String]) -> Result<Option<StoreId>, Error> {
    match values {
        [] => Ok(None),
        [value] => StoreId::try_from(value.clone()).map(Some),
        _ => Err(Error::DuplicatePointer),
    }
}

/// Validate all Store metadata before returning its database path.
pub fn validate(root: &Path, id: &StoreId, common: &Path) -> Result<PathBuf, Error> {
    let dir = root.join(id.text());
    let manifest = read_manifest(&dir)?;
    if manifest.association.git_common_dir != common {
        return Err(Error::AssociationMismatch);
    }
    Ok(dir.join("tk.db"))
}

/// Refuse legacy data even when no Git pointer exists.
pub fn refuse_legacy(common: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(common.join("tk")) {
        Ok(_) => Err(Error::Legacy),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Serialize fresh init across the data root while scanning and publishing Stores.
pub fn lock_init(root: &Path) -> Result<File, Error> {
    create_private_dirs(root)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.parent().unwrap().join("init.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(Error::Busy),
        Err(fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Reserve a fresh Store directory exclusively; a collision never opens its contents.
pub fn reserve(root: &Path, id: &StoreId) -> Result<PathBuf, Error> {
    let dir = root.join(id.text());
    match fs::create_dir(&dir) {
        Ok(()) => crate::platform::set_dir_mode_0700(&dir)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(Error::Collision),
        Err(e) => return Err(e.into()),
    }
    create_private_dirs(&dir.join("backups"))?;
    Ok(dir)
}

/// Publish complete metadata before installing Git's pointer; retain failed creations.
pub fn publish(dir: &Path, manifest: &Manifest) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|_| Error::Manifest)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("store.json"))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()?;
        File::open(dir.parent().unwrap())?.sync_all()?;
    }
    Ok(())
}

/// Create missing ancestors with owner-only access, preserving existing permissions.
pub fn create_private_dirs(path: &Path) -> Result<(), std::io::Error> {
    match fs::create_dir(path) {
        Ok(()) => crate::platform::set_dir_mode_0700(path),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                create_private_dirs(parent)?;
            }
            match fs::create_dir(path) {
                Ok(()) => crate::platform::set_dir_mode_0700(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

/// Check manifest structure before comparing it with the requested association.
pub(super) fn read_manifest(dir: &Path) -> Result<Manifest, Error> {
    if !fs::symlink_metadata(dir)?.file_type().is_dir() {
        return Err(Error::Manifest);
    }
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(dir.join("store.json"))?).map_err(|_| Error::Manifest)?;
    if dir.file_name() != Some(std::ffi::OsStr::new(manifest.store_id.text()))
        || manifest.version != 1
        || !manifest.association.git_common_dir.is_absolute()
        || manifest
            .evidence
            .previous_git_common_dirs
            .iter()
            .any(|p| !p.is_absolute())
    {
        return Err(Error::Manifest);
    }
    Ok(manifest)
}

/// Hold the returned guard through association validation and database use.
pub(super) fn lock_store(dir: &Path, exclusive: bool) -> Result<File, Error> {
    lock_file(&dir.join("association.lock"), exclusive)
}

/// Retain the returned descriptor through the protected filesystem operation.
pub(super) fn lock_file(path: &Path, exclusive: bool) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let result = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    match result {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(Error::Busy),
        Err(fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Replace metadata on the same filesystem before changing Git's pointer.
pub(super) fn replace(
    dir: &Path,
    manifest: &Manifest,
    rng: &mut dyn rand::Rng,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|_| Error::Manifest)?;
    let staged = dir.join(format!(
        "store.json.{}.pending",
        StoreId::generate(rng).text()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(staged, dir.join("store.json"))?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}
