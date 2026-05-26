//! `tk self-update` — replace the running tk binary with the latest release.
//!
//! Forward-only in v1; downgrade and Linux ABI variant switching go through
//! the install script with `TK_VERSION=...` or `TK_LINUX_ABI=...`. Refuses
//! to run on development builds (`build_options.triple == "dev"`). See ADR
//! 0013 and ticket tk-32 for the design and the resolved design notes.

const std = @import("std");
const Allocator = std.mem.Allocator;
const build_options = @import("build_options");
const clap = @import("clap");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const platform = @import("../platform.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");

/// Triple-string sentinel value embedded in non-release builds. The dev
/// refusal in `run` compares against this constant rather than re-spelling
/// the literal at the call site.
pub const dev_triple = "dev";

/// GitHub Releases API endpoint for the latest published release of `tk`.
/// Returns `releases/latest` JSON; we parse only `tag_name` from it. See
/// tk-32 design notes — asset URL is constructed separately from the
/// embedded triple and fetched via the stable
/// `releases/latest/download/<asset>` redirect.
pub const api_url = "https://api.github.com/repos/lithammer/tk/releases/latest";

/// Browser-facing base URL for a release tag. Used both to construct the
/// asset download URL (`<base>/latest/download/<asset>`) and the manual-
/// install diagnostic when an asset is 404 (`<base>/tag/<tag>`).
pub const releases_base_url = "https://github.com/lithammer/tk/releases";

/// Stage-file prefix used inside the exe directory. Hidden-dot keeps the
/// staged binary out of `ls` output, mirroring `commands/manpage.zig`'s
/// `.tk.1.tmp.` convention. The hex suffix is 64 random bits via
/// `deps.random`.
pub const stage_name_prefix = ".tk.tmp.";

/// Stage-name buffer size: prefix + 16 hex characters.
const stage_name_buf_size = stage_name_prefix.len + 16;

/// Dispatcher metadata for `tk self-update`.
pub const meta: cli.CommandMeta = .{
    .name = "self-update",
    .description = "Replace the running tk binary with the latest release",
};

const params = clap.parseParamsComptime(
    \\-h, --help   Display this help and exit.
    \\    --check  Check whether an update is available; do not download.
    \\
);

/// Entry point for `tk self-update`. Thin wrapper that reads the embedded
/// version and triple from `build_options` and delegates to `runWith` so
/// tests can inject non-`dev` values without forking the build.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    return runWith(deps, args_iter, build_options.version, build_options.triple);
}

/// Workhorse for `tk self-update`. `embedded_version` and `embedded_triple`
/// come from `build_options` in production and are passed explicitly in
/// tests so the dev-build refusal branch can be exercised separately from
/// the comparison branches.
///
/// Refusal on dev builds is symmetric across `tk self-update` and
/// `tk self-update --check`: both early-return exit 1 with the same
/// diagnostic before any flag parsing happens. A dev build has no canonical
/// upstream tag to compare against, so even the read-only `--check` query
/// has nothing meaningful to report.
fn runWith(
    deps: cli.Deps,
    args_iter: anytype,
    embedded_version: []const u8,
    embedded_triple: []const u8,
) !u8 {
    if (std.mem.eql(u8, embedded_triple, dev_triple)) {
        deps.stderr.writeAll(messages.self_update_dev_build ++ "\n") catch {};
        return 1;
    }

    var res = (try parse_diagnostic.parseOrReportUsage(clap.Help, &params, clap.parsers.default, args_iter, .{
        .stderr = deps.stderr,
        .allocator = deps.gpa,
        .command = .{ .subcommand = meta.name },
    })) orelse return 2;
    defer res.deinit();

    if (res.args.help != 0) {
        writeHelp(deps) catch {};
        return 0;
    }

    const check_only = res.args.check != 0;

    const fetch = fetchLatestTag(deps);
    switch (fetch) {
        .ok => |tag| {
            defer deps.gpa.free(tag);
            const cmp = compareVersions(embedded_version, tag) catch |err| {
                renderVersionParseFailure(deps, err, embedded_version, tag);
                return 1;
            };

            // `--check` is purely a query: render the comparison result
            // and exit per the query-subcommand 0/1 convention.
            if (check_only) return renderCheckResult(deps, cmp, embedded_version, tag);

            // Non-`--check`: `.up_to_date` and `.ahead` still print the
            // diagnostic and exit 0; only `.newer_available` triggers the
            // download / smoke / rename flow.
            switch (cmp) {
                .up_to_date, .ahead => return renderCheckResult(deps, cmp, embedded_version, tag),
                .newer_available => return performFullUpdate(deps, embedded_triple, tag),
            }
        },
        .network_error => {
            deps.stderr.writeAll(messages.self_update_query_network ++ "\n") catch {};
            return 1;
        },
        .tls_error => {
            deps.stderr.writeAll(messages.self_update_query_tls ++ "\n") catch {};
            return 1;
        },
        .http_status => |code| {
            deps.stderr.print(messages.self_update_query_http_status_prefix ++ "{d}\n", .{code}) catch {};
            return 1;
        },
        .malformed_json => {
            deps.stderr.writeAll(messages.self_update_query_malformed ++ "\n") catch {};
            return 1;
        },
        .missing_tag_field => {
            deps.stderr.writeAll(messages.self_update_query_missing_tag ++ "\n") catch {};
            return 1;
        },
        .out_of_memory => {
            deps.stderr.writeAll(messages.self_update_out_of_memory ++ "\n") catch {};
            return 1;
        },
    }
}

/// Outcome of querying `releases/latest`. `ok.tag` is owned by the caller's
/// allocator; every other arm carries no payload to free, so callers
/// switch and clean up the `.ok` payload at the use site directly (no
/// asymmetric `deinit`).
const FetchTagOutcome = union(enum) {
    ok: []u8,
    network_error,
    tls_error,
    http_status: u16,
    malformed_json,
    missing_tag_field,
    out_of_memory,
};

/// Minimal JSON shape parsed out of the API response. `tag_name` is
/// optional so a literally absent key surfaces through the dedicated
/// `missing_tag_field` outcome rather than getting absorbed into
/// `malformed_json`. `ignore_unknown_fields` keeps the schema forward-
/// compatible with new fields GitHub may add.
const ReleaseJson = struct { tag_name: ?[]const u8 = null };

