const builtin = @import("builtin");

/// Skip the calling test on Windows. Use at the top of test bodies that
/// exercise POSIX-only behavior (permission bits, path semantics with no
/// Windows analogue) and document the reason at the call site.
///
/// The OS check is comptime-known, so on non-Windows builds this inlines
/// to nothing.
pub inline fn skipOnWindows() error{SkipZigTest}!void {
    if (comptime builtin.os.tag == .windows) return error.SkipZigTest;
}
