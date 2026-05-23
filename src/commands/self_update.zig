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
            if (check_only) return renderCheckResult(deps, cmp, embedded_version, tag);

            // Slice E and beyond: stage / download / smoke / rename.
            deps.stderr.writeAll("tk self-update: full update flow not yet implemented\n") catch {};
            return 1;
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
};

/// Minimal JSON shape parsed out of the API response. `ignore_unknown_fields`
/// keeps us forward-compatible with new fields GitHub may add.
const ReleaseJson = struct { tag_name: []const u8 };

fn fetchLatestTag(deps: cli.Deps) FetchTagOutcome {
    var resp = deps.http.getJson(deps.gpa, api_url) catch |err| return switch (err) {
        error.NetworkError => .network_error,
        error.TlsError => .tls_error,
        error.MalformedResponse => .malformed_json,
        error.WriteFailed, error.OutOfMemory => .network_error,
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

    if (parsed.value.tag_name.len == 0) return .missing_tag_field;

    const owned = deps.gpa.dupe(u8, parsed.value.tag_name) catch return .network_error;
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

test "self-update: dev build refuses without flags" {
    var h = Harness.init(std.testing.allocator, &.{});
    defer h.deinit();
    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_dev_build) != null);
    try std.testing.expectEqualStrings("", h.stdout());
}

test "self-update: dev build refuses --check the same way" {
    var h = Harness.init(std.testing.allocator, &.{"--check"});
    defer h.deinit();
    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_dev_build) != null);
    try std.testing.expectEqualStrings("", h.stdout());
}

test "self-update --check: up-to-date exits 0 with diagnostic" {
    var h = Harness.init(std.testing.allocator, &.{"--check"});
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
    var h = Harness.init(std.testing.allocator, &.{"--check"});
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
    var h = Harness.init(std.testing.allocator, &.{"--check"});
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
    var h = Harness.init(std.testing.allocator, &.{"--check"});
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
    var h = Harness.init(std.testing.allocator, &.{"--check"});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 503, .body = "" });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_query_http_status_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "503") != null);
    try std.testing.expectEqualStrings("", h.stdout());
}

test "self-update --check: network error surfaces transport diagnostic" {
    var h = Harness.init(std.testing.allocator, &.{"--check"});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .err = error.NetworkError });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_query_network) != null);
    try std.testing.expectEqualStrings("", h.stdout());
}

test "self-update --check: malformed JSON" {
    var h = Harness.init(std.testing.allocator, &.{"--check"});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 200, .body = "{not valid json" });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_query_malformed) != null);
}

test "self-update --check: missing tag_name field" {
    var h = Harness.init(std.testing.allocator, &.{"--check"});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 200, .body = "{\"tag_name\":\"\"}" });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_query_missing_tag) != null);
}

test "self-update --check: unparseable latest tag surfaces tag" {
    var h = Harness.init(std.testing.allocator, &.{"--check"});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 200, .body = "{\"tag_name\":\"not-semver\"}" });
    const code = try runWith(h.deps(), &h.iter, "v0.5.0", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_latest_unparseable_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "not-semver") != null);
}

test "self-update --check: unparseable embedded version surfaces version" {
    var h = Harness.init(std.testing.allocator, &.{"--check"});
    defer h.deinit();
    try h.fake_http.expect(api_url, .{ .status = 200, .body = "{\"tag_name\":\"v0.5.0\"}" });
    const code = try runWith(h.deps(), &h.iter, "not-semver", "x86_64-linux-musl");
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.self_update_embedded_unparseable_prefix) != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "not-semver") != null);
}
