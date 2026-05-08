//! Injectable monotonic-ish clock returning UTC milliseconds since the Unix
//! epoch. Tests substitute a fake clock so timestamps stay deterministic.

const std = @import("std");

pub const Clock = struct {
    context: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        nowMs: *const fn (context: *anyopaque) i64,
    };

    pub fn nowMs(self: Clock) i64 {
        return self.vtable.nowMs(self.context);
    }

    /// Convenience: format `nowMs()` as an ISO-8601 UTC millisecond string.
    pub fn nowIso(self: Clock, buf: *[24]u8) []const u8 {
        return formatIso(self.nowMs(), buf);
    }
};

pub const RealClock = struct {
    io: std.Io,

    pub fn init(io: std.Io) RealClock {
        return .{ .io = io };
    }

    pub fn clock(self: *RealClock) Clock {
        return .{ .context = self, .vtable = &vtable };
    }

    const vtable: Clock.VTable = .{ .nowMs = nowImpl };

    fn nowImpl(context: *anyopaque) i64 {
        const self: *RealClock = @ptrCast(@alignCast(context));
        return std.Io.Clock.real.now(self.io).toMilliseconds();
    }
};

pub const FakeClock = struct {
    /// Mutable so tests can advance the clock between calls.
    current_ms: i64,

    pub fn init(start_ms: i64) FakeClock {
        return .{ .current_ms = start_ms };
    }

    pub fn advance(self: *FakeClock, delta_ms: i64) void {
        self.current_ms += delta_ms;
    }

    pub fn clock(self: *FakeClock) Clock {
        return .{ .context = self, .vtable = &vtable };
    }

    const vtable: Clock.VTable = .{ .nowMs = nowImpl };

    fn nowImpl(context: *anyopaque) i64 {
        const self: *FakeClock = @ptrCast(@alignCast(context));
        return self.current_ms;
    }
};

/// Format a millisecond Unix timestamp as `YYYY-MM-DDTHH:MM:SS.sssZ`.
/// Always exactly 24 bytes. The buffer is filled with the formatted output.
pub fn formatIso(ms: i64, buf: *[24]u8) []const u8 {
    // Days/seconds/ms split. Negative timestamps are clamped to 1970-01-01
    // because Ticket only ever generates "now" timestamps for current state
    // and pre-epoch timestamps would only appear from a misconfigured clock.
    const total_ms: i64 = if (ms < 0) 0 else ms;
    const seconds: u64 = @intCast(@divFloor(total_ms, 1000));
    const millis: u32 = @intCast(@mod(total_ms, 1000));

    const epoch_seconds: std.time.epoch.EpochSeconds = .{ .secs = seconds };
    const day_seconds = epoch_seconds.getDaySeconds();
    const epoch_day = epoch_seconds.getEpochDay();
    const year_day = epoch_day.calculateYearDay();
    const month_day = year_day.calculateMonthDay();

    const year: u16 = year_day.year;
    const month: u8 = month_day.month.numeric();
    const day: u8 = month_day.day_index + 1;
    const hour: u8 = day_seconds.getHoursIntoDay();
    const minute: u8 = day_seconds.getMinutesIntoHour();
    const second: u8 = day_seconds.getSecondsIntoMinute();

    _ = std.fmt.bufPrint(buf, "{d:0>4}-{d:0>2}-{d:0>2}T{d:0>2}:{d:0>2}:{d:0>2}.{d:0>3}Z", .{
        year, month, day, hour, minute, second, millis,
    }) catch unreachable;

    return buf[0..];
}

test "formatIso epoch" {
    var buf: [24]u8 = undefined;
    const out = formatIso(0, &buf);
    try std.testing.expectEqualStrings("1970-01-01T00:00:00.000Z", out);
}

test "formatIso known instant" {
    var buf: [24]u8 = undefined;
    // 2026-05-09T12:34:56.789Z corresponds to a specific epoch ms value.
    const ms: i64 = 1778330096789;
    const out = formatIso(ms, &buf);
    try std.testing.expectEqualStrings("2026-05-09T12:34:56.789Z", out);
}

test "FakeClock advances" {
    var fake = FakeClock.init(1_000);
    var c = fake.clock();
    try std.testing.expectEqual(@as(i64, 1_000), c.nowMs());
    fake.advance(500);
    try std.testing.expectEqual(@as(i64, 1_500), c.nowMs());
}