fn fetchLatestTag(deps: cli.Deps) FetchTagOutcome {
    var resp = deps.http.getJson(deps.gpa, api_url) catch |err| return switch (err) {
        error.NetworkError => .network_error,
        error.TlsError => .tls_error,
        error.MalformedResponse => .malformed_json,
        error.WriteFailed => .network_error,
        error.OutOfMemory => .out_of_memory,
    };
    defer resp.deinit(deps.gpa);

    if (resp.status < 200 or resp.status >= 300) return .{ .http_status = resp.status };

    var parsed = std.json.parseFromSlice(
        ReleaseJson,
        deps.gpa,
        resp.body,
        .{ .ignore_unknown_fields = true },
    ) catch return .malformed_json;
    defer parsed.deinit();

    const tag = parsed.value.tag_name orelse return .missing_tag_field;
    if (tag.len == 0) return .missing_tag_field;

    const owned = deps.gpa.dupe(u8, tag) catch return .out_of_memory;
    return .{ .ok = owned };
}

/// Result of comparing the embedded version against the latest published tag.
const Comparison = enum {
    /// Embedded version equals the latest tag — nothing to do.
    up_to_date,
    /// Latest tag is strictly newer than the embedded version.
    newer_available,
    /// Embedded version is strictly newer than the latest tag — the local
    /// build is ahead of the latest published release (e.g. a freshly cut
    /// build or a yanked release). Treated as a benign exit 0 to honor
    /// "forward-only" without surprising the user.
    ahead,
};

const VersionParseError = error{
    EmbeddedVersionUnparseable,
    LatestTagUnparseable,
};

fn compareVersions(embedded_version: []const u8, latest_tag: []const u8) VersionParseError!Comparison {
    const embedded = parseSemver(embedded_version) catch return error.EmbeddedVersionUnparseable;
    const latest = parseSemver(latest_tag) catch return error.LatestTagUnparseable;
    return switch (embedded.order(latest)) {
        .eq => .up_to_date,
        .lt => .newer_available,
        .gt => .ahead,
    };
}

/// Strip a leading `v` (semver.org spec doesn't include it, but the
/// convention is universal) before parsing.
fn parseSemver(s: []const u8) !std.SemanticVersion {
    const stripped = if (s.len > 0 and s[0] == 'v') s[1..] else s;
    return std.SemanticVersion.parse(stripped);
}

fn renderCheckResult(
    deps: cli.Deps,
    cmp: Comparison,
    embedded_version: []const u8,
    latest_tag: []const u8,
) !u8 {
    switch (cmp) {
        .up_to_date => {
            deps.stdout.print(messages.self_update_check_up_to_date_prefix ++ "{s}\n", .{latest_tag}) catch {};
            return 0;
        },
        .newer_available => {
            deps.stdout.print(
                messages.self_update_check_newer_available_prefix ++ "{s} (current: {s})\n",
                .{ latest_tag, embedded_version },
            ) catch {};
            return 1;
        },
        .ahead => {
            deps.stdout.print(
                messages.self_update_check_ahead_prefix ++ "{s} is ahead of latest published release {s}; nothing to do\n",
                .{ embedded_version, latest_tag },
            ) catch {};
            return 0;
        },
    }
}

/// Resolve the running binary's path via `std.process.executablePath`,
/// split into directory + basename, and dispatch to `performUpdate`.
///
/// This is the only thin layer that's hard to unit-test: tests call
/// `performUpdate` directly with a tmpdir as the target. The exe-path
/// resolution itself is exercised by integration through the real
/// binary in smoke / scenario tests.
fn performFullUpdate(deps: cli.Deps, embedded_triple: []const u8, latest_tag: []const u8) !u8 {
    var exe_buf: [std.Io.Dir.max_path_bytes]u8 = undefined;
    const exe_len = std.process.executablePath(deps.io, &exe_buf) catch |err| {
        deps.stderr.print(messages.self_update_exe_resolve_failure_prefix ++ "{s}\n", .{@errorName(err)}) catch {};
        return 1;
    };
    const exe_path = exe_buf[0..exe_len];
    const target_dir_path = std.fs.path.dirname(exe_path) orelse {
        deps.stderr.writeAll(messages.self_update_exe_resolve_failure_prefix ++ "no parent directory\n") catch {};
        return 1;
    };
    const target_name = std.fs.path.basename(exe_path);
    return performUpdate(deps, target_dir_path, target_name, embedded_triple, latest_tag);
}

