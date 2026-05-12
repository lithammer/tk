//! Shared test helper that materializes a fake Git repository on disk and
//! synthesizes the `git rev-parse` stdout that a FakeRunner replays.
//!
//! Repository Store commands resolve their database path from Git's common
//! directory, so tests that exercise the open/discovery flow need (a) a real
//! `toplevel` directory, (b) a `.git` subdirectory to serve as `git_common_dir`,
//! and (c) the absolute paths threaded through a faked `git rev-parse` result.

const std = @import("std");

/// On-disk scaffolding for a fake Repository Store. Caller drives `init` and
/// `deinit`; the embedded `tmp` field is cleaned up by `deinit`.
pub const TmpStore = struct {
    tmp: std.testing.TmpDir,
    common_dir_path: []u8,
    toplevel_path: []u8,
    db_path: [:0]u8,

    /// Create a temporary repository rooted at `<tmp>/basename`. `basename`
    /// determines the seeded Display Prefix because `tk init` derives the
    /// prefix from `std.fs.path.basename(toplevel)` — choosing a known
    /// basename lets tests pin the expected Display ID.
    pub fn init(gpa: std.mem.Allocator, basename: []const u8) !TmpStore {
        var tmp = std.testing.tmpDir(.{});
        errdefer tmp.cleanup();
        const tmp_root = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
        defer gpa.free(tmp_root);

        const toplevel = try std.fs.path.join(gpa, &.{ tmp_root, basename });
        errdefer gpa.free(toplevel);
        try std.Io.Dir.cwd().createDirPath(std.testing.io, toplevel);

        const common_dir = try std.fs.path.join(gpa, &.{ toplevel, ".git" });
        errdefer gpa.free(common_dir);
        try std.Io.Dir.cwd().createDirPath(std.testing.io, common_dir);

        const db_path = try std.fs.path.joinZ(gpa, &.{ common_dir, "tk", "ticket.db" });

        return .{
            .tmp = tmp,
            .common_dir_path = common_dir,
            .toplevel_path = toplevel,
            .db_path = db_path,
        };
    }

    pub fn deinit(self: *TmpStore, gpa: std.mem.Allocator) void {
        gpa.free(self.common_dir_path);
        gpa.free(self.toplevel_path);
        gpa.free(self.db_path);
        self.tmp.cleanup();
    }

    /// Build the stdout payload that a real `git rev-parse --git-common-dir
    /// --show-toplevel` would print for this temporary repo. Tests feed this
    /// to `FakeRunner.expect` so the open path sees the same shape Git emits.
    pub fn gitRevParseStdout(self: TmpStore, gpa: std.mem.Allocator) ![]u8 {
        return std.fmt.allocPrint(gpa, "{s}\n{s}\n", .{ self.common_dir_path, self.toplevel_path });
    }
};
