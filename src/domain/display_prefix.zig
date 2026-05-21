//! Derives a Local Display ID prefix from a repository basename.
//!
//! Algorithm (per `ARCHITECTURE.md` ("Repository Store Contracts")):
//! - Lowercase.
//! - Treat underscores as separators.
//! - Split on separators and punctuation (`-`, `_`, `.`, `/`, `:`, `#`,
//!   whitespace).
//! - Drop empty segments.
//! - If the joined form (segments joined with `-`) is at most 12 chars, use it.
//! - Else if the first two segments joined with `-` fit in 12 chars, use that.
//! - Else truncate the sanitized basename to 12 chars.
//! - If the result is empty or starts with a digit, prefix with `tk-`.
//!
//! Vowels are not stripped. Output is always lowercase ASCII.

const std = @import("std");
const Allocator = std.mem.Allocator;

/// Maximum length of the stored local Display ID prefix.
pub const max_prefix_len: usize = 12;

const separators = "-_./:# \t";

/// Returns a newly-allocated, lowercase prefix derived from `basename`.
/// Caller owns the returned slice.
pub fn derive(gpa: Allocator, basename: []const u8) ![]u8 {
    const lowered = try std.ascii.allocLowerString(gpa, basename);
    defer gpa.free(lowered);

    var segments: std.ArrayList([]u8) = .empty;
    defer {
        for (segments.items) |seg| gpa.free(seg);
        segments.deinit(gpa);
    }

    var it = std.mem.tokenizeAny(u8, lowered, separators);
    while (it.next()) |raw| {
        const seg = try filterAlnum(gpa, raw);
        if (seg.len == 0) {
            gpa.free(seg);
            continue;
        }
        try segments.append(gpa, seg);
    }

    const result = try chooseShape(gpa, segments.items);
    if (result.len == 0 or std.ascii.isDigit(result[0])) {
        defer gpa.free(result);
        return std.fmt.allocPrint(gpa, "tk-{s}", .{result});
    }
    return result;
}

fn chooseShape(gpa: Allocator, segments: []const []u8) ![]u8 {
    const joined_all = try std.mem.join(gpa, "-", segments);
    if (joined_all.len > 0 and joined_all.len <= max_prefix_len) return joined_all;
    defer gpa.free(joined_all);

    if (segments.len >= 2) {
        const joined_two = try std.mem.join(gpa, "-", segments[0..2]);
        if (joined_two.len <= max_prefix_len) return joined_two;
        gpa.free(joined_two);
    }

    const concat = try std.mem.concat(gpa, u8, segments);
    defer gpa.free(concat);
    const upper = @min(concat.len, max_prefix_len);
    return gpa.dupe(u8, concat[0..upper]);
}

fn filterAlnum(gpa: Allocator, segment: []const u8) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(gpa);
    for (segment) |b| {
        if (std.ascii.isAlphanumeric(b)) try out.append(gpa, b);
    }
    return out.toOwnedSlice(gpa);
}

test "derive: short single word" {
    const out = try derive(std.testing.allocator, "ticket");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("ticket", out);
}

test "derive: lowercases" {
    const out = try derive(std.testing.allocator, "Ticket");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("ticket", out);
}

test "derive: multi-word fits joined" {
    const out = try derive(std.testing.allocator, "my-cool-app");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("my-cool-app", out);
}

test "derive: long basename falls back to first two segments" {
    const out = try derive(std.testing.allocator, "src-cafe-extras-and-more");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("src-cafe", out);
}

test "derive: very long single word truncates to 12" {
    const out = try derive(std.testing.allocator, "abcdefghijklmnopqrstuvwxyz");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("abcdefghijkl", out);
}

test "derive: underscores treated as separators" {
    const out = try derive(std.testing.allocator, "my_cool_app");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("my-cool-app", out);
}

test "derive: punctuation stripped from segments" {
    const out = try derive(std.testing.allocator, "ab.cd!ef");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("ab-cdef", out);
}

test "derive: empty becomes tk-" {
    const out = try derive(std.testing.allocator, "");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("tk-", out);
}

test "derive: all-punctuation becomes tk-" {
    const out = try derive(std.testing.allocator, "---");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("tk-", out);
}

test "derive: digit-leading prefixed with tk-" {
    const out = try derive(std.testing.allocator, "42-things");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("tk-42-things", out);
}

test "derive: vowels preserved" {
    const out = try derive(std.testing.allocator, "iou");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("iou", out);
}

test "derive: long first two segments truncate full basename" {
    const out = try derive(std.testing.allocator, "alphabetone-betatwo-gamma");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("alphabetoneb", out);
}
