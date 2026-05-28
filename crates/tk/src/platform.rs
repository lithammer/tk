//! Host-OS predicates used to gate platform-specific behaviour.
//!
//! Centralising the predicate gives LSP "show references" and grep a single
//! canonical symbol to follow instead of scattered `cfg(target_os = "...")`
//! checks across command modules.

/// `true` when compiling for Windows. Const so it folds away on POSIX builds.
pub const IS_WINDOWS: bool = cfg!(target_os = "windows");
