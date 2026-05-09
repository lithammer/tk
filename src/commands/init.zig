//! `tk init` — create the Repository Store at `<git-common-dir>/tk/ticket.db`.

const std = @import("std");
const builtin = @import("builtin");
const clap = @import("clap");
const zqlite = @import("zqlite");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const migrations = @import("../store/migrations.zig");
const display_prefix = @import("../domain/display_prefix.zig");

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
        // TODO(followups): "Prefix tk init clap diagnostics with the command
        // name" — docs/followups.md.
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
    const dir_status = std.Io.Dir.cwd().createDirPathStatus(
        deps.io,
        tk_dir_path,
        @enumFromInt(0o700),
    ) catch |err| {
        deps.stderr.print("tk init: failed to create {s}: {s}\n", .{ tk_dir_path, @errorName(err) }) catch {};
        return 1;
    };
    // Per docs/implementation.md: when the directory already exists with
    // broader permissions, slice 2 uses it as-is and does not chmod it.
    // Only tighten when we're the ones who just created it.
    // TODO(followups): "Surface a stderr warning when tk init can't tighten
    // store permissions" — docs/followups.md.
    if (dir_status == .created) setDirMode0700(deps, tk_dir_path) catch {};

    const db_path = try std.fs.path.joinZ(deps.gpa, &.{ tk_dir_path, "ticket.db" });
    defer deps.gpa.free(db_path);

    const conn = zqlite.open(db_path.ptr, zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode) catch |err| {
        deps.stderr.print("tk init: failed to open {s}: {s}\n", .{ db_path, @errorName(err) }) catch {};
        return 1;
    };
    defer conn.close();

    // Classify *before* enabling WAL or any other pragma that would mutate the
    // file. A foreign rollback-journal SQLite file at this path must stay
    // exactly as we found it when we refuse.
    const kind = classify(conn) catch |err| {
        deps.stderr.print(
            "tk init: failed to inspect {s}: {s}: {s}\n",
            .{ db_path, @errorName(err), std.mem.span(conn.lastError()) },
        ) catch {};
        return 1;
    };
    switch (kind) {
        .foreign => {
            deps.stderr.print(
                "tk init: {s} exists but is " ++ messages.init_refuse_foreign ++ "\n",
                .{db_path},
            ) catch {};
            return 1;
        },
        .fresh, .ours => {},
    }

    try configureForTicketStore(conn);

    var iso_buf: [24]u8 = undefined;
    const now_iso = deps.clock.nowIso(&iso_buf);

    migrations.applyAll(conn, now_iso) catch |err| switch (err) {
        error.StoreFromFutureVersion => {
            deps.stderr.print(
                "tk init: {s} was created by a " ++ messages.init_refuse_future_version ++ "\n",
                .{db_path},
            ) catch {};
            return 1;
        },
        else => {
            deps.stderr.print(
                "tk init: migration failed: {s}: {s}\n",
                .{ @errorName(err), migrations.lastError() },
            ) catch {};
            return 1;
        },
    };

    seedDisplayPrefix(deps, conn, paths.toplevel) catch |err| {
        deps.stderr.print(
            "tk init: failed to seed display_prefix: {s}: {s}\n",
            .{ @errorName(err), std.mem.span(conn.lastError()) },
        ) catch {};
        return 1;
    };

    switch (kind) {
        .ours => deps.stdout.print(messages.init_success_existing ++ "{s}\n", .{db_path}) catch {},
        .fresh => deps.stdout.print(messages.init_success_fresh ++ "{s}\n", .{db_path}) catch {},
        .foreign => unreachable,
    }
    return 0;
}

const StoreKind = enum {
    /// File was just created by sqlite3_open_v2 and contains no schema yet.
    fresh,
    /// Existing Ticket Repository Store (matching application_id).
    ours,
    /// Existing SQLite file written by something else.
    foreign,
};

fn classify(conn: zqlite.Conn) migrations.QueryError!StoreKind {
    const app_id = (try migrations.queryOptionalInt(conn, "pragma application_id")) orelse 0;
    if (app_id == migrations.application_id) return .ours;

    const table_count = (try migrations.queryOptionalInt(conn, "select count(*) from sqlite_master")) orelse 0;
    if (app_id == 0 and table_count == 0) return .fresh;
    return .foreign;
}

