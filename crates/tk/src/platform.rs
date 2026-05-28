//! Host-OS predicates used to gate platform-specific behaviour.
//!
//! Mirrors `src/platform.zig` in the Zig oracle: a single canonical predicate
//! that LSP "show references" and grep can follow, instead of scattered
//! `cfg(target_os = "windows")` checks.

/// `true` when compiling for Windows. Const so it folds away on POSIX builds.
pub const IS_WINDOWS: bool = cfg!(target_os = "windows");
