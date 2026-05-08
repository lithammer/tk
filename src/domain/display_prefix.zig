//! Derives a Local Display ID prefix from a repository basename.
//!
//! Algorithm (per `docs/implementation.md`, Storage section):
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

pub const max_prefix_len: usize = 12;

/// Returns a newly-allocated, lowercase prefix derived from `basename`.
/// Caller owns the returned slice.
pub fn derive(gpa: std.mem.Allocator, basename: []const u8) ![]u8 {
    var segments: std.ArrayList([]const u8) = .empty;
    defer segments.deinit(gpa);

    var lowered: std.ArrayList(u8) = .empty;
    defer lowered.deinit(gpa);

    for (basename) |b| {
        try lowered.append(gpa, std.ascii.toLower(b));
    }

    var start: usize = 0;
    var i: usize = 0;
    while (i < lowered.items.len) : (i += 1) {
        if (isSeparator(lowered.items[i])) {
            if (i > start) {
                const seg = sanitize(gpa, lowered.items[start..i]) catch |err| return err;
                if (seg.len > 0) {
                    try segments.append(gpa, seg);
                } else {
                    gpa.free(seg);
                }
            }
            start = i + 1;
        }
    }
    if (start < lowered.items.len) {
        const seg = try sanitize(gpa, lowered.items[start..]);
        if (seg.len > 0) {
            try segments.append(gpa, seg);
        } else {
            gpa.free(seg);
        }
    }

    defer for (segments.items) |seg| gpa.free(seg);

    const result = blk: {
        const joined_all = try joinWithDash(gpa, segments.items);
        if (joined_all.len > 0 and joined_all.len <= max_prefix_len) break :blk joined_all;
        defer gpa.free(joined_all);

        if (segments.items.len >= 2) {
            const joined_two = try joinWithDash(gpa, segments.items[0..2]);
            if (joined_two.len <= max_prefix_len) break :blk joined_two;
            gpa.free(joined_two);
        }

        // Truncate the full sanitized basename (no separators preserved).
        var concat: std.ArrayList(u8) = .empty;
        defer concat.deinit(gpa);
        for (segments.items) |seg| {
            try concat.appendSlice(gpa, seg);
        }
        const upper = @min(concat.items.len, max_prefix_len);
        break :blk try gpa.dupe(u8, concat.items[0..upper]);
    };

    if (result.len == 0 or std.ascii.isDigit(result[0])) {
        defer gpa.free(result);
        return try std.fmt.allocPrint(gpa, "tk-{s}", .{result});
    }
    return result;
}

fn isSeparator(b: u8) bool {
    return switch (b) {
        '-', '_', '.', '/', ':', '#', ' ', '\t' => true,
        else => false,
    };
}

fn sanitize(gpa: std.mem.Allocator, segment: []const u8) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(gpa);
    for (segment) |b| {
        if (std.ascii.isAlphanumeric(b)) {
            try out.append(gpa, b);
        }
    }
    return out.toOwnedSlice(gpa);
}

fn joinWithDash(gpa: std.mem.Allocator, segments: []const []const u8) ![]u8 {
    if (segments.len == 0) return try gpa.alloc(u8, 0);
    var total: usize = 0;
    for (segments) |seg| total += seg.len;
    total += segments.len - 1;

    var buf = try gpa.alloc(u8, total);
    var pos: usize = 0;
    for (segments, 0..) |seg, i| {
        if (i > 0) {
            buf[pos] = '-';
            pos += 1;
        }
        @memcpy(buf[pos .. pos + seg.len], seg);
        pos += seg.len;
    }
    return buf;
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
    // Joined: "src-cafe-extras-and-more" 24 chars > 12.
    // First two: "src-cafe" 8 chars <= 12.
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
    // '.' is a separator → segments ["ab", "cdef"] → joined "ab-cdef".
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
    // Joined "42-things" is 9 chars (fits) but starts with digit.
    try std.testing.expectEqualStrings("tk-42-things", out);
}

test "derive: vowels preserved" {
    const out = try derive(std.testing.allocator, "iou");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("iou", out);
}

test "derive: long first two segments truncate full basename" {
    // First two joined: "alphabetone-betatwo" = 19 chars > 12.
    // Full sanitized concat truncated to 12: "alphabetoneb".
    const out = try derive(std.testing.allocator, "alphabetone-betatwo-gamma");
    defer std.testing.allocator.free(out);
    try std.testing.expectEqualStrings("alphabetoneb", out);
}
