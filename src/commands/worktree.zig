//! `tk worktree` — inspect and configure Workspace Scope.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const worktree_scope = @import("../worktree/scope.zig");
const git_discovery = @import("../git/discovery.zig");
const clock_mod = @import("../clock.zig");
const ItemClass = @import("../domain/item_class.zig").ItemClass;

/// Dispatcher metadata for `tk worktree`.
pub const meta: cli.CommandMeta = .{
    .name = "worktree",
    .description = "Inspect or configure the current Workspace Scope",
};

/// Parse `tk worktree` args, dispatch to subcommand or report status.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    // Manual subcommand peek before zig-clap parses anything else, so each
    // arm can own its own parameter shape (`set <id>`, `start <id> [path]
    // [--no-status]`). With no subcommand we fall through to the read-only
    // status report.
    const sub = args_iter.next();
    if (sub) |s| {
        if (std.mem.eql(u8, s, "-h") or std.mem.eql(u8, s, "--help")) {
            writeHelp(deps) catch {};
            return 0;
        }
        if (std.mem.eql(u8, s, "clear")) return try runClear(deps);
        if (std.mem.eql(u8, s, "set")) return try runSet(deps, args_iter);
        if (std.mem.eql(u8, s, "start")) return try runStart(deps, args_iter);
    }
    return try runStatus(deps);
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk worktree - inspect or configure Workspace Scope
        \\
        \\Usage:
        \\  tk worktree                     Report configured/inferred scope.
        \\  tk worktree set <id>            Configure Workspace Scope for this worktree.
        \\  tk worktree clear               Remove configured Workspace Scope.
        \\  tk worktree start <id> [path]   Create a Ticket Branch and scoped git worktree.
        \\
        \\Options for `start`:
        \\  --no-status   Skip marking the scoped item active.
        \\
        \\Options:
        \\  -h, --help    Display this help and exit.
        \\
    );
}

fn runStatus(deps: cli.Deps) !u8 {
    const open_outcome = repository.openExisting(deps.gpa, deps.runner, deps.cwd) catch |err| {
        renderStatusStorageError(deps, err);
        return 1;
    };
    const store = switch (open_outcome) {
        .ok => |store| store,
        else => {
            repository.renderOpenFailure(deps.stderr, deps.gpa, "worktree", messages.worktree_status_missing_store, open_outcome);
            return 1;
        },
    };
    defer store.close();

    const raw = worktree_scope.readGitSide(deps.gpa, deps.runner, deps.cwd) catch |err| {
        renderStatusStorageError(deps, err);
        return 1;
    };
    defer worktree_scope.freeRaw(deps.gpa, raw);

    const scope_outcome = worktree_scope.resolveAgainstStore(store, deps.gpa, raw) catch |err| {
        renderStatusStorageError(deps, err);
        return 1;
    };
    switch (scope_outcome) {
        .none => {
            try deps.stdout.print("{s}\n", .{messages.worktree_no_scope});
            return 0;
        },
        .scope => |s| {
            defer s.deinit(deps.gpa);
            try deps.stdout.print("Scope:  {s} - {s}\n", .{ s.display_id, s.title });
            switch (s.source) {
                .configured => try deps.stdout.writeAll("Source: configured\n"),
                .inferred => try deps.stdout.print(
                    "Source: inferred from branch '{s}'\n",
                    .{raw.branch_name.?},
                ),
            }
            return 0;
        },
        .configured_unresolved => |stored| {
            defer deps.gpa.free(stored);
            deps.stderr.print(
                "{s}{s}{s}\n",
                .{ messages.worktree_status_unresolved_prefix, stored, messages.worktree_status_unresolved_suffix },
            ) catch {};
            return 1;
        },
    }
}

fn renderStatusStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, .{
        .busy_retry = messages.worktree_status_store_busy_retry,
        .out_of_memory = messages.worktree_status_out_of_memory,
        .fallback = messages.worktree_status_read_failed,
    });
}