/// Stage → download → smoke → atomic rename. Pure of `executablePath` so
/// tests can pass a tmpdir as `target_dir_path` and exercise the full
/// pipeline without relying on the running test-binary's location.
///
/// On POSIX, the rename is atomic and the running inode stays alive
/// through the swap. On Windows (Slice G) the same flow is followed
/// through smoke, then a rename-self pattern takes over.
fn performUpdate(
    deps: cli.Deps,
    target_dir_path: []const u8,
    target_name: []const u8,
    embedded_triple: []const u8,
    latest_tag: []const u8,
) !u8 {
    // Compute the stage and target paths up-front so error messages
    // can name the would-be target.
    var stage_name_buf: [stage_name_buf_size]u8 = undefined;
    const stage_name = renderStageName(&stage_name_buf, deps.random);

    const target_path = try std.fs.path.join(deps.gpa, &.{ target_dir_path, target_name });
    defer deps.gpa.free(target_path);
    const stage_path = try std.fs.path.join(deps.gpa, &.{ target_dir_path, stage_name });
    defer deps.gpa.free(stage_path);

    // Open the target directory. Failures here are not the user's
    // permission problem; they mean the resolved exe path is bogus.
    var target_dir = std.Io.Dir.cwd().openDir(deps.io, target_dir_path, .{}) catch |err| {
        deps.stderr.print(messages.self_update_exe_resolve_failure_prefix ++ "{s}: {s}\n", .{ target_dir_path, @errorName(err) }) catch {};
        return 1;
    };
    defer target_dir.close(deps.io);

    // Build the asset URL before staging. The two `try` allocations
    // here can fail with OutOfMemory; doing them first means a failure
    // happens before any stage file exists, so there's nothing to
    // clean up. Putting them after `createFile` would leak the open
    // handle and the on-disk dirent.
    const asset_name = try buildAssetName(deps.gpa, embedded_triple);
    defer deps.gpa.free(asset_name);
    const asset_url = try std.fmt.allocPrint(
        deps.gpa,
        releases_base_url ++ "/latest/download/{s}",
        .{asset_name},
    );
    defer deps.gpa.free(asset_url);

    // Stage file create is the write-access test. `executable_file`
    // permissions (0o777 modulo umask on POSIX) let the smoke
    // subprocess exec the staged binary directly without a follow-up
    // chmod call. AccessDenied is the canonical "no write here" path
    // and fast-fails before any network traffic.
    var stage_file = target_dir.createFile(deps.io, stage_name, .{
        .exclusive = true,
        .permissions = .executable_file,
    }) catch |err| switch (err) {
        error.AccessDenied => {
            deps.stderr.print(messages.self_update_no_write_access_prefix ++ "{s}: permission denied\n", .{target_path}) catch {};
            return 1;
        },
        else => {
            deps.stderr.print(messages.self_update_stage_failure_prefix ++ "{s}: {s}\n", .{ stage_path, @errorName(err) }) catch {};
            return 1;
        },
    };

    // Stream the download into the stage file, buffered through a
    // small fixed-size writer. Any failure during this step deletes
    // the stage file before returning.
    var stage_writer_buf: [4096]u8 = undefined;
    var stage_writer = stage_file.writer(deps.io, &stage_writer_buf);
    const dl_status = deps.http.download(deps.gpa, asset_url, &stage_writer.interface) catch |err| {
        stage_file.close(deps.io);
        target_dir.deleteFile(deps.io, stage_name) catch {};
        switch (err) {
            error.NetworkError => {
                deps.stderr.writeAll(messages.self_update_download_network ++ "\n") catch {};
            },
            error.TlsError => {
                deps.stderr.writeAll(messages.self_update_download_tls ++ "\n") catch {};
            },
            error.MalformedResponse => {
                deps.stderr.writeAll(messages.self_update_download_malformed ++ "\n") catch {};
            },
            error.WriteFailed => {
                deps.stderr.print(messages.self_update_stage_io_failure_prefix ++ "{s}\n", .{@errorName(err)}) catch {};
            },
            error.OutOfMemory => {
                deps.stderr.writeAll(messages.self_update_out_of_memory ++ "\n") catch {};
            },
        }
        return 1;
    };
    stage_writer.interface.flush() catch |err| {
        stage_file.close(deps.io);
        target_dir.deleteFile(deps.io, stage_name) catch {};
        deps.stderr.print(messages.self_update_stage_io_failure_prefix ++ "{s}\n", .{@errorName(err)}) catch {};
        return 1;
    };
    stage_file.close(deps.io);

    if (dl_status == 404) {
        target_dir.deleteFile(deps.io, stage_name) catch {};
        deps.stderr.print(
            messages.self_update_asset_missing_prefix ++ "{s} is not in release {s}. Install manually from " ++ releases_base_url ++ "/tag/{s}\n",
            .{ asset_name, latest_tag, latest_tag },
        ) catch {};
        return 1;
    }
    if (dl_status < 200 or dl_status >= 300) {
        target_dir.deleteFile(deps.io, stage_name) catch {};
        deps.stderr.print(messages.self_update_download_http_status_prefix ++ "{d}\n", .{dl_status}) catch {};
        return 1;
    }

    // Smoke verify: run `<stage> --version`. Pass requires exit 0
    // AND stdout containing both the tag string and the expected
    // triple as substrings (tk-32 design notes, Q7). Substring
    // containment (not exact match) keeps the check forward-compatible
    // across `--version` format changes between releases.
    const smoke_argv: []const []const u8 = &.{ stage_path, "--version" };
    var smoke = deps.runner.run(deps.gpa, .{ .argv = smoke_argv }) catch |err| {
        target_dir.deleteFile(deps.io, stage_name) catch {};
        deps.stderr.print(messages.self_update_smoke_exit_prefix ++ "spawn failed ({s})\n", .{@errorName(err)}) catch {};
        return 1;
    };
    defer smoke.deinit(deps.gpa);

    if (smoke.exit.code() != 0) {
        target_dir.deleteFile(deps.io, stage_name) catch {};
        deps.stderr.print(messages.self_update_smoke_failure_prefix ++ "{f}\n", .{smoke.exit}) catch {};
        return 1;
    }
    if (!smokeOutputContainsToken(smoke.stdout, latest_tag)) {
        target_dir.deleteFile(deps.io, stage_name) catch {};
        deps.stderr.print(messages.self_update_smoke_version_mismatch_prefix ++ "{s}\n", .{latest_tag}) catch {};
        return 1;
    }
    if (!smokeOutputContainsToken(smoke.stdout, embedded_triple)) {
        target_dir.deleteFile(deps.io, stage_name) catch {};
        deps.stderr.print(messages.self_update_smoke_triple_mismatch_prefix ++ "{s}\n", .{embedded_triple}) catch {};
        return 1;
    }

    // Commit the binary swap. POSIX uses a single atomic rename within
    // the directory; the running inode stays alive through the swap.
    // Windows uses the rename-self pattern (target → target+".old",
    // stage → target) because Windows forbids overwriting a running
    // `.exe`. The `.old` file is cleaned up at next launch by
    // `cleanupStaleExe` in main.zig — but only when the canonical
    // binary exists at the exe path, so a `.rollback_failed` outcome
    // doesn't destroy the only surviving copy.
    switch (commitInstall(deps, target_dir, stage_name, target_name, platform.is_windows)) {
        .ok => {},
        .primary_failed => |err| {
            target_dir.deleteFile(deps.io, stage_name) catch {};
            deps.stderr.print(messages.self_update_rename_failure_prefix ++ "{s}\n", .{@errorName(err)}) catch {};
            return 1;
        },
        .primary_recovered => |err| {
            target_dir.deleteFile(deps.io, stage_name) catch {};
            deps.stderr.print(
                messages.self_update_rename_failure_prefix ++ "{s}; rolled back to previous binary\n",
                .{@errorName(err)},
            ) catch {};
            return 1;
        },
        .rollback_failed => |info| {
            // Preserve the stage file for forensics. The original is at
            // <target>.old; the canonical path is empty. Surface a
            // distinct stderr line with explicit recovery instructions
            // so the user doesn't lose their working binary.
            deps.stderr.print(
                messages.self_update_rollback_failure_prefix ++
                    "primary={s}, rollback={s}; original preserved at {s}.old (restore with: mv {s}.old {s})\n",
                .{ @errorName(info.primary), @errorName(info.rollback), target_name, target_name, target_name },
            ) catch {};
            return 1;
        },
    }

    // Binary swap committed. Print success unconditionally before the
    // manpage step so the user sees what happened even when the
    // follow-up manpage subprocess fails.
    deps.stdout.print(messages.self_update_install_success_prefix ++ "{s}\n", .{latest_tag}) catch {};

    // Delegate the manpage install to the newly-installed binary so the
    // bytes from its `@embedFile` get written (matched-version docs).
    // Failure here is warn-and-continue per tk-32 design notes:
    // the binary swap stands; the user gets a clear stderr warning with
    // a retry suggestion and exit 1.
    var manpage = deps.runner.run(deps.gpa, .{
        .argv = &.{ target_path, "manpage", "--install" },
    }) catch |err| {
        deps.stderr.print(messages.self_update_manpage_failure_prefix ++ "spawn failed: {s}\n", .{@errorName(err)}) catch {};
        return 1;
    };
    defer manpage.deinit(deps.gpa);

    if (manpage.exit.code() != 0) {
        deps.stderr.print(messages.self_update_manpage_failure_prefix ++ "{f}\n", .{manpage.exit}) catch {};
        return 1;
    }

    return 0;
}

