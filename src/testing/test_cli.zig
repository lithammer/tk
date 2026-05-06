const std = @import("std");
const cli = @import("../cli.zig");
const SliceArgIter = @import("arg_iter.zig").SliceArgIter;

pub const Harness = struct {
    stdout_buf: std.Io.Writer.Allocating,
    stderr_buf: std.Io.Writer.Allocating,
    iter: SliceArgIter,
    gpa: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator, args: []const []const u8) Harness {
        return .{
            .stdout_buf = .init(allocator),
            .stderr_buf = .init(allocator),
            .iter = .{ .items = args },
            .gpa = allocator,
        };
    }

    pub fn deinit(self: *Harness) void {
        self.stdout_buf.deinit();
        self.stderr_buf.deinit();
    }

    pub fn deps(self: *Harness) cli.Deps {
        return .{
            .stdout = &self.stdout_buf.writer,
            .stderr = &self.stderr_buf.writer,
            .gpa = self.gpa,
        };
    }

    pub fn stdout(self: *Harness) []const u8 {
        return self.stdout_buf.written();
    }

    pub fn stderr(self: *Harness) []const u8 {
        return self.stderr_buf.written();
    }
};
