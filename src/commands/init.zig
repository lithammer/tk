//! `tk init` — create the Repository Store at `<git-common-dir>/tk/ticket.db`.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const proc = @import("../proc/runner.zig");
const sqlite = @import("../store/sqlite.zig");
const migrations = @import("../store/migrations.zig");
const display_prefix = @import("../domain/display_prefix.zig");
const clock_mod = @import("../clock.zig");

pub const meta: cli.CommandMeta = .{
    .name = "init",
    .description = "Initialize the Repository Store in the current Git repository",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\
);

pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    var diag: clap.Diagnostic = .{};
    var res = clap.parseEx(clap.Help, &params, clap.parsers.default, args_iter, .{
        .diagnostic = &diag,
        .allocator = deps.gpa,
    }) catch |err| {
        diag.report(deps.stderr, err) catch {};
        return 2;
    };
    defer res.deinit();

    if (res.args.help != 0) {
        writeHelp(deps) catch {};
        return 0;
    }

    return execute(deps);
}

fn execute(deps: cli.Deps) !u8 {
    const paths = (try discoverPaths(deps)) orelse return 1;
    defer deps.gpa.free(paths.git_common_dir);
    defer deps.gpa.free(paths.toplevel);

    const tk_dir_path = try std.fs.path.join(deps.gpa, &.{ paths.git_common_dir, "tk" });
    defer deps.gpa.free(tk_dir_path);
    std.Io.Dir.cwd().createDirPath(deps.io, tk_dir_path) catch |err| {
        deps.stderr.print("tk init: failed to create {s}: {s}\n", .{ tk_dir_path, @errorName(err) }) catch {};
        return 1;
    };
    // Best-effort tighten permissions on POSIX. If chmod fails or the dir
    // already exists with broader permissions, leave it as-is.
    chmod0700(tk_dir_path) catch {};

    const db_path = try std.fs.path.joinZ(deps.gpa, &.{ tk_dir_path, "ticket.db" });
    defer deps.gpa.free(db_path);

    const exists = blk: {
        std.Io.Dir.cwd().access(deps.io, db_path, .{}) catch break :blk false;
        break :blk true;
    };

    if (exists) {
        if (!isTicketStore(db_path)) {
            deps.stderr.print(
                "tk init: {s} exists but is not a Ticket Repository Store\n",
                .{db_path},
            ) catch {};
            return 1;
        }
    }

    var db = sqlite.Db.open(db_path, .{}) catch |err| {
        deps.stderr.print("tk init: failed to open {s}: {s}\n", .{ db_path, @errorName(err) }) catch {};
        return 1;
    };
    defer db.close();

    db.exec("pragma journal_mode = wal") catch {};
    db.exec("pragma foreign_keys = on") catch |err| {
        deps.stderr.print("tk init: pragma foreign_keys failed: {s}\n", .{@errorName(err)}) catch {};
        return 1;
    };
    db.exec("pragma busy_timeout = 5000") catch {};

    var iso_buf: [24]u8 = undefined;
    const now_iso = deps.clock.nowIso(&iso_buf);

    migrations.applyAll(&db, now_iso) catch |err| switch (err) {
        error.StoreFromFutureVersion => {
            deps.stderr.print(
                "tk init: {s} was created by a newer Ticket version\n",
                .{db_path},
            ) catch {};
            return 1;
        },
        else => {
            deps.stderr.print(
                "tk init: migration failed: {s}: {s}\n",
                .{ @errorName(err), db.errorMessage() },
            ) catch {};
            return 1;
        },
    };

    seedDisplayPrefix(deps, &db, paths.toplevel) catch |err| {
        deps.stderr.print(
            "tk init: failed to seed display_prefix: {s}: {s}\n",
            .{ @errorName(err), db.errorMessage() },
        ) catch {};
        return 1;
    };

    if (exists) {
        deps.stdout.print("Repository Store already initialized at {s}\n", .{db_path}) catch {};
    } else {
        deps.stdout.print("Initialized Repository Store at {s}\n", .{db_path}) catch {};
    }
    return 0;
}

const DiscoveredPaths = struct {
    git_common_dir: []u8,
    toplevel: []u8,
};