/// Outcome of `commitInstall`. POSIX collapses to either `ok` or
/// `primary_failed`. Windows can also reach `primary_recovered` (step 2
/// failed but the rollback restored the original binary) and
/// `rollback_failed` (step 2 failed AND the rollback also failed — the
/// canonical path is empty and the original survives at `<target>.old`).
/// Callers switch on this and clean up payload at the use site (no
/// asymmetric `deinit`).
const CommitOutcome = union(enum) {
    ok,
    primary_failed: anyerror,
    primary_recovered: anyerror,
    rollback_failed: struct {
        primary: anyerror,
        rollback: anyerror,
    },
};

/// Commit a staged binary into place. On POSIX, one atomic rename does
/// the job; the running process's inode stays alive across the swap. On
/// Windows (`use_windows_pattern = true`), the running `.exe` cannot be
/// overwritten directly, so a rename-self pattern is used: the current
/// target is moved aside to `<target>.old` first, then the stage is
/// renamed into place. A second-rename failure attempts to roll back
/// by renaming `.old` back to the target name; if both fail, the
/// outcome surfaces the catastrophic state and the original is
/// preserved at `<target>.old` for manual recovery.
///
/// The Windows branch is exercised on POSIX by setting
/// `use_windows_pattern = true` explicitly in tests. POSIX rename
/// semantics happen to support the rename-self steps too, so the rename
/// mechanics are testable without a Windows host. Actual Windows
/// open-handle semantics still need a Windows runner.
fn commitInstall(
    deps: cli.Deps,
    target_dir: std.Io.Dir,
    stage_name: []const u8,
    target_name: []const u8,
    use_windows_pattern: bool,
) CommitOutcome {
    if (!use_windows_pattern) {
        std.Io.Dir.rename(target_dir, stage_name, target_dir, target_name, deps.io) catch |err| {
            return .{ .primary_failed = err };
        };
        return .ok;
    }

    var old_name_buf: [std.Io.Dir.max_path_bytes]u8 = undefined;
    const old_name = std.fmt.bufPrint(&old_name_buf, "{s}.old", .{target_name}) catch {
        return .{ .primary_failed = error.NameTooLong };
    };

    // Step 1: move current target aside. A missing target file is OK —
    // first-time installs land in directories without a prior `tk.exe`,
    // and the rename simply has nothing to move.
    std.Io.Dir.rename(target_dir, target_name, target_dir, old_name, deps.io) catch |err| switch (err) {
        error.FileNotFound => {},
        else => |e| return .{ .primary_failed = e },
    };

    // Step 2: place new bytes at the target name. On failure, undo
    // step 1 so the user is left with a working binary at the original
    // path rather than a missing one. If the rollback also fails, the
    // outcome surfaces the catastrophic state — the original is
    // preserved at `<target>.old` and the user must restore manually.
    std.Io.Dir.rename(target_dir, stage_name, target_dir, target_name, deps.io) catch |err| {
        std.Io.Dir.rename(target_dir, old_name, target_dir, target_name, deps.io) catch |rb_err| {
            return .{ .rollback_failed = .{ .primary = err, .rollback = rb_err } };
        };
        return .{ .primary_recovered = err };
    };

    return .ok;
}

/// Best-effort cleanup of a stale `<exe-dir>/tk.exe.old` left behind by a
/// previous Windows self-update. Called early from `main.zig` under
/// `platform.is_windows`. Failure is silent because the file may not
/// exist, may be held open by another process, or the user may lack
/// permission — none of those should block a normal `tk` run. Mirrors
/// `tk.exe.old` literal name used by `commitInstall`.
pub fn cleanupStaleExe(io: std.Io) void {
    var exe_buf: [std.Io.Dir.max_path_bytes]u8 = undefined;
    const exe_len = std.process.executablePath(io, &exe_buf) catch return;
    cleanupStaleExeAt(io, exe_buf[0..exe_len]);
}

/// Variant of `cleanupStaleExe` that takes the exe path explicitly so
/// tests can run against a tmpdir without `std.process.executablePath`.
///
/// The literal `tk.exe.old` matches the file `commitInstall` produces
/// when the canonical Windows binary is `tk.exe`. If a user renames the
/// shipped binary (rare; not part of the supported install paths),
/// `commitInstall` will produce `<their-name>.old` and this cleanup
/// helper will not find it. That mismatch is intentional — the literal
/// `tk.exe.old` is the cross-launch contract.
///
/// Safety: only delete the `.old` sidecar when the canonical binary
/// exists at `exe_path`. Otherwise we may be removing the user's only
/// recoverable copy after a `commitInstall` `.rollback_failed` outcome
/// where step 2 failed and the rollback couldn't restore the original.
fn cleanupStaleExeAt(io: std.Io, exe_path: []const u8) void {
    const exe_dir = std.fs.path.dirname(exe_path) orelse return;
    var path_buf: [std.Io.Dir.max_path_bytes]u8 = undefined;
    const old_path = std.fmt.bufPrint(
        &path_buf,
        "{s}{c}tk.exe.old",
        .{ exe_dir, std.fs.path.sep },
    ) catch return;
    std.Io.Dir.cwd().access(io, exe_path, .{}) catch return;
    std.Io.Dir.cwd().deleteFile(io, old_path) catch {};
}

