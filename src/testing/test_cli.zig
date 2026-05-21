const std = @import("std");
const Allocator = std.mem.Allocator;

const cli = @import("../cli.zig");
const fake_proc = @import("../proc/fake.zig");
const clock_mod = @import("../clock.zig");
const render = @import("../render/styler.zig");
const SliceArgIter = @import("arg_iter.zig").SliceArgIter;

/// Default fixed instant used by `Harness` so timestamps in command output stay
/// stable. `2026-05-09T12:34:56.789Z` matches one of the doctests in
/// `clock.zig` — pick anything; it just needs to be deterministic.
pub const default_fake_now_ms: i64 = 1778330096789;

/// In-process command-handler harness.
///
/// `Harness.init` synthesizes a strict fake runner that fails any subprocess
/// call. Tests that exercise commands invoking subprocesses must register
/// expectations on `harness.fake_runner` before calling the command.
pub const Harness = struct {
    /// Captured command stdout.
    stdout_buf: std.Io.Writer.Allocating,
    /// Captured command stderr.
    stderr_buf: std.Io.Writer.Allocating,
    /// Fixed stdin supplied to commands that read `deps.stdin`.
    stdin_reader: std.Io.Reader,
    /// Slice-backed iterator over command args.
    iter: SliceArgIter,
    /// Allocator threaded into `cli.Deps`.
    gpa: Allocator,
    /// Strict fake subprocess runner used by default.
    fake_runner: fake_proc.FakeRunner,
    /// Deterministic clock used by default.
    fake_clock: clock_mod.FakeClock,
    /// Deterministic PRNG used for opaque internal IDs in command tests.
    prng: std.Random.DefaultPrng,
    /// Optional cwd override for commands that resolve paths.
    cwd_override: ?std.Io.Dir,
    /// Per-stream color mode propagated into `deps.styler`. Defaults to
    /// `.no_color`, matching non-TTY test capture.
    stdout_mode: render.Mode,
    stderr_mode: render.Mode,

    /// Optional harness overrides.
    pub const Options = struct {
        /// Overrides `deps.cwd`. Slice 3+ commands resolve paths relative to
        /// `deps.cwd`; tests that exercise that resolution must pass an
        /// explicit value rather than letting the test runner's cwd leak in.
        /// Slice 2 callers may leave this null and get `std.Io.Dir.cwd()`.
        cwd: ?std.Io.Dir = null,
        /// Bytes exposed through `deps.stdin`.
        stdin: []const u8 = "",
        /// Per-stream color mode for `deps.styler`. Tests that exercise the
        /// styled path set one or both to `.escape_codes`.
        stdout_mode: render.Mode = .no_color,
        stderr_mode: render.Mode = .no_color,
    };

    /// Create a harness with default options.
    pub fn init(allocator: Allocator, args: []const []const u8) Harness {
        return initWith(allocator, args, .{});
    }

    /// Create a harness with an optional cwd override.
    pub fn initWith(allocator: Allocator, args: []const []const u8, opts: Options) Harness {
        return .{
            .stdout_buf = .init(allocator),
            .stderr_buf = .init(allocator),
            .stdin_reader = .fixed(opts.stdin),
            .iter = .{ .items = args },
            .gpa = allocator,
            .fake_runner = fake_proc.FakeRunner.init(allocator),
            .fake_clock = clock_mod.FakeClock.init(default_fake_now_ms),
            .prng = std.Random.DefaultPrng.init(0),
            .cwd_override = opts.cwd,
            .stdout_mode = opts.stdout_mode,
            .stderr_mode = opts.stderr_mode,
        };
    }

    /// Free captured output buffers and fake-runner expectations.
    pub fn deinit(self: *Harness) void {
        self.stdout_buf.deinit();
        self.stderr_buf.deinit();
        self.fake_runner.deinit();
    }

    /// Build the `cli.Deps` value to pass into a command under test.
    pub fn deps(self: *Harness) cli.Deps {
        return .{
            .stdout = &self.stdout_buf.writer,
            .stderr = &self.stderr_buf.writer,
            .stdin = &self.stdin_reader,
            .gpa = self.gpa,
            .io = std.testing.io,
            .cwd = self.cwd_override orelse std.Io.Dir.cwd(),
            .runner = self.fake_runner.runner(),
            .clock = self.fake_clock.clock(),
            .random = self.prng.random(),
            .styler = .{ .stdout = self.stdout_mode, .stderr = self.stderr_mode },
        };
    }

    /// Captured stdout bytes written by the command.
    pub fn stdout(self: *Harness) []const u8 {
        return self.stdout_buf.written();
    }

    /// Captured stderr bytes written by the command.
    pub fn stderr(self: *Harness) []const u8 {
        return self.stderr_buf.written();
    }
};