fn discoverPaths(deps: cli.Deps) !?DiscoveredPaths {
    var run_result = deps.runner.run(deps.gpa, .{
        .argv = &.{ "git", "rev-parse", "--path-format=absolute", "--git-common-dir", "--show-toplevel" },
        .cwd = deps.cwd,
    }) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.SpawnFailed => {
            deps.stderr.writeAll("tk init: failed to invoke git\n") catch {};
            return null;
        },
    };
    defer run_result.deinit(deps.gpa);

    if (run_result.exit_code != 0) {
        deps.stderr.writeAll("tk init: not in a git repository\n") catch {};
        return null;
    }

    var lines = std.mem.tokenizeScalar(u8, run_result.stdout, '\n');
    const common = lines.next() orelse {
        deps.stderr.writeAll("tk init: git did not report a common directory\n") catch {};
        return null;
    };
    const toplevel = lines.next() orelse {
        deps.stderr.writeAll("tk init: bare repositories are not supported\n") catch {};
        return null;
    };

    const common_owned = try deps.gpa.dupe(u8, std.mem.trim(u8, common, " \t\r\n"));
    errdefer deps.gpa.free(common_owned);
    const toplevel_owned = try deps.gpa.dupe(u8, std.mem.trim(u8, toplevel, " \t\r\n"));
    errdefer deps.gpa.free(toplevel_owned);

    return .{ .git_common_dir = common_owned, .toplevel = toplevel_owned };
}

fn isTicketStore(db_path: [:0]const u8) bool {
    var db = sqlite.Db.open(db_path, .{ .create = false }) catch return false;
    defer db.close();
    const id = db.queryOneInt("pragma application_id") catch return false;
    if (id) |v| return v == migrations.application_id;
    return false;
}

fn seedDisplayPrefix(deps: cli.Deps, db: *sqlite.Db, toplevel: []const u8) !void {
    const existing = try db.queryOneText(
        deps.gpa,
        "select value from store_config where key = 'display_prefix'",
    );
    if (existing) |val| {
        deps.gpa.free(val);
        return;
    }
    const basename = std.fs.path.basename(toplevel);
    const prefix = try display_prefix.derive(deps.gpa, basename);
    defer deps.gpa.free(prefix);

    var buf: [256]u8 = undefined;
    const sql = try std.fmt.bufPrintZ(
        &buf,
        "insert into store_config(key, value) values ('display_prefix', '{s}')",
        .{prefix},
    );
    try db.exec(sql);
}

fn chmod0700(path: []const u8) !void {
    const builtin = @import("builtin");
    if (builtin.os.tag == .windows) return;
    const path_z = try std.heap.page_allocator.dupeZ(u8, path);
    defer std.heap.page_allocator.free(path_z);
    _ = std.posix.system.chmod(path_z, 0o700);
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk init - initialize the Repository Store
        \\
        \\Creates <git-common-dir>/tk/ticket.db with the v1 schema. Must be run
        \\from within a Git repository. Idempotent: re-running on a current
        \\store is a no-op.
        \\
        \\Usage:
        \\  tk init [options]
        \\
        \\Options:
        \\
    );
    try clap.help(deps.stdout, clap.Help, &params, .{
        .description_on_new_line = false,
        .description_indent = 2,
        .indent = 2,
        .spacing_between_parameters = 0,
    });
}

const Harness = @import("../testing/test_cli.zig").Harness;
const FakeRunner = @import("../proc/fake.zig").FakeRunner;

test "init: returns exit 1 with diagnostic when not in a git repo" {
    var h = Harness.init(std.testing.allocator, &.{});
    defer h.deinit();

    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{
        .exit_code = 128,
        .stderr = "fatal: not a git repository (or any of the parent directories): .git\n",
    });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "git repository") != null);
}

test "init: --help prints to stdout, exits 0" {
    var h = Harness.init(std.testing.allocator, &.{"--help"});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "tk init") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Repository Store") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

// ---- End-to-end tests using a temp dir as the simulated Git common dir ----

const TmpStore = struct {
    tmp: std.testing.TmpDir,
    common_dir_path: []u8,
    toplevel_path: []u8,
    db_path: [:0]u8,

    fn init(gpa: std.mem.Allocator) !TmpStore {
        var tmp = std.testing.tmpDir(.{});
        errdefer tmp.cleanup();
        const root = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
        errdefer gpa.free(root);

        // Pretend the temp dir is the git toplevel; common_dir is a `.git`
        // child mkdir'd under it. tk init will create .git/tk/ticket.db.
        const toplevel = try gpa.dupe(u8, root);
        errdefer gpa.free(toplevel);
        const common_dir = try std.fs.path.join(gpa, &.{ root, ".git" });
        errdefer gpa.free(common_dir);
        try std.Io.Dir.cwd().createDirPath(std.testing.io, common_dir);

        const db_path = try std.fs.path.joinZ(gpa, &.{ common_dir, "tk", "ticket.db" });

        gpa.free(root);
        return .{
            .tmp = tmp,
            .common_dir_path = common_dir,
            .toplevel_path = toplevel,
            .db_path = db_path,
        };
    }

    fn deinit(self: *TmpStore, gpa: std.mem.Allocator) void {
        gpa.free(self.common_dir_path);
        gpa.free(self.toplevel_path);
        gpa.free(self.db_path);
        self.tmp.cleanup();
    }

    fn gitRevParseStdout(self: TmpStore, gpa: std.mem.Allocator) ![]u8 {
        return std.fmt.allocPrint(gpa, "{s}\n{s}\n", .{ self.common_dir_path, self.toplevel_path });
    }
};

