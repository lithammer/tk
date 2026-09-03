//! Host-OS predicates, and the small host-specific effects they gate.
//!
//! Centralising them gives LSP "show references" and grep a single canonical
//! symbol to follow instead of scattered `cfg(target_os = "...")` checks
//! across the command and store modules.

/// `true` when compiling for Windows. Const so it folds away on POSIX builds.
pub const IS_WINDOWS: bool = cfg!(target_os = "windows");

/// Tighten a directory to owner-only, where the host has Unix-style
/// permissions.
///
/// Callers must apply this only to a directory they just created: a
/// pre-existing one keeps the permissions the user gave it (ARCHITECTURE.md,
/// Repository Store Contracts). `chmod` follows symlinks, so calling it on a
/// directory tk did not create can retarget somewhere the user meant to keep.
pub fn set_dir_mode_0700(path: &std::path::Path) -> std::io::Result<()> {
    if IS_WINDOWS {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let _ = path; // keep `path` used on non-unix non-windows targets.
    Ok(())
}