fn runStart(deps: cli.Deps, args_iter: anytype) !u8 {
    var id_arg: ?[]const u8 = null;
    var path_arg: ?[]const u8 = null;
    var no_status = false;
    while (args_iter.next()) |arg| {
        if (std.mem.eql(u8, arg, "--no-status")) {
            no_status = true;
        } else if (id_arg == null) {
            id_arg = arg;
        } else if (path_arg == null) {
            path_arg = arg;
        }
    }
    const id = id_arg orelse {
        deps.stderr.print("{s}\n", .{messages.worktree_start_id_required}) catch {};
        return 2;
    };

    const open_outcome = repository.openExisting(deps.gpa, deps.runner, deps.cwd) catch |err| {
        renderStartStorageError(deps, err);
        return 1;
    };
    const store = switch (open_outcome) {
        .ok => |store| store,
        else => {
            repository.renderOpenFailure(deps.stderr, deps.gpa, "worktree start", messages.worktree_start_missing_store, open_outcome);
            return 1;
        },
    };
    defer store.close();

    const resolved = (repository.resolveItemRef(store, deps.gpa, id) catch |err| {
        renderStartStorageError(deps, err);
        return 1;
    }) orelse {
        deps.stderr.print(
            "{s}{s}{s}\n",
            .{ messages.worktree_start_id_not_found_prefix, id, messages.worktree_start_id_not_found_suffix },
        ) catch {};
        return 1;
    };
    defer resolved.deinit(deps.gpa);

    const detail = (repository.showItem(store, deps.gpa, id) catch |err| {
        renderStartStorageError(deps, err);
        return 1;
    }) orelse unreachable; // resolveItemRef just succeeded
    defer detail.deinit(deps.gpa);

    if (detail.status == .done) {
        deps.stderr.print(
            "{s}{s}\n",
            .{ messages.worktree_start_locked_done_prefix, detail.item_class.label() },
        ) catch {};
        return 1;
    }

    const branch_slug = try worktree_scope.sanitize(deps.gpa, detail.title, 40);
    defer deps.gpa.free(branch_slug);
    const path_slug = try worktree_scope.sanitize(deps.gpa, detail.title, 30);
    defer deps.gpa.free(path_slug);

    const branch = try buildBranch(deps.gpa, detail.display_id, branch_slug);
    defer deps.gpa.free(branch);

    const worktree_path = if (path_arg) |p|
        try deps.gpa.dupe(u8, p)
    else
        try buildDefaultPath(deps.gpa, deps.runner, deps.cwd, detail.display_id, path_slug);
    defer deps.gpa.free(worktree_path);

    // git config extensions.worktreeConfig true
    try runGitOrFail(deps, &.{ "git", "config", "extensions.worktreeConfig", "true" }, messages.worktree_start_git_failed) orelse return 1;
    // git worktree add -b <branch> <path>
    try runGitOrFail(deps, &.{ "git", "worktree", "add", "-b", branch, worktree_path }, messages.worktree_start_git_failed) orelse return 1;
    // git -C <path> config --worktree tk.scope <id>
    try runGitOrFail(deps, &.{ "git", "-C", worktree_path, "config", "--worktree", "tk.scope", id }, messages.worktree_start_git_failed) orelse return 1;

    if (!no_status) {
        const set_outcome = repository.setItemStatus(store, deps.gpa, deps.clock, .{
            .id = resolved.id,
            .status = .active,
        }) catch |err| {
            renderStartStorageError(deps, err);
            return 1;
        };
        // Status flip succeeded or was idempotent — either way the worktree
        // is in place. `.locked_done` is impossible here because we
        // pre-checked detail.status above; future races are caught by the
        // schema trigger from ADR 0006.
        switch (set_outcome) {
            .ok => |snap| snap.deinit(deps.gpa),
            .not_found => unreachable,
            .locked_done => unreachable,
        }
    }

    try deps.stdout.print("{s}{s}: {s} - {s}\n", .{
        messages.worktree_start_success_prefix,
        detail.item_class.label(),
        detail.display_id,
        detail.title,
    });
    if (!no_status) try deps.stdout.writeAll("Status: active\n");
    try deps.stdout.print("Branch: {s}\n", .{branch});
    try deps.stdout.print("Path:   {s}\n", .{worktree_path});
    return 0;
}

fn buildBranch(gpa: std.mem.Allocator, display_id: []const u8, slug: []const u8) ![]u8 {
    if (slug.len == 0) return try std.fmt.allocPrint(gpa, "tk/{s}", .{display_id});
    return try std.fmt.allocPrint(gpa, "tk/{s}-{s}", .{ display_id, slug });
}

