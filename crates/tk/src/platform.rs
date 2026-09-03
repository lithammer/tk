//! Host-OS predicates used to gate platform-specific behaviour.
//!
//! Centralising the predicate gives LSP "show references" and grep a single
//! canonical symbol to follow instead of scattered `cfg(target_os = "...")`
//! checks across command modules.

/// `true` when compiling for Windows. Const so it folds away on POSIX builds.
pub const IS_WINDOWS: bool = cfg!(target_os = "windows");

/// Tighten a directory tk created to owner-only, where the host has
/// Unix-style permissions.
///
/// Only ever applied to a directory tk created itself: `tk init` leaves a
/// pre-existing `tk/` with broader permissions alone (see ARCHITECTURE.md),
/// and the Store Backup directory is always tk's own (ADR-0048).
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