/// Construct the asset basename for the running triple. `.exe` is
/// appended for windows-gnu triples so the manual download URL matches
/// what GitHub serves and the Windows CreateProcessW path finds the
/// staged file by extension.
fn buildAssetName(gpa: Allocator, triple: []const u8) ![]u8 {
    const ext: []const u8 = if (isWindowsTriple(triple)) ".exe" else "";
    return std.fmt.allocPrint(gpa, "tk-{s}{s}", .{ triple, ext });
}

/// Match any Windows ABI suffix — `-windows-gnu`, `-windows-msvc`, etc. —
/// rather than the original literal `-windows-gnu`, so future Windows
/// triples that the release-targets table grows pick up `.exe` without
/// silently 404ing on the download.
fn isWindowsTriple(triple: []const u8) bool {
    return std.mem.indexOf(u8, triple, "-windows-") != null;
}

/// Token-anchored substring check for smoke verification: returns true
/// only if `token` appears as a whole word in `text`, with whitespace or
/// `()` as separators on both sides. Empty `token` returns false (the
/// indexOf-empty-needle bypass). Defends against prefix collisions like
/// `latest_tag = "v0.6.0"` matching `"v0.6.0-rc1"` in the staged
/// binary's `--version` output.
fn smokeOutputContainsToken(text: []const u8, token: []const u8) bool {
    if (token.len == 0) return false;
    var it = std.mem.tokenizeAny(u8, text, " \t\r\n()");
    while (it.next()) |tok| {
        if (std.mem.eql(u8, tok, token)) return true;
    }
    return false;
}

/// Build the staged binary's filename: `.tk.tmp.<8-byte-hex>`. Hex suffix
/// uses 64 random bits from `deps.random` so concurrent self-updates
/// against the same exe directory cannot collide on a stage path.
pub fn renderStageName(buf: *[stage_name_buf_size]u8, random: std.Random) []const u8 {
    var rand_bytes: [8]u8 = undefined;
    random.bytes(&rand_bytes);
    const hex = std.fmt.bytesToHex(rand_bytes, .lower);
    return std.fmt.bufPrint(buf, stage_name_prefix ++ "{s}", .{hex[0..]}) catch unreachable;
}

fn renderVersionParseFailure(
    deps: cli.Deps,
    err: VersionParseError,
    embedded_version: []const u8,
    latest_tag: []const u8,
) void {
    switch (err) {
        error.EmbeddedVersionUnparseable => {
            deps.stderr.print(
                messages.self_update_embedded_unparseable_prefix ++ "{s}\n",
                .{embedded_version},
            ) catch {};
        },
        error.LatestTagUnparseable => {
            deps.stderr.print(
                messages.self_update_latest_unparseable_prefix ++ "{s}\n",
                .{latest_tag},
            ) catch {};
        },
    }
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk self-update - replace the running tk binary with the latest release
        \\
        \\Queries the GitHub Releases API for the latest tag, compares against
        \\the embedded build version, and replaces the running binary if
        \\newer. Refuses to run on development builds.
        \\
        \\With --check, queries the API and prints whether an update is
        \\available without downloading. By query-subcommand convention,
        \\exits 0 when already on the latest release and 1 when a newer
        \\release is available; real query failures also exit 1 with the
        \\reason on stderr.
        \\
        \\Usage:
        \\  tk self-update [--check]
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

// Dev-refusal tests call `runWith` with an explicit "dev" triple so they
// exercise the refusal branch regardless of whatever build_options.triple
// has been set to by `-Drelease-triple=...` at `zig build test` time.

test "self-update: dev build refuses without flags" {
    var h = Harness.init(std.testing.allocator, &.{}, .{});
    defer h.deinit();
    const code = try runWith(h.deps(), &h.iter, "v0.0.1", dev_triple);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_dev_build) != null);
    try std.testing.expectEqualStrings("", h.stdout());
}

test "self-update: dev build refuses --check the same way" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    const code = try runWith(h.deps(), &h.iter, "v0.0.1", dev_triple);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_dev_build) != null);
    try std.testing.expectEqualStrings("", h.stdout());
}

test "self-update --check: up-to-date exits 0 with diagnostic" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{
        .status = 200,
        .body = "{\"tag_name\":\"v0.5.0\"}",
    });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.self_update_check_up_to_date_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "v0.5.0") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "self-update --check: newer available exits 1 with diagnostic" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{
        .status = 200,
        .body = "{\"tag_name\":\"v0.6.0\"}",
    });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.self_update_check_newer_available_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "v0.6.0") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "v0.5.0") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "self-update --check: local build ahead exits 0 with diagnostic" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{
        .status = 200,
        .body = "{\"tag_name\":\"v0.4.0\"}",
    });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.self_update_check_ahead_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "v0.5.0") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "v0.4.0") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "self-update --check: ignores unknown JSON fields" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{
        .status = 200,
        .body =
        \\{"tag_name":"v0.5.0","name":"Release v0.5.0","prerelease":false,"assets":[{"name":"tk-x86_64-linux-musl"}]}
        ,
    });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.self_update_check_up_to_date_prefix) != null);
}

test "self-update --check: HTTP 5xx surfaces status code" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 503, .body = "" });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_query_http_status_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "503") != null);
    try std.testing.expectEqualStrings("", h.stdout());
}

test "self-update --check: network error surfaces transport diagnostic" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .err = error.NetworkError });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_query_network) != null);
    try std.testing.expectEqualStrings("", h.stdout());
}

test "self-update --check: malformed JSON" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 200, .body = "{not valid json" });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_query_malformed) != null);
}

test "self-update --check: missing tag_name field" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 200, .body = "{\"tag_name\":\"\"}" });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_query_missing_tag) != null);
}

test "self-update --check: unparseable latest tag surfaces tag" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 200, .body = "{\"tag_name\":\"not-semver\"}" });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_latest_unparseable_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "not-semver") != null);
}

test "self-update --check: unparseable embedded version surfaces version" {
    var h = Harness.init(std.testing.allocator, &.{"--check"}, .{});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 200, .body = "{\"tag_name\":\"v0.5.0\"}" });
    const code = try runWith(h.deps(), &h.iter, "not-semver", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_embedded_unparseable_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "not-semver") != null);
}

