const std = @import("std");
const cli = @import("../cli.zig");
const fake_proc = @import("../proc/fake.zig");
const clock_mod = @import("../clock.zig");
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
    /// Slice-backed iterator over command args.
    iter: SliceArgIter,
    /// Allocator threaded into `cli.Deps`.
    gpa: std.mem.Allocator,
    /// Strict fake subprocess runner used by default.
    fake_runner: fake_proc.FakeRunner,
    /// Deterministic clock used by default.
    fake_clock: clock_mod.FakeClock,
    /// Optional cwd override for commands that resolve paths.
    cwd_override: ?std.Io.Dir,

    /// Optional harness overrides.
    pub const Options = struct {
        /// Overrides `deps.cwd`. Slice 3+ commands resolve paths relative to
        /// `deps.cwd`; tests that exercise that resolution must pass an
        /// explicit value rather than letting the test runner's cwd leak in.
        /// Slice 2 callers may leave this null and get `std.Io.Dir.cwd()`.
        cwd: ?std.Io.Dir = null,
    };

    /// Create a harness with default options.
    pub fn init(allocator: std.mem.Allocator, args: []const []const u8) Harness {
        return initWith(allocator, args, .{});
    }

    /// Create a harness with an optional cwd override.
    pub fn initWith(allocator: std.mem.Allocator, args: []const []const u8, opts: Options) Harness {
        return .{
            .stdout_buf = .init(allocator),
            .stderr_buf = .init(allocator),
            .iter = .{ .items = args },
            .gpa = allocator,
            .fake_runner = fake_proc.FakeRunner.init(allocator),
            .fake_clock = clock_mod.FakeClock.init(default_fake_now_ms),
            .cwd_override = opts.cwd,
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
            .gpa = self.gpa,
            .io = std.testing.io,
            .cwd = self.cwd_override orelse std.Io.Dir.cwd(),
            .runner = self.fake_runner.runner(),
            .clock = self.fake_clock.clock(),
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
