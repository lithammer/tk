//! Comptime guards for `@embedFile` call sites.
//!
//! Lives as a leaf module so `store/`, `testing/`, and `commands/` can apply
//! the guards without importing `cli.zig` (which would invert the module
//! dependency direction described in `ARCHITECTURE.md`).

const std = @import("std");

/// Compile-time guard that an `@embedFile` payload contains no CR bytes.
/// Apply at every `@embedFile` call site so a CRLF regression (escaped
/// `.gitattributes`, contributor with stale local autocrlf working tree)
/// fails the build instead of shipping a broken binary.
///
/// `@setEvalBranchQuota` is bumped per byte because the migration SQL files
/// (~28 KB) and the manpage (~6 KB) exceed Zig's default 1000-branch quota
/// when scanned at comptime.
pub fn assertNoCR(comptime bytes: []const u8) void {
    @setEvalBranchQuota(bytes.len * 8 + 1000);
    for (bytes) |b| if (b == '\r') @compileError("embedded file contains CR; check .gitattributes for eol=lf");
}
