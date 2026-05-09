//! Thin Zig wrapper over the vendored SQLite C amalgamation.
//!
//! Only what slice 2 needs: open with flags, exec a SQL string, query a single
//! integer, query a single text value, and close. Higher-level features (prep,
//! parameter binding, iteration) land when later slices need them.

const std = @import("std");

pub const c = @cImport({
    @cInclude("sqlite3.h");
});

pub const Error = error{
    OpenFailed,
    ExecFailed,
    PrepareFailed,
    StepFailed,
    BindFailed,
    NoRow,
    BadType,
    OutOfMemory,
};

pub const OpenFlags = struct {
    create: bool = true,
    readwrite: bool = true,
};

pub const Db = struct {
    handle: ?*c.sqlite3,

    pub fn open(path_z: [:0]const u8, flags: OpenFlags) Error!Db {
        var raw: ?*c.sqlite3 = null;
        var c_flags: c_int = c.SQLITE_OPEN_NOMUTEX;
        if (flags.create) c_flags |= c.SQLITE_OPEN_CREATE;
        if (flags.readwrite) c_flags |= c.SQLITE_OPEN_READWRITE;
        const rc = c.sqlite3_open_v2(path_z.ptr, &raw, c_flags, null);
        if (rc != c.SQLITE_OK) {
            if (raw) |h| _ = c.sqlite3_close(h);
            return error.OpenFailed;
        }
        var db: Db = .{ .handle = raw };
        // Per-connection pragmas every Repository Store connection needs.
        // journal_mode persists in the file header; foreign_keys and
        // busy_timeout are connection-scoped and have to be set every open.
        db.exec("pragma journal_mode = wal") catch {};
        db.exec("pragma busy_timeout = 5000") catch {};
        db.exec("pragma foreign_keys = on") catch |err| {
            db.close();
            return err;
        };
        return db;
    }

    pub fn close(self: *Db) void {
        if (self.handle) |h| {
            _ = c.sqlite3_close(h);
            self.handle = null;
        }
    }

    pub fn exec(self: *Db, sql: [:0]const u8) Error!void {
        var err_msg: [*c]u8 = null;
        const rc = c.sqlite3_exec(self.handle, sql.ptr, null, null, &err_msg);
        if (rc != c.SQLITE_OK) {
            if (err_msg != null) c.sqlite3_free(err_msg);
            return error.ExecFailed;
        }
    }

    pub fn errorMessage(self: *Db) []const u8 {
        if (self.handle) |h| {
            const cstr = c.sqlite3_errmsg(h);
            if (cstr != null) return std.mem.sliceTo(cstr, 0);
        }
        return "";
    }

    /// Run a SELECT that returns at most one row of one integer column.
    /// Returns null when there is no row.
    pub fn queryOneInt(self: *Db, sql: [:0]const u8) Error!?i64 {
        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.handle, sql.ptr, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);
        const step_rc = c.sqlite3_step(stmt);
        if (step_rc == c.SQLITE_DONE) return null;
        if (step_rc != c.SQLITE_ROW) return error.StepFailed;
        const value = c.sqlite3_column_int64(stmt, 0);
        return value;
    }

    /// Run a SELECT that returns at most one row of one text column.
    /// The returned slice is allocated from `gpa` and owned by the caller.
    pub fn queryOneText(self: *Db, gpa: std.mem.Allocator, sql: [:0]const u8) Error!?[]u8 {
        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.handle, sql.ptr, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);
        const step_rc = c.sqlite3_step(stmt);
        if (step_rc == c.SQLITE_DONE) return null;
        if (step_rc != c.SQLITE_ROW) return error.StepFailed;
        const ctype = c.sqlite3_column_type(stmt, 0);
        if (ctype == c.SQLITE_NULL) return null;
        const text_ptr = c.sqlite3_column_text(stmt, 0);
        const text_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 0));
        const slice = std.mem.sliceTo(@as([*:0]const u8, @ptrCast(text_ptr)), 0);
        const copy = try gpa.alloc(u8, text_len);
        @memcpy(copy, slice[0..text_len]);
        return copy;
    }
};

test "sqlite: open in-memory and exec" {
    var db = try Db.open(":memory:", .{});
    defer db.close();
    try db.exec("create table t(x integer)");
    try db.exec("insert into t values(42)");
    const v = try db.queryOneInt("select x from t");
    try std.testing.expectEqual(@as(i64, 42), v.?);
}

test "sqlite: queryOneInt returns null on empty table" {
    var db = try Db.open(":memory:", .{});
    defer db.close();
    try db.exec("create table t(x integer)");
    const v = try db.queryOneInt("select x from t");
    try std.testing.expect(v == null);
}

test "sqlite: queryOneText copies value" {
    var db = try Db.open(":memory:", .{});
    defer db.close();
    try db.exec("create table t(s text)");
    try db.exec("insert into t values('hello')");
    const got = try db.queryOneText(std.testing.allocator, "select s from t");
    defer if (got) |g| std.testing.allocator.free(g);
    try std.testing.expectEqualStrings("hello", got.?);
}