fn configureForTicketStore(conn: zqlite.Conn) zqlite.Error!void {
    // journal_mode persists in the file header; foreign_keys and
    // busy_timeout are connection-scoped and have to be set every open.
    try conn.execNoArgs("pragma journal_mode = wal");
    try conn.busyTimeout(5000);
    try conn.execNoArgs("pragma foreign_keys = on");
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
        error.ExecutableNotFound => {
            deps.stderr.writeAll("tk init: " ++ messages.init_git_missing ++ "\n") catch {};
            return null;
        },
        error.SpawnFailed => {
            deps.stderr.writeAll("tk init: " ++ messages.init_git_spawn_failed ++ "\n") catch {};
            return null;
        },
    };
    defer run_result.deinit(deps.gpa);

    if (run_result.exit_code != 0) {
        // git's stderr already explains "not a git repository", "must be run
        // in a work tree" (bare repos), etc. Reuse it rather than fabricate a
        // generic message that loses detail.
        const trimmed = std.mem.trim(u8, run_result.stderr, " \t\r\n");
        if (trimmed.len > 0) {
            deps.stderr.print("tk init: {s}\n", .{trimmed}) catch {};
        } else {
            deps.stderr.writeAll("tk init: " ++ messages.init_outside_git_default ++ "\n") catch {};
        }
        return null;
    }

    var lines = std.mem.tokenizeScalar(u8, run_result.stdout, '\n');
    const common = lines.next() orelse {
        deps.stderr.writeAll("tk init: git did not report a common directory\n") catch {};
        return null;
    };
    const toplevel = lines.next() orelse {
        // git rev-parse with --show-toplevel inside a worktree always emits
        // both lines together; this branch is unreachable in practice.
        deps.stderr.writeAll("tk init: git did not report a working tree\n") catch {};
        return null;
    };

    const common_owned = try deps.gpa.dupe(u8, std.mem.trim(u8, common, " \t\r\n"));
    errdefer deps.gpa.free(common_owned);
    const toplevel_owned = try deps.gpa.dupe(u8, std.mem.trim(u8, toplevel, " \t\r\n"));
    errdefer deps.gpa.free(toplevel_owned);

    return .{ .git_common_dir = common_owned, .toplevel = toplevel_owned };
}

fn seedDisplayPrefix(deps: cli.Deps, conn: zqlite.Conn, toplevel: []const u8) !void {
    if (try conn.row("select 1 from store_config where key = 'display_prefix'", .{})) |existing| {
        existing.deinit();
        return;
    }
    const basename = std.fs.path.basename(toplevel);
    const prefix = try display_prefix.derive(deps.gpa, basename);
    defer deps.gpa.free(prefix);

    try conn.exec(
        "insert into store_config(key, value) values ('display_prefix', ?1)",
        .{prefix},
    );
}

fn setDirMode0700(deps: cli.Deps, path: []const u8) !void {
    if (builtin.os.tag == .windows) return;
    try std.Io.Dir.cwd().setFilePermissions(deps.io, path, @enumFromInt(0o700), .{});
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

test "init: empty-stderr git failure falls back to default diagnostic" {
    var h = Harness.init(std.testing.allocator, &.{});
    defer h.deinit();

    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 128, .stderr = "" });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.init_outside_git_default) != null);
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

const TmpStore = struct {
    tmp: std.testing.TmpDir,
    common_dir_path: []u8,
    toplevel_path: []u8,
    db_path: [:0]u8,

    /// `basename` is the directory name simulated as the repository toplevel
    /// — display_prefix derivation is fed `std.fs.path.basename(toplevel)`,
    /// so picking a known basename lets tests pin the seeded prefix.
    fn init(gpa: std.mem.Allocator, basename: []const u8) !TmpStore {
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
    var store = try TmpStore.init(gpa, "my-test-repo");
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
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.init_success_fresh) != null);
    try std.testing.expectEqualStrings("", h.stderr());

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    try std.testing.expectEqual(
        @as(?i64, migrations.application_id),
        try migrations.queryOptionalInt(conn, "pragma application_id"),
    );
    try std.testing.expectEqual(
        @as(?i64, 1),
        try migrations.queryOptionalInt(conn, "pragma user_version"),
    );

    if (try conn.row("pragma journal_mode", .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings("wal", r.text(0));
    } else return error.ExpectedRow;

    if (try conn.row("select value from store_config where key = 'display_prefix'", .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings("my-test-repo", r.text(0));
    } else return error.ExpectedRow;
}

test "init: tightens tk dir to mode 0700 on POSIX" {
    if (builtin.os.tag == .windows) return error.SkipZigTest;

    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "my-test-repo");
    defer store.deinit(gpa);

    var h = Harness.init(gpa, &.{});
    defer h.deinit();
    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);

    const tk_dir = try std.fs.path.join(gpa, &.{ store.common_dir_path, "tk" });
    defer gpa.free(tk_dir);
    const st = try std.Io.Dir.cwd().statFile(std.testing.io, tk_dir, .{});
    const mode_bits = @intFromEnum(st.permissions) & 0o777;
    try std.testing.expectEqual(@as(@TypeOf(mode_bits), 0o700), mode_bits);
}