/// Compute the default worktree path: `<parent-of-main-toplevel>/<repo>.<id-sanitized>-<slug>`.
///
/// `<parent-of-main-toplevel>` is derived from `git rev-parse --show-toplevel`
/// of the current worktree; this matches the v1 simplification noted in Q12
/// that running from inside a linked worktree may nest paths in unexpected
/// places. Configurable layout is deferred.
fn buildDefaultPath(
    gpa: std.mem.Allocator,
    runner: anytype,
    cwd: std.Io.Dir,
    display_id: []const u8,
    slug: []const u8,
) ![]u8 {
    const outcome = try git_discovery.discoverPaths(gpa, runner, cwd);
    switch (outcome) {
        .ok => |paths| {
            var p = paths;
            defer p.deinit(gpa);
            const parent = std.fs.path.dirname(p.toplevel) orelse "/";
            const repo = std.fs.path.basename(p.toplevel);
            const id_sanitized = try sanitizeIdForPath(gpa, display_id);
            defer gpa.free(id_sanitized);
            const leaf = if (slug.len == 0)
                try std.fmt.allocPrint(gpa, "{s}.{s}", .{ repo, id_sanitized })
            else
                try std.fmt.allocPrint(gpa, "{s}.{s}-{s}", .{ repo, id_sanitized, slug });
            defer gpa.free(leaf);
            return try std.fs.path.join(gpa, &.{ parent, leaf });
        },
        else => |inner| {
            git_discovery.renderFailure(undefined, gpa, "worktree start", inner);
            return error.GitDiscoveryFailed;
        },
    }
}

/// Lowercase Display ID and replace `/`, `:`, `#` with `-` for filesystem
/// safety, then collapse consecutive `-`. Matches the path-safety rule
/// recorded in the implementation contract.
fn sanitizeIdForPath(gpa: std.mem.Allocator, display_id: []const u8) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(gpa);
    var prev_dash = false;
    for (display_id) |c| {
        const lower = std.ascii.toLower(c);
        const mapped: u8 = switch (lower) {
            '/', ':', '#' => '-',
            else => lower,
        };
        if (mapped == '-') {
            if (!prev_dash and out.items.len > 0) {
                try out.append(gpa, '-');
                prev_dash = true;
            }
        } else {
            try out.append(gpa, mapped);
            prev_dash = false;
        }
    }
    if (out.items.len > 0 and out.items[out.items.len - 1] == '-') _ = out.pop();
    return try out.toOwnedSlice(gpa);
}

fn renderStartStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, .{
        .busy_retry = messages.worktree_start_store_busy_retry,
        .out_of_memory = messages.worktree_start_out_of_memory,
        .fallback = messages.worktree_start_write_failed,
    });
}

fn runSet(deps: cli.Deps, args_iter: anytype) !u8 {
    const id = args_iter.next() orelse {
        deps.stderr.print("{s}\n", .{messages.worktree_set_id_required}) catch {};
        return 2;
    };

    const open_outcome = repository.openExisting(deps.gpa, deps.runner, deps.cwd) catch |err| {
        renderSetStorageError(deps, err);
        return 1;
    };
    const store = switch (open_outcome) {
        .ok => |store| store,
        else => {
            repository.renderOpenFailure(deps.stderr, deps.gpa, "worktree set", messages.worktree_set_missing_store, open_outcome);
            return 1;
        },
    };
    defer store.close();

    if (try repository.resolveItemRef(store, deps.gpa, id)) |resolved| {
        defer resolved.deinit(deps.gpa);
    } else {
        deps.stderr.print(
            "{s}{s}{s}\n",
            .{ messages.worktree_set_id_not_found_prefix, id, messages.worktree_set_id_not_found_suffix },
        ) catch {};
        return 1;
    }

    // Ensure the repo opts into per-worktree config before we write
    // `tk.scope` against the current worktree. The set is itself
    // idempotent at git level: writing `true` over an existing `true`
    // is a no-op rewrite.
    try runGitOrFail(deps, &.{ "git", "config", "extensions.worktreeConfig", "true" }, messages.worktree_set_failed) orelse return 1;
    try runGitOrFail(deps, &.{ "git", "config", "--worktree", "tk.scope", id }, messages.worktree_set_failed) orelse return 1;

    try deps.stdout.print("{s}{s}\n", .{ messages.worktree_set_prefix, id });
    return 0;
}