// -- Slice E: POSIX full update tests --------------------------------------
//
// These exercise `performUpdate` directly with a tmpdir as the target
// directory. The Harness's PRNG (`DefaultPrng.init(0)`) is mirrored at
// the test side so we can predict the staged filename and register the
// matching FakeRunner argv_prefix for the smoke subprocess. Each test
// derives the same `expected_stage_name` from a freshly seeded PRNG.

/// Mirror the harness's `DefaultPrng.init(0)` to compute the stage name
/// `performUpdate` will produce. Tests use this to register the
/// FakeRunner expectation against the predicted stage path.
fn predictStageName(buf: *[stage_name_buf_size]u8) []const u8 {
    var prng = std.Random.DefaultPrng.init(0);
    return renderStageName(buf, prng.random());
}

/// Helper that wires the asset URL string from a triple + tag the same
/// way `performUpdate` does, so tests don't drift if `releases_base_url`
/// or the asset-name layout ever changes.
fn predictAssetUrl(gpa: Allocator, triple: []const u8) ![]u8 {
    const ext: []const u8 = if (isWindowsTriple(triple)) ".exe" else "";
    return std.fmt.allocPrint(
        gpa,
        releases_base_url ++ "/latest/download/tk-{s}{s}",
        .{ triple, ext },
    );
}

test "self-update full: POSIX happy path streams asset into target" {
    try platform.skipOnWindows();
    const gpa = std.testing.allocator;

    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const target_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
    defer gpa.free(target_dir_path);

    var stage_name_buf: [stage_name_buf_size]u8 = undefined;
    const expected_stage_name = predictStageName(&stage_name_buf);
    const expected_stage_path = try std.fs.path.join(gpa, &.{ target_dir_path, expected_stage_name });
    defer gpa.free(expected_stage_path);

    const asset_url = try predictAssetUrl(gpa, "x86_64-linux-musl");
    defer gpa.free(asset_url);

    const expected_target_path = try std.fs.path.join(gpa, &.{ target_dir_path, "tk" });
    defer gpa.free(expected_target_path);

    const new_binary_bytes = "fake-new-tk-binary-bytes-v0.6.0";
    try h.fake_http.expect(asset_url, .{ .status = 200, .body = new_binary_bytes });
    try h.fake_runner.expect(
        &.{ expected_stage_path, "--version" },
        .{ .exit_code = 0, .stdout = "v0.6.0 (x86_64-linux-musl)\n" },
    );
    try h.fake_runner.expect(
        &.{ expected_target_path, "manpage", "--install" },
        .{ .exit_code = 0, .stdout = "Installed manpage at /irrelevant/path\n" },
    );

    const code = try performUpdate(h.deps(), target_dir_path, "tk", "x86_64-linux-musl", "v0.6.0");
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.self_update_install_success_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "v0.6.0") != null);
    try std.testing.expectEqualStrings("", h.stderr());

    // Stage file was renamed into place; the target now holds the
    // downloaded bytes and no stage file is left behind.
    const installed = try tmp.dir.readFileAlloc(std.testing.io, "tk", gpa, .unlimited);
    defer gpa.free(installed);
    try std.testing.expectEqualStrings(new_binary_bytes, installed);
    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, expected_stage_name, .{}));
}

test "self-update full: manpage subprocess failure warns but preserves binary swap" {
    try platform.skipOnWindows();
    const gpa = std.testing.allocator;

    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const target_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
    defer gpa.free(target_dir_path);

    var stage_name_buf: [stage_name_buf_size]u8 = undefined;
    const expected_stage_name = predictStageName(&stage_name_buf);
    const expected_stage_path = try std.fs.path.join(gpa, &.{ target_dir_path, expected_stage_name });
    defer gpa.free(expected_stage_path);
    const expected_target_path = try std.fs.path.join(gpa, &.{ target_dir_path, "tk" });
    defer gpa.free(expected_target_path);

    const asset_url = try predictAssetUrl(gpa, "x86_64-linux-musl");
    defer gpa.free(asset_url);

    const new_binary_bytes = "fake-new-tk-binary-bytes-v0.6.0";
    try h.fake_http.expect(asset_url, .{ .status = 200, .body = new_binary_bytes });
    try h.fake_runner.expect(
        &.{ expected_stage_path, "--version" },
        .{ .exit_code = 0, .stdout = "v0.6.0 (x86_64-linux-musl)\n" },
    );
    try h.fake_runner.expect(
        &.{ expected_target_path, "manpage", "--install" },
        .{ .exit_code = 1, .stderr = "tk manpage: install failed at /some/path: ...\n" },
    );

    const code = try performUpdate(h.deps(), target_dir_path, "tk", "x86_64-linux-musl", "v0.6.0");
    try std.testing.expectEqual(@as(u8, 1), code);

    // Binary swap stood: success line on stdout, target file holds the
    // new bytes.
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.self_update_install_success_prefix) != null);
    const installed = try tmp.dir.readFileAlloc(std.testing.io, "tk", gpa, .unlimited);
    defer gpa.free(installed);
    try std.testing.expectEqualStrings(new_binary_bytes, installed);

    // Manpage-failure warning surfaced on stderr with the retry hint.
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_manpage_failure_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "exit 1") != null);
}

test "self-update full: AccessDenied at stage create fast-fails before download" {
    try platform.skipOnWindows();
    // Root bypasses DAC permission checks (CAP_DAC_OVERRIDE), so a
    // 0o555 chmod on the staging dir won't actually block createFile.
    // Skip rather than report a false pass: the assertion below would
    // panic on the unmatched URL via FakeHttpClient's strict mode.
    if (std.c.geteuid() == 0) return error.SkipZigTest;
    const gpa = std.testing.allocator;

    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    // Use a subdirectory so cleanup can restore writable perms before
    // `tmp.cleanup()` runs and we don't deadlock the outer dir.
    try tmp.dir.createDirPath(std.testing.io, "ro");
    var ro_dir = try tmp.dir.openDir(std.testing.io, "ro", .{});
    defer ro_dir.close(std.testing.io);

    const ro_path = try tmp.dir.realPathFileAlloc(std.testing.io, "ro", gpa);
    defer gpa.free(ro_path);

    // Make the subdir read-only. `defer` runs LIFO so the chmod-back
    // executes before `tmp.cleanup()` and the tree can be torn down.
    try tmp.dir.setFilePermissions(
        std.testing.io,
        "ro",
        std.Io.File.Permissions.fromMode(0o555),
        .{},
    );
    defer tmp.dir.setFilePermissions(
        std.testing.io,
        "ro",
        std.Io.File.Permissions.fromMode(0o755),
        .{},
    ) catch {};

    // No HTTP expectation registered — the test verifies that the
    // AccessDenied path fast-fails before any network traffic. An
    // unexpected http call would panic via FakeHttpClient's strict
    // mode.
    const code = try performUpdate(h.deps(), ro_path, "tk", "x86_64-linux-musl", "v0.6.0");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_no_write_access_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "permission denied") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), ro_path) != null);
    try std.testing.expectEqualStrings("", h.stdout());
}

