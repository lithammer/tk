//! `tk init` — create the Repository Store at `<git-common-dir>/tk/ticket.db`.

const std = @import("std");
const builtin = @import("builtin");
const clap = @import("clap");
const zqlite = @import("zqlite");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const migrations = @import("../store/migrations.zig");
const Diagnostic = @import("../store/diagnostic.zig").Diagnostic;
const display_prefix = @import("../domain/display_prefix.zig");
const discovery = @import("../git/discovery.zig");

/// Dispatcher metadata for `tk init`.
pub const meta: cli.CommandMeta = .{
    .name = "init",
    .description = "Initialize the Repository Store in the current Git repository",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\
);

/// Parse `tk init` flags and create or migrate the Repository Store.
///
/// This command is intentionally flagless in slice 2 except for `--help`.
/// Store discovery is tied to Git's common directory so linked worktrees share
/// one untracked Repository Store.
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

/// Execute Repository Store discovery, classification, migration, and prefix
/// seeding after command-line parsing has succeeded.
fn execute(deps: cli.Deps) !u8 {
    const discovery_outcome = try discovery.discoverPaths(deps.gpa, deps.runner, deps.cwd);
    var paths = switch (discovery_outcome) {
        .ok => |ok| ok,
        else => {
            discovery.renderFailure(deps.stderr, deps.gpa, "init", discovery_outcome);
            return 1;
        },
    };
    defer paths.deinit(deps.gpa);

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

    var migration_diag: Diagnostic = .{};
    migrations.applyAll(conn, now_iso, &migration_diag) catch |err| switch (err) {
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
                .{ @errorName(err), migration_diag.message() },
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

/// Classification of the SQLite file at `<git-common-dir>/tk/ticket.db`.
///
/// `tk init` inspects before mutating so a foreign SQLite file can be refused
/// without changing its journal mode or application_id.
pub const StoreKind = enum {
    /// File was just created by sqlite3_open_v2 and contains no schema yet.
    fresh,
    /// Existing Ticket Repository Store (matching application_id).
    ours,
    /// Existing SQLite file written by something else.
    foreign,
};

/// Classify an opened SQLite connection as fresh, ours, or foreign.
pub fn classify(conn: zqlite.Conn) migrations.QueryError!StoreKind {
    const app_id = (try migrations.queryOptionalInt(conn, "pragma application_id")) orelse 0;
    if (app_id == migrations.application_id) return .ours;

    const table_count = (try migrations.queryOptionalInt(conn, "select count(*) from sqlite_master")) orelse 0;
    if (app_id == 0 and table_count == 0) return .fresh;
    return .foreign;
}

/// Apply connection and file pragmas required by the Repository Store.
fn configureForTicketStore(conn: zqlite.Conn) zqlite.Error!void {
    // journal_mode persists in the file header; foreign_keys and
    // busy_timeout are connection-scoped and have to be set every open.
    try conn.execNoArgs("pragma journal_mode = wal");
    try conn.busyTimeout(5000);
    try conn.execNoArgs("pragma foreign_keys = on");
}

/// Seed `store_config.display_prefix` from the repository basename when the
/// store does not already carry an explicit prefix.
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

/// Tighten a freshly-created Repository Store directory on platforms that
/// expose Unix-style permissions.
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
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

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

    // application_id and user_version are covered by the migration-level
    // test in src/store/migrations.zig. journal_mode and display_prefix are
    // unique to init: configureForTicketStore and seedDisplayPrefix don't
    // run inside applyAll.
    if (try conn.row("pragma journal_mode", .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings("wal", r.text(0));
    } else return error.ExpectedRow;

    if (try conn.row("select value from store_config where key = 'display_prefix'", .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings("my-test-repo", r.text(0));
    } else return error.ExpectedRow;
}

test "init: only tightens permissions on directories it created" {
    if (builtin.os.tag == .windows) return error.SkipZigTest;

    const gpa = std.testing.allocator;

    // Half 1: tk dir does not exist beforehand → init creates it at 0700.
    {
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

    // Half 2: tk dir pre-created at 0o755 → init leaves it alone.
    {
        var store = try TmpStore.init(gpa, "my-test-repo");
        defer store.deinit(gpa);

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

fn openMemoryConn() !zqlite.Conn {
    return try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
}

test "classify: a freshly-created database is .fresh" {
    const conn = try openMemoryConn();
    defer conn.close();

    try std.testing.expectEqual(StoreKind.fresh, try classify(conn));
}

test "classify: a Ticket Repository Store is .ours" {
    const conn = try openMemoryConn();
    defer conn.close();

    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try std.testing.expectEqual(StoreKind.ours, try classify(conn));
}

test "classify: a SQLite file with foreign tables is .foreign" {
    const conn = try openMemoryConn();
    defer conn.close();

    try conn.execNoArgs("create table other_app(x integer)");

    try std.testing.expectEqual(StoreKind.foreign, try classify(conn));
}

test "classify: a SQLite file with a foreign application_id is .foreign" {
    // SQLite only persists `pragma application_id` to the database header
    // when committed alongside a real write, so the pragma is paired with a
    // table+insert in one transaction.
    const conn = try openMemoryConn();
    defer conn.close();

    try conn.execNoArgs(
        \\begin;
        \\pragma application_id = 0x12345678;
        \\create table other_app (x integer);
        \\insert into other_app values (1);
        \\commit;
    );

    try std.testing.expectEqual(StoreKind.foreign, try classify(conn));
}

test "init: surfaces the foreign-store diagnostic on stderr" {
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
}

test "init: rejects a store created by a future Ticket version" {
    // The migration-level test in src/store/migrations.zig covers the
    // future-version detection itself. This test only asserts the
    // user-visible diagnostic phrasing reaches stderr through tk init.
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "my-test-repo");
    defer store.deinit(gpa);

    const tk_dir = try std.fs.path.join(gpa, &.{ store.common_dir_path, "tk" });
    defer gpa.free(tk_dir);
    try std.Io.Dir.cwd().createDirPath(std.testing.io, tk_dir);

    {
        const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.Create | zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer conn.close();
        try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
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

test "init: surfaces SQLite error when migration fails" {
    // Pin the user-visible behavior of migrations.Diagnostic flowing through
    // tk init: a Ticket-classified store whose schema conflicts with
    // migration 1 must produce a stderr line that includes the SQLite errmsg
    // ("items"), not just the literal "migration failed:" prefix. A
    // regression that drops `&migration_diag` (passes null) or reorders the
    // format args would not be caught by migration-level tests alone.
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "my-test-repo");
    defer store.deinit(gpa);

    const tk_dir = try std.fs.path.join(gpa, &.{ store.common_dir_path, "tk" });
    defer gpa.free(tk_dir);
    try std.Io.Dir.cwd().createDirPath(std.testing.io, tk_dir);

    {
        // Set application_id so classify returns .ours, but leave
        // schema_migrations empty and pre-create `items` so migration 1's
        // `create table items` fails inside applyAll's transaction.
        const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.Create | zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer conn.close();
        var pragma_buf: [128]u8 = undefined;
        const pragma_sql = std.fmt.bufPrintZ(
            &pragma_buf,
            "begin; pragma application_id = {d}; create table items(x integer); commit;",
            .{migrations.application_id},
        ) catch unreachable;
        try conn.execNoArgs(pragma_sql);
    }

    var h = Harness.init(gpa, &.{});
    defer h.deinit();
    const stdout_payload = try store.gitRevParseStdout(gpa);
    defer gpa.free(stdout_payload);
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = stdout_payload });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "migration failed") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "items") != null);
}

test "init: surfaces unparseable rev-parse output" {
    // Pin that the .git_output_unparseable arm of the discovery switch
    // routes to messages.init_git_unparseable. The variant itself is
    // unit-tested in src/git/discovery.zig; this test covers the wiring.
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = "" });

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.init_git_unparseable) != null);
}