/// Run a git subprocess, returning `true` on success and surfacing the failure
/// diagnostic + exit-1 path through `null` so callers can `orelse return 1`.
fn runGitOrFail(deps: cli.Deps, argv: []const []const u8, failure_msg: []const u8) !?void {
    var result = deps.runner.run(deps.gpa, .{ .argv = argv, .cwd = deps.cwd }) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.ExecutableNotFound, error.SpawnFailed => {
            deps.stderr.print("{s}\n{s}\n", .{ failure_msg, @errorName(err) }) catch {};
            return null;
        },
    };
    defer result.deinit(deps.gpa);
    if (result.exit_code != 0) {
        deps.stderr.print("{s}\n", .{failure_msg}) catch {};
        return null;
    }
    return;
}

fn renderSetStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, .{
        .busy_retry = messages.worktree_set_store_busy_retry,
        .out_of_memory = messages.worktree_set_out_of_memory,
        .fallback = messages.worktree_set_read_failed,
    });
}

fn runClear(deps: cli.Deps) !u8 {
    var result = deps.runner.run(deps.gpa, .{
        .argv = &.{ "git", "config", "--worktree", "--unset", "tk.scope" },
        .cwd = deps.cwd,
    }) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.ExecutableNotFound, error.SpawnFailed => {
            deps.stderr.print("{s}\n{s}\n", .{ messages.worktree_clear_failed, @errorName(err) }) catch {};
            return 1;
        },
    };
    defer result.deinit(deps.gpa);

    // Exit code 5 means the key was already absent; per Q8 that is the
    // idempotent no-op success path. Any other non-zero exit is a real
    // failure worth surfacing.
    if (result.exit_code != 0 and result.exit_code != 5) {
        deps.stderr.print("{s}\n", .{messages.worktree_clear_failed}) catch {};
        return 1;
    }
    try deps.stdout.print("{s}\n", .{messages.worktree_cleared});
    return 0;
}

const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
const init_command = @import("init.zig");
const zqlite = @import("zqlite");

const StoreFixture = struct {
    tmp_store: TmpStore,
    cwd: std.Io.Dir,
    rev_parse: []u8,

    fn init(gpa: std.mem.Allocator) !StoreFixture {
        var tmp_store = try TmpStore.init(gpa, "project");
        errdefer tmp_store.deinit(gpa);
        var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
        errdefer cwd.close(std.testing.io);
        const rev_parse = try tmp_store.gitRevParseStdout(gpa);
        errdefer gpa.free(rev_parse);

        {
            var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
            defer h.deinit();
            try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
            try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
        }

        return .{ .tmp_store = tmp_store, .cwd = cwd, .rev_parse = rev_parse };
    }

    fn deinit(self: *StoreFixture, gpa: std.mem.Allocator) void {
        gpa.free(self.rev_parse);
        self.cwd.close(std.testing.io);
        self.tmp_store.deinit(gpa);
    }
};

test "worktree: with configured scope prints two-line block" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "Implement scope", .created_seq = 1 });

    var h = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 0, .stdout = "project-1\n" });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        \\Scope:  project-1 - Implement scope
        \\Source: configured
        \\
    , h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree: with inferred branch scope prints provenance hint" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "Implement scope", .created_seq = 1 });

    var h = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 0, .stdout = "tk/project-1-implement-scope\n" });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        \\Scope:  project-1 - Implement scope
        \\Source: inferred from branch 'tk/project-1-implement-scope'
        \\
    , h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree: with unresolved configured scope exits 1 with diagnostic" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 0, .stdout = "ghost-42\n" });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        messages.worktree_status_unresolved_prefix ++ "ghost-42" ++ messages.worktree_status_unresolved_suffix ++ "\n",
        h.stderr(),
    );
}