test "self-update full: asset 404 renders unified missing-asset diagnostic" {
    try platform.skipOnWindows();
    const gpa = std.testing.allocator;

    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const target_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
    defer gpa.free(target_dir_path);

    const asset_url = try predictAssetUrl(gpa, "aarch64-windows-gnu");
    defer gpa.free(asset_url);
    try h.fake_http.expect(asset_url, .{ .status = 404, .body = "" });

    const code = try performUpdate(h.deps(), target_dir_path, "tk", "aarch64-windows-gnu", "v0.6.0");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "tk-aarch64-windows-gnu.exe is not in release v0.6.0") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), releases_base_url ++ "/tag/v0.6.0") != null);

    // Stage file was cleaned up; the target was never created.
    var stage_name_buf: [stage_name_buf_size]u8 = undefined;
    const expected_stage_name = predictStageName(&stage_name_buf);
    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, expected_stage_name, .{}));
    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, "tk", .{}));
}

test "self-update full: asset HTTP 5xx surfaces status code" {
    try platform.skipOnWindows();
    const gpa = std.testing.allocator;

    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const target_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
    defer gpa.free(target_dir_path);

    const asset_url = try predictAssetUrl(gpa, "x86_64-linux-musl");
    defer gpa.free(asset_url);
    try h.fake_http.expect(asset_url, .{ .status = 503, .body = "" });

    const code = try performUpdate(h.deps(), target_dir_path, "tk", "x86_64-linux-musl", "v0.6.0");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_download_http_status_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "503") != null);
}

test "self-update full: download network error cleans up stage" {
    try platform.skipOnWindows();
    const gpa = std.testing.allocator;

    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const target_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
    defer gpa.free(target_dir_path);

    const asset_url = try predictAssetUrl(gpa, "x86_64-linux-musl");
    defer gpa.free(asset_url);
    try h.fake_http.expect(asset_url, .{ .err = error.NetworkError });

    const code = try performUpdate(h.deps(), target_dir_path, "tk", "x86_64-linux-musl", "v0.6.0");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_download_network) != null);

    var stage_name_buf: [stage_name_buf_size]u8 = undefined;
    const expected_stage_name = predictStageName(&stage_name_buf);
    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, expected_stage_name, .{}));
}

test "self-update full: smoke exit-nonzero leaves target untouched" {
    try platform.skipOnWindows();
    const gpa = std.testing.allocator;

    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const target_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
    defer gpa.free(target_dir_path);

    var stage_name_buf: [stage_name_buf_size]u8 = undefined;
    const expected_stage_name = predictStageName(&stage_name_buf);
    const expected_stage_path = try std.fs.path.join(gpa, &.{ target_dir_path, expected_stage_name });
    defer gpa.free(expected_stage_path);

    const asset_url = try predictAssetUrl(gpa, "x86_64-linux-musl");
    defer gpa.free(asset_url);
    try h.fake_http.expect(asset_url, .{ .status = 200, .body = "junk-bytes" });
    try h.fake_runner.expect(
        &.{ expected_stage_path, "--version" },
        .{ .exit_code = 7, .stdout = "" },
    );

    const code = try performUpdate(h.deps(), target_dir_path, "tk", "x86_64-linux-musl", "v0.6.0");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_smoke_exit_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "7") != null);

    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, expected_stage_name, .{}));
    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, "tk", .{}));
}

test "self-update full: smoke version-mismatch leaves target untouched" {
    try platform.skipOnWindows();
    const gpa = std.testing.allocator;

    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const target_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
    defer gpa.free(target_dir_path);

    var stage_name_buf: [stage_name_buf_size]u8 = undefined;
    const expected_stage_name = predictStageName(&stage_name_buf);
    const expected_stage_path = try std.fs.path.join(gpa, &.{ target_dir_path, expected_stage_name });
    defer gpa.free(expected_stage_path);

    const asset_url = try predictAssetUrl(gpa, "x86_64-linux-musl");
    defer gpa.free(asset_url);
    try h.fake_http.expect(asset_url, .{ .status = 200, .body = "bytes" });
    try h.fake_runner.expect(
        &.{ expected_stage_path, "--version" },
        .{ .exit_code = 0, .stdout = "v9.9.9 (x86_64-linux-musl)\n" },
    );

    const code = try performUpdate(h.deps(), target_dir_path, "tk", "x86_64-linux-musl", "v0.6.0");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_smoke_version_mismatch_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "v0.6.0") != null);

    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, "tk", .{}));
}

test "self-update full: smoke triple-mismatch leaves target untouched" {
    try platform.skipOnWindows();
    const gpa = std.testing.allocator;

    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const target_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
    defer gpa.free(target_dir_path);

    var stage_name_buf: [stage_name_buf_size]u8 = undefined;
    const expected_stage_name = predictStageName(&stage_name_buf);
    const expected_stage_path = try std.fs.path.join(gpa, &.{ target_dir_path, expected_stage_name });
    defer gpa.free(expected_stage_path);

    const asset_url = try predictAssetUrl(gpa, "x86_64-linux-musl");
    defer gpa.free(asset_url);
    try h.fake_http.expect(asset_url, .{ .status = 200, .body = "bytes" });
    try h.fake_runner.expect(
        &.{ expected_stage_path, "--version" },
        .{ .exit_code = 0, .stdout = "v0.6.0 (aarch64-macos)\n" },
    );

    const code = try performUpdate(h.deps(), target_dir_path, "tk", "x86_64-linux-musl", "v0.6.0");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_smoke_triple_mismatch_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "x86_64-linux-musl") != null);

    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, "tk", .{}));
}

