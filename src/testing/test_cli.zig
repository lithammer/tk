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
    stdout_buf: std.Io.Writer.Allocating,
    stderr_buf: std.Io.Writer.Allocating,
    iter: SliceArgIter,
    gpa: std.mem.Allocator,
    fake_runner: fake_proc.FakeRunner,
    fake_clock: clock_mod.FakeClock,

    pub fn init(allocator: std.mem.Allocator, args: []const []const u8) Harness {
        return .{
            .stdout_buf = .init(allocator),
            .stderr_buf = .init(allocator),
            .iter = .{ .items = args },
            .gpa = allocator,
            .fake_runner = fake_proc.FakeRunner.init(allocator),
            .fake_clock = clock_mod.FakeClock.init(default_fake_now_ms),
        };
    }

    pub fn deinit(self: *Harness) void {
        self.stdout_buf.deinit();
        self.stderr_buf.deinit();
        self.fake_runner.deinit();
    }

    pub fn deps(self: *Harness) cli.Deps {
        return .{
            .stdout = &self.stdout_buf.writer,
            .stderr = &self.stderr_buf.writer,
            .gpa = self.gpa,
            .io = std.testing.io,
            .cwd = std.Io.Dir.cwd(),
            .runner = self.fake_runner.runner(),
            .clock = self.fake_clock.clock(),
        };
    }

    pub fn stdout(self: *Harness) []const u8 {
        return self.stdout_buf.written();
    }

    pub fn stderr(self: *Harness) []const u8 {
        return self.stderr_buf.written();
    }
};