test "init: success creates store, applies migration, seeds prefix" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa);
    defer store.deinit(gpa);

    var h = Harness.init(gpa, &.{});
    defer h.deinit();

    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{
        .exit_code = 0,
        .stdout = stdout_payload,
    });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Initialized Repository Store at") != null);
    try std.testing.expectEqualStrings("", h.stderr());

    // Open the DB and assert state.
    var db = try sqlite.Db.open(store.db_path, .{ .create = false });
    defer db.close();

    const app_id = (try db.queryOneInt("pragma application_id")).?;
    try std.testing.expectEqual(@as(i64, migrations.application_id), app_id);

    const user_v = (try db.queryOneInt("pragma user_version")).?;
    try std.testing.expectEqual(@as(i64, 1), user_v);

    const journal = try db.queryOneText(gpa, "pragma journal_mode");
    defer if (journal) |j| gpa.free(j);
    try std.testing.expectEqualStrings("wal", journal.?);

    const prefix = try db.queryOneText(gpa, "select value from store_config where key='display_prefix'");
    defer if (prefix) |p| gpa.free(p);
    try std.testing.expect(prefix != null);
    try std.testing.expect(prefix.?.len > 0);
}

test "init: idempotent on second run" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa);
    defer store.deinit(gpa);

    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);

    {
        var h = Harness.init(gpa, &.{});
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });
        const code = try run(h.deps(), &h.iter);
        try std.testing.expectEqual(@as(u8, 0), code);
    }

    {
        var h = Harness.init(gpa, &.{});
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });
        const code = try run(h.deps(), &h.iter);
        try std.testing.expectEqual(@as(u8, 0), code);
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "already initialized") != null);
    }

    // schema_migrations should still have exactly one row.
    var db = try sqlite.Db.open(store.db_path, .{ .create = false });
    defer db.close();
    const count = (try db.queryOneInt("select count(*) from schema_migrations")).?;
    try std.testing.expectEqual(@as(i64, 1), count);
}

test "init: refuses to overwrite a non-Ticket SQLite file" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa);
    defer store.deinit(gpa);

    // Pre-create the tk/ dir and put a plain SQLite file at ticket.db.
    const tk_dir = try std.fs.path.join(gpa, &.{ store.common_dir_path, "tk" });
    defer gpa.free(tk_dir);
    try std.Io.Dir.cwd().createDirPath(std.testing.io, tk_dir);

    {
        var foreign = try sqlite.Db.open(store.db_path, .{});
        defer foreign.close();
        try foreign.exec("create table other_app(x integer)");
    }

    var h = Harness.init(gpa, &.{});
    defer h.deinit();
    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "not a Ticket Repository Store") != null);

    // The pre-existing table must still be there (we did not replace the file).
    var db = try sqlite.Db.open(store.db_path, .{ .create = false });
    defer db.close();
    const exists = (try db.queryOneInt("select count(*) from sqlite_master where name='other_app'")).?;
    try std.testing.expectEqual(@as(i64, 1), exists);
}

test "init: rejects a store created by a future Ticket version" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa);
    defer store.deinit(gpa);

    // Initialize once normally.
    {
        var h = Harness.init(gpa, &.{});
        defer h.deinit();
        const stdout_payload = try store.gitRevParseStdout(gpa);
        defer gpa.free(stdout_payload);
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });
        const code = try run(h.deps(), &h.iter);
        try std.testing.expectEqual(@as(u8, 0), code);
    }

    // Forge a future-version migration row.
    {
        var db = try sqlite.Db.open(store.db_path, .{ .create = false });
        defer db.close();
        try db.exec("insert into schema_migrations(version, applied_at) values (999, '2099-01-01T00:00:00.000Z')");
    }

    var h = Harness.init(gpa, &.{});
    defer h.deinit();
    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "newer Ticket version") != null);
}