test "buildAssetName: appends .exe for any -windows- ABI" {
    const gpa = std.testing.allocator;
    const cases = [_]struct { triple: []const u8, expected: []const u8 }{
        .{ .triple = "x86_64-linux-musl", .expected = "tk-x86_64-linux-musl" },
        .{ .triple = "aarch64-linux-musl", .expected = "tk-aarch64-linux-musl" },
        .{ .triple = "x86_64-linux-gnu", .expected = "tk-x86_64-linux-gnu" },
        .{ .triple = "aarch64-macos", .expected = "tk-aarch64-macos" },
        .{ .triple = "x86_64-windows-gnu", .expected = "tk-x86_64-windows-gnu.exe" },
        .{ .triple = "aarch64-windows-gnu", .expected = "tk-aarch64-windows-gnu.exe" },
        // Future-proofing: any other -windows-* ABI gets `.exe` too.
        .{ .triple = "x86_64-windows-msvc", .expected = "tk-x86_64-windows-msvc.exe" },
        .{ .triple = "aarch64-windows-msvc", .expected = "tk-aarch64-windows-msvc.exe" },
    };
    for (cases) |c| {
        const got = try buildAssetName(gpa, c.triple);
        defer gpa.free(got);
        try std.testing.expectEqualStrings(c.expected, got);
    }
}

test "commitInstall: POSIX pattern atomically replaces target" {
    const gpa = std.testing.allocator;
    _ = gpa;

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = ".tk.tmp.aaaa", .data = "new-bytes" });
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "tk", .data = "old-bytes" });

    var h = Harness.init(std.testing.allocator, &.{}, .{});
    defer h.deinit();

    try std.testing.expectEqual(CommitOutcome.ok, commitInstall(h.deps(), tmp.dir, ".tk.tmp.aaaa", "tk", false));

    const installed = try tmp.dir.readFileAlloc(std.testing.io, "tk", std.testing.allocator, .unlimited);
    defer std.testing.allocator.free(installed);
    try std.testing.expectEqualStrings("new-bytes", installed);
    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, ".tk.tmp.aaaa", .{}));
    // POSIX pattern leaves no `.old` sidecar.
    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, "tk.old", .{}));
}

test "commitInstall: Windows pattern moves current to .old then places stage" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = ".tk.tmp.bbbb", .data = "new-bytes" });
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "tk.exe", .data = "old-bytes" });

    var h = Harness.init(std.testing.allocator, &.{}, .{});
    defer h.deinit();

    try std.testing.expectEqual(CommitOutcome.ok, commitInstall(h.deps(), tmp.dir, ".tk.tmp.bbbb", "tk.exe", true));

    // New bytes at target, old bytes preserved as `.old` sidecar for
    // the next-launch cleanup hook to delete.
    const installed = try tmp.dir.readFileAlloc(std.testing.io, "tk.exe", std.testing.allocator, .unlimited);
    defer std.testing.allocator.free(installed);
    try std.testing.expectEqualStrings("new-bytes", installed);

    const sidecar = try tmp.dir.readFileAlloc(std.testing.io, "tk.exe.old", std.testing.allocator, .unlimited);
    defer std.testing.allocator.free(sidecar);
    try std.testing.expectEqualStrings("old-bytes", sidecar);

    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, ".tk.tmp.bbbb", .{}));
}

test "commitInstall: Windows pattern tolerates absent target (first install)" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = ".tk.tmp.cccc", .data = "first-bytes" });
    // No existing tk.exe at all.

    var h = Harness.init(std.testing.allocator, &.{}, .{});
    defer h.deinit();

    try std.testing.expectEqual(CommitOutcome.ok, commitInstall(h.deps(), tmp.dir, ".tk.tmp.cccc", "tk.exe", true));

    const installed = try tmp.dir.readFileAlloc(std.testing.io, "tk.exe", std.testing.allocator, .unlimited);
    defer std.testing.allocator.free(installed);
    try std.testing.expectEqualStrings("first-bytes", installed);
    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, "tk.exe.old", .{}));
}

test "cleanupStaleExeAt: deletes a stale .old file next to the exe" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "tk.exe.old", .data = "stale" });
    // Canonical binary must exist for cleanup to delete the sidecar
    // — see the safety net in cleanupStaleExeAt.
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "tk.exe", .data = "current" });
    const exe_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(exe_dir_path);
    const exe_path = try std.fs.path.join(std.testing.allocator, &.{ exe_dir_path, "tk.exe" });
    defer std.testing.allocator.free(exe_path);

    cleanupStaleExeAt(std.testing.io, exe_path);

    try std.testing.expectError(error.FileNotFound, tmp.dir.access(std.testing.io, "tk.exe.old", .{}));
}

test "cleanupStaleExeAt: preserves .old when canonical binary is missing" {
    // Defends against the catastrophic `rollback_failed` outcome in
    // commitInstall: tk.exe is gone, original survives at tk.exe.old.
    // Cleanup must NOT delete the only recoverable copy.
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "tk.exe.old", .data = "the-only-copy" });
    // No tk.exe at all.

    const exe_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(exe_dir_path);
    const exe_path = try std.fs.path.join(std.testing.allocator, &.{ exe_dir_path, "tk.exe" });
    defer std.testing.allocator.free(exe_path);

    cleanupStaleExeAt(std.testing.io, exe_path);

    const preserved = try tmp.dir.readFileAlloc(std.testing.io, "tk.exe.old", std.testing.allocator, .unlimited);
    defer std.testing.allocator.free(preserved);
    try std.testing.expectEqualStrings("the-only-copy", preserved);
}

test "cleanupStaleExeAt: no-op when .old file is absent" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const exe_dir_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(exe_dir_path);
    const exe_path = try std.fs.path.join(std.testing.allocator, &.{ exe_dir_path, "tk.exe" });
    defer std.testing.allocator.free(exe_path);

    // Should not panic, throw, or leave any side effect.
    cleanupStaleExeAt(std.testing.io, exe_path);
}

test "renderStageName: prefix + 16 lowercase hex chars" {
    var prng = std.Random.DefaultPrng.init(42);
    var buf: [stage_name_buf_size]u8 = undefined;
    const name = renderStageName(&buf, prng.random());
    try std.testing.expect(std.mem.startsWith(u8, name, stage_name_prefix));
    try std.testing.expectEqual(stage_name_prefix.len + 16, name.len);
    for (name[stage_name_prefix.len..]) |b| {
        try std.testing.expect((b >= '0' and b <= '9') or (b >= 'a' and b <= 'f'));
    }
}