test "worktree start: success block lists Ticket, status, branch, and absolute path" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Implement worktree start",
        .created_seq = 1,
    });

    // Expected derived names.
    const branch = "tk/project-1-implement-worktree-start";
    const path_leaf = "project.project-1-implement-worktree-start";
    const parent = std.fs.path.dirname(fixture.tmp_store.toplevel_path).?;
    const path = try std.fs.path.join(gpa, &.{ parent, path_leaf });
    defer gpa.free(path);

    var h = Harness.initWith(gpa, &.{ "start", "project-1" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "extensions.worktreeConfig", "true" }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "worktree", "add", "-b", branch, path }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "-C", path, "config", "--worktree", "tk.scope", "project-1" }, .{ .exit_code = 0 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    const expected = try std.fmt.allocPrint(gpa,
        \\Created worktree for Ticket: project-1 - Implement worktree start
        \\Status: active
        \\Branch: {s}
        \\Path:   {s}
        \\
    , .{ branch, path });
    defer gpa.free(expected);
    try std.testing.expectEqualStrings(expected, h.stdout());

    // Ticket should be active.
    const row = (try conn.row("select status from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("active", row.text(0));
}

test "worktree start: rejects a done Ticket uniformly per ADR 0006" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Done already",
        .status = "done",
        .created_seq = 1,
    });

    var h = Harness.initWith(gpa, &.{ "start", "project-1" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        messages.worktree_start_locked_done_prefix ++ "Ticket\n",
        h.stderr(),
    );
}

test "worktree start: --no-status omits the Status line and leaves item open" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Implement worktree start",
        .created_seq = 1,
    });

    const branch = "tk/project-1-implement-worktree-start";
    const path_leaf = "project.project-1-implement-worktree-start";
    const parent = std.fs.path.dirname(fixture.tmp_store.toplevel_path).?;
    const path = try std.fs.path.join(gpa, &.{ parent, path_leaf });
    defer gpa.free(path);

    var h = Harness.initWith(gpa, &.{ "start", "project-1", "--no-status" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "extensions.worktreeConfig", "true" }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "worktree", "add", "-b", branch, path }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "-C", path, "config", "--worktree", "tk.scope", "project-1" }, .{ .exit_code = 0 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    // No Status line; item should remain `open`.
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Status:") == null);
    const row = (try conn.row("select status from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("open", row.text(0));
}

test "worktree start: unknown id exits 1" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{ "start", "no-such-id" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        messages.worktree_start_id_not_found_prefix ++ "no-such-id" ++ messages.worktree_start_id_not_found_suffix ++ "\n",
        h.stderr(),
    );
}

test "worktree start: missing positional id exits 2" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"start"});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(messages.worktree_start_id_required ++ "\n", h.stderr());
}

test "worktree: with no scope prints No Workspace Scope and exits 0" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("No Workspace Scope.\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree clear: runs git config --unset and prints success" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--unset", "tk.scope" }, .{ .exit_code = 0 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Workspace Scope cleared\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree clear: exit code 5 from git config --unset is the idempotent no-op" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--unset", "tk.scope" }, .{ .exit_code = 5 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Workspace Scope cleared\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree set: missing positional id exits 2 with usage" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"set"});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.worktree_set_id_required ++ "\n", h.stderr());
}

test "worktree set: unknown id exits 1 with role-specific diagnostic" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{ "set", "no-such-id" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        messages.worktree_set_id_not_found_prefix ++ "no-such-id" ++ messages.worktree_set_id_not_found_suffix ++ "\n",
        h.stderr(),
    );
}

test "worktree set: git config failure surfaces as exit 1" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "T", .created_seq = 1 });

    var h = Harness.initWith(gpa, &.{ "set", "project-1" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "extensions.worktreeConfig", "true" }, .{ .exit_code = 128 });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.worktree_set_failed ++ "\n", h.stderr());
}

test "worktree set: validates id, enables per-worktree config, writes tk.scope" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "Implement", .created_seq = 1 });

    var h = Harness.initWith(gpa, &.{ "set", "project-1" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "extensions.worktreeConfig", "true" }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "tk.scope", "project-1" }, .{ .exit_code = 0 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Set Workspace Scope to project-1\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree clear: other non-zero git exit surfaces as exit 1" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--unset", "tk.scope" }, .{ .exit_code = 128 });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.worktree_clear_failed ++ "\n", h.stderr());
}
