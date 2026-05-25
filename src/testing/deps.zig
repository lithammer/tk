const std = @import("std");
const Allocator = std.mem.Allocator;

const cli = @import("../cli.zig");
const clock_mod = @import("../clock.zig");
const http_mod = @import("../http/client.zig");
const proc = @import("../proc/runner.zig");
const render = @import("../render/styler.zig");

/// Default fixed instant used by command tests so Repository Store timestamps
/// stay stable. `2026-05-09T12:34:56.789Z` matches one of the doctests in
/// `clock.zig`; the exact instant is unimportant as long as it is
/// deterministic.
pub const default_fake_now_ms: i64 = 1778330096789;

/// Test-side inputs for constructing `cli.Deps`.
///
/// Callers own all lifetimed resources and fake services. This helper only
/// assembles the borrowed handles into the production command dependency
/// contract, keeping fake-vs-real choices explicit at each harness boundary.
pub const Options = struct {
    stdout: *std.Io.Writer,
    stderr: *std.Io.Writer,
    stdin: *std.Io.Reader,
    gpa: Allocator,
    io: ?std.Io = null,
    cwd: std.Io.Dir,
    runner: proc.Runner,
    http: http_mod.Http,
    clock: clock_mod.Clock,
    random: std.Random,
    stdout_mode: render.Mode = .no_color,
    stderr_mode: render.Mode = .no_color,
};

/// Assemble a `cli.Deps` value for command tests.
pub fn init(options: Options) cli.Deps {
    return .{
        .stdout = options.stdout,
        .stderr = options.stderr,
        .stdin = options.stdin,
        .gpa = options.gpa,
        .io = options.io orelse std.testing.io,
        .cwd = options.cwd,
        .runner = options.runner,
        .http = options.http,
        .clock = options.clock,
        .random = options.random,
        .styler = .{
            .stdout = options.stdout_mode,
            .stderr = options.stderr_mode,
        },
    };
}

test "init defaults to testing io and no-color stream modes" {
    const gpa = std.testing.allocator;

    var stdout_buf: std.Io.Writer.Allocating = .init(gpa);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(gpa);
    defer stderr_buf.deinit();
    var stdin_reader: std.Io.Reader = .fixed("");

    var fake_runner = @import("../proc/fake.zig").FakeRunner.init(gpa);
    defer fake_runner.deinit();
    var fake_http = @import("../http/fake.zig").FakeHttpClient.init(gpa);
    defer fake_http.deinit();
    var fake_clock = clock_mod.FakeClock.init(default_fake_now_ms);
    var prng = std.Random.DefaultPrng.init(0);

    const deps = init(.{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .stdin = &stdin_reader,
        .gpa = gpa,
        .cwd = std.Io.Dir.cwd(),
        .runner = fake_runner.runner(),
        .http = fake_http.http(),
        .clock = fake_clock.clock(),
        .random = prng.random(),
    });

    try std.testing.expect(deps.io.vtable == std.testing.io.vtable);
    try std.testing.expect(deps.io.userdata == std.testing.io.userdata);
    try std.testing.expect(deps.styler.stdout == .no_color);
    try std.testing.expect(deps.styler.stderr == .no_color);
}
