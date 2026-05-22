//! Comptime-known facts about the host OS.
//!
//! Prefer `platform.is_windows` over inline `builtin.os.tag == .windows`
//! checks throughout the codebase. Centralising the predicate gives LSP
//! "show references" and `grep` a single, canonical symbol to follow and
//! keeps the condition consistent if the supported OS set ever changes.

const builtin = @import("builtin");

/// `true` when compiling for Windows. Comptime-known, so call sites like
/// `if (platform.is_windows)` are dead-code-eliminated on POSIX.
pub const is_windows: bool = builtin.os.tag == .windows;

/// Skip the calling test on Windows. Use at the top of test bodies that
/// exercise POSIX-only behavior (permission bits, path semantics with no
/// Windows analogue) and document the reason at the call site.
///
/// The OS check is comptime-known, so on non-Windows builds this inlines
/// to nothing.
pub inline fn skipOnWindows() error{SkipZigTest}!void {
    if (is_windows) return error.SkipZigTest;
}

/// Skip the calling test on non-Windows targets. Counterpart to
/// `skipOnWindows`; use for tests that pin Windows-only behaviour (e.g.
/// the `tk manpage --install` no-op contract) so they report as
/// `skipped` rather than silently `passed` on POSIX hosts.
pub inline fn skipOnPosix() error{SkipZigTest}!void {
    if (!is_windows) return error.SkipZigTest;
}
