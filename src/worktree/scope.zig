//! Workspace Scope discovery and helpers.
//!
//! Per `docs/implementation.md` "Worktrees" section, this module owns
//! Workspace Scope storage, discovery (configured + inferred), branch-name
//! inference, slug derivation, and the git config helpers used by
//! `tk worktree set`, `clear`, and `start`.

const std = @import("std");

/// Sanitize a Ticket/Epic title into a slug for a git ref or filesystem path
/// component.
///
/// Replaces every maximal run of characters outside `[a-z0-9]` with a single
/// `-`, trims leading and trailing `-`, and truncates the result to `max_len`
/// characters at the last `-` boundary that fits. Returns the empty slice
/// when the input contains no `[a-z0-9]` characters after lowercasing.
///
/// Caller owns the returned slice and frees it through `gpa`.
pub fn sanitize(gpa: std.mem.Allocator, title: []const u8, max_len: usize) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(gpa);

    var prev_dash = false;
    for (title) |c| {
        const lower = std.ascii.toLower(c);
        if (isSlugByte(lower)) {
            try out.append(gpa, lower);
            prev_dash = false;
        } else if (!prev_dash and out.items.len > 0) {
            try out.append(gpa, '-');
            prev_dash = true;
        }
    }
    if (out.items.len > 0 and out.items[out.items.len - 1] == '-') {
        _ = out.pop();
    }

    if (out.items.len > max_len) {
        // Truncate at the last `-` boundary at or before `max_len`. If none,
        // hard truncate to `max_len`. Then trim any trailing `-` so the
        // result never ends in a dash.
        var cut: usize = max_len;
        while (cut > 0 and out.items[cut] != '-') : (cut -= 1) {}
        if (cut == 0) cut = max_len;
        out.shrinkRetainingCapacity(cut);
        if (out.items.len > 0 and out.items[out.items.len - 1] == '-') {
            _ = out.pop();
        }
    }
    return try out.toOwnedSlice(gpa);
}

fn isSlugByte(c: u8) bool {
    return (c >= 'a' and c <= 'z') or (c >= '0' and c <= '9');
}

test "sanitize: lowercase ASCII alphanumeric and hyphens pass through unchanged" {
    const gpa = std.testing.allocator;
    const result = try sanitize(gpa, "fix-login-bug", 40);
    defer gpa.free(result);
    try std.testing.expectEqualStrings("fix-login-bug", result);
}

test "sanitize: uppercase letters lowercase and spaces become single hyphen" {
    const gpa = std.testing.allocator;
    const result = try sanitize(gpa, "Fix Login", 40);
    defer gpa.free(result);
    try std.testing.expectEqualStrings("fix-login", result);
}

test "sanitize: truncates at the last hyphen boundary that fits within max_len" {
    const gpa = std.testing.allocator;
    const result = try sanitize(gpa, "fix login bug", 8);
    defer gpa.free(result);
    try std.testing.expectEqualStrings("fix", result);
}

test "sanitize: hard truncates a single long word when no hyphen boundary fits" {
    const gpa = std.testing.allocator;
    const result = try sanitize(gpa, "antidisestablishmentarianism", 8);
    defer gpa.free(result);
    try std.testing.expectEqualStrings("antidise", result);
}
