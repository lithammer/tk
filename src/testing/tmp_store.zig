//! Shared test helper that materializes a fake Git repository on disk and
//! synthesizes the `git rev-parse` stdout that a FakeRunner replays.
//!
//! Repository Store commands resolve their database path from Git's common
//! directory, so tests that exercise the open/discovery flow need (a) a real
//! `toplevel` directory, (b) a `.git` subdirectory to serve as `git_common_dir`,
//! and (c) the absolute paths threaded through a faked `git rev-parse` result.

const std = @import("std");
const zqlite = @import("zqlite");

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

    /// Raw Repository Store item fixture used by read-command tests.
    ///
    /// This deliberately bypasses production write APIs so slices can seed
    /// Epics, backend-origin items, Dependencies, and External Blockers before
    /// their user-facing mutation commands exist.
    pub const FixtureItem = struct {
        id: []const u8,
        display: []const u8,
        item_class: []const u8 = "ticket",
        ticket_kind: ?[]const u8 = "task",
        priority: ?[]const u8 = "P2",
        title: []const u8,
        body: []const u8 = "",
        status: []const u8 = "open",
        origin: []const u8 = "local",
        backend_kind: ?[]const u8 = null,
        backend_key: ?[]const u8 = null,
        container_id: ?[]const u8 = null,
        container_class: ?[]const u8 = null,
        created_seq: i64,
        created_at: []const u8 = "2026-05-09T00:00:00.000Z",
        updated_at: []const u8 = "2026-05-09T00:00:00.000Z",
    };

    /// Insert one current-state item plus its Display ID resolver row.
    pub fn insertFixtureItem(conn: zqlite.Conn, args: FixtureItem) !void {
        const container_class = if (args.container_id == null) null else args.container_class orelse "epic";
        try conn.transaction();
        errdefer conn.rollback();
        try conn.exec(
            \\insert into items(
            \\  id, display_value, item_class, ticket_kind, priority, title, body,
            \\  container_id, container_class, origin, backend_kind, backend_key,
            \\  status, created_seq, created_at, updated_at
            \\)
            \\values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        , .{
            args.id,
            args.display,
            args.item_class,
            args.ticket_kind,
            args.priority,
            args.title,
            args.body,
            args.container_id,
            container_class,
            args.origin,
            args.backend_kind,
            args.backend_key,
            args.status,
            args.created_seq,
            args.created_at,
            args.updated_at,
        });
        try conn.exec(
            "insert into item_ids(value, source, item_id, created_at) values (?1, 'display', ?2, ?3)",
            .{ args.display, args.id, args.created_at },
        );
        try conn.commit();
    }

    /// Insert a Dependency edge from a Blocking Item to a Blocked Item.
    pub fn insertDependency(conn: zqlite.Conn, blocking_id: []const u8, blocked_id: []const u8) !void {
        try conn.exec(
            \\insert into dependencies(blocking_id, blocked_id, created_at)
            \\values (?1, ?2, '2026-05-09T00:00:00.000Z')
        , .{ blocking_id, blocked_id });
    }

    /// Insert an External Blocker fixture. `resolved_at = null` means unresolved.
    pub fn insertExternalBlocker(conn: zqlite.Conn, id: []const u8, item_id: []const u8, resolved_at: ?[]const u8) !void {
        try conn.exec(
            \\insert into external_blockers(id, item_id, reason, created_at, resolved_at)
            \\values (?1, ?2, 'fixture blocker', '2026-05-09T00:00:00.000Z', ?3)
        , .{ id, item_id, resolved_at });
    }
};
