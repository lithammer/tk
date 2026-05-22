const builtin = @import("builtin");

/// Skip the calling test on Windows. Use at the top of test bodies that
/// exercise POSIX-only behavior (permission bits, troff backslash escapes,
/// path semantics with no Windows analogue) and document the reason at the
/// call site.
pub fn skipOnWindows() error{SkipZigTest}!void {
    if (builtin.os.tag == .windows) return error.SkipZigTest;
}