test "init: leaves an existing tk dir's permissions untouched on POSIX" {
    if (builtin.os.tag == .windows) return error.SkipZigTest;

    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "my-test-repo");
    defer store.deinit(gpa);

    // Pre-create <common>/tk with broader permissions, mimicking a user who
    // wants the store readable by their own group.
    const tk_dir = try std.fs.path.join(gpa, &.{ store.common_dir_path, "tk" });
    defer gpa.free(tk_dir);
    try std.Io.Dir.cwd().createDirPath(std.testing.io, tk_dir);
    try std.Io.Dir.cwd().setFilePermissions(std.testing.io, tk_dir, @enumFromInt(0o755), .{});

    var h = Harness.init(gpa, &.{});
    defer h.deinit();
    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);

    const st = try std.Io.Dir.cwd().statFile(std.testing.io, tk_dir, .{});
    const mode_bits = @intFromEnum(st.permissions) & 0o777;
    try std.testing.expectEqual(@as(@TypeOf(mode_bits), 0o755), mode_bits);
}

test "init: idempotent on second run preserves an externally-set prefix" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "my-test-repo");
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

    // Overwrite the seeded prefix between runs. A regression that recomputes
    // on every init (instead of skipping when present) would clobber this.
    {
        const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer conn.close();
        try conn.exec(
            "update store_config set value = ?1 where key = 'display_prefix'",
            .{"sentinel-value"},
        );
    }

    {
        var h = Harness.init(gpa, &.{});
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });
        const code = try run(h.deps(), &h.iter);
        try std.testing.expectEqual(@as(u8, 0), code);
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.init_success_existing) != null);
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try std.testing.expectEqual(
        @as(?i64, 1),
        try migrations.queryOptionalInt(conn, "select count(*) from schema_migrations"),
    );
    if (try conn.row("select value from store_config where key = 'display_prefix'", .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings("sentinel-value", r.text(0));
    } else return error.ExpectedRow;
}

test "init: refuses to overwrite a non-Ticket SQLite file with tables" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "my-test-repo");
    defer store.deinit(gpa);

    const tk_dir = try std.fs.path.join(gpa, &.{ store.common_dir_path, "tk" });
    defer gpa.free(tk_dir);
    try std.Io.Dir.cwd().createDirPath(std.testing.io, tk_dir);

    {
        const foreign = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.Create | zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer foreign.close();
        try foreign.execNoArgs("create table other_app(x integer)");
    }

    var h = Harness.init(gpa, &.{});
    defer h.deinit();
    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.init_refuse_foreign) != null);

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try std.testing.expectEqual(
        @as(?i64, 1),
        try migrations.queryOptionalInt(conn, "select count(*) from sqlite_master where name='other_app'"),
    );
}

test "init: refuses a foreign SQLite file even when its application_id is non-zero" {
    // SQLite only persists `pragma application_id` to the database header
    // when it is committed alongside a real schema write, so this exercises
    // the `app_id != ours and app_id != 0` branch of `classify` rather than
    // the `app_id == 0 with tables` branch covered by the previous test.
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "my-test-repo");
    defer store.deinit(gpa);

    const tk_dir = try std.fs.path.join(gpa, &.{ store.common_dir_path, "tk" });
    defer gpa.free(tk_dir);
    try std.Io.Dir.cwd().createDirPath(std.testing.io, tk_dir);

    {
        const foreign = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.Create | zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer foreign.close();
        try foreign.execNoArgs(
            \\begin;
            \\pragma application_id = 0x12345678;
            \\create table other_app (x integer);
            \\insert into other_app values (1);
            \\commit;
        );
    }

    var h = Harness.init(gpa, &.{});
    defer h.deinit();
    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.init_refuse_foreign) != null);

    // Confirm we did not touch the foreign data.
    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try std.testing.expectEqual(
        @as(?i64, 0x12345678),
        try migrations.queryOptionalInt(conn, "pragma application_id"),
    );
    try std.testing.expectEqual(
        @as(?i64, 1),
        try migrations.queryOptionalInt(conn, "select count(*) from other_app"),
    );
}

test "init: rejects a store created by a future Ticket version" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "my-test-repo");
    defer store.deinit(gpa);

    {
        var h = Harness.init(gpa, &.{});
        defer h.deinit();
        const stdout_payload = try store.gitRevParseStdout(gpa);
        defer gpa.free(stdout_payload);
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });
        const code = try run(h.deps(), &h.iter);
        try std.testing.expectEqual(@as(u8, 0), code);
    }

    {
        const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer conn.close();
        try conn.exec(
            "insert into schema_migrations(version, applied_at) values (?1, ?2)",
            .{ @as(i64, 999), "2099-01-01T00:00:00.000Z" },
        );
    }

    var h = Harness.init(gpa, &.{});
    defer h.deinit();
    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.init_refuse_future_version) != null);
}
