//! Fake `Http` client for tests. URL-keyed scripted responses.
//!
//! Mirrors `proc/fake.zig`'s strict shape: an unmatched URL panics because
//! it means the test forgot to script the network call the command actually
//! made.

const std = @import("std");
const Allocator = std.mem.Allocator;

const http = @import("client.zig");

const Http = http.Http;
const Error = http.Error;
const JsonResponse = http.JsonResponse;

/// Scripted response returned by `FakeHttpClient`.
pub const ScriptedResponse = struct {
    /// HTTP status code returned to the caller. Non-2xx is valid here; the
    /// trait does not turn it into an error.
    status: u16 = 200,
    /// Response body bytes. Copied into a caller-owned slice for `getJson`
    /// or written verbatim into the sink for `download`.
    body: []const u8 = "",
    /// If non-null, the fake returns this error instead of the scripted
    /// status/body. Lets tests exercise the error-mapping paths
    /// (`NetworkError`, `TlsError`, …) without standing up a real server.
    err: ?Error = null,
};

/// Strict fake HTTP client used by command tests.
pub const FakeHttpClient = struct {
    gpa: Allocator,
    responses: std.StringHashMapUnmanaged(ScriptedResponse),
    key_arena: std.heap.ArenaAllocator,

    /// Create an empty fake client.
    pub fn init(gpa: Allocator) FakeHttpClient {
        return .{
            .gpa = gpa,
            .responses = .empty,
            .key_arena = std.heap.ArenaAllocator.init(gpa),
        };
    }

    /// Free scripted-response storage and copied URL keys.
    pub fn deinit(self: *FakeHttpClient) void {
        self.responses.deinit(self.gpa);
        self.key_arena.deinit();
    }

    /// Register a scripted response for `url`.
    pub fn expect(self: *FakeHttpClient, url: []const u8, response: ScriptedResponse) !void {
        const owned_url = try self.key_arena.allocator().dupe(u8, url);
        try self.responses.put(self.gpa, owned_url, response);
    }

    /// Return the type-erased `Http` view passed through `cli.Deps`.
    pub fn http(self: *FakeHttpClient) Http {
        return .{ .context = self, .vtable = &vtable };
    }

    const vtable: Http.VTable = .{
        .get_json = getJsonImpl,
        .download = downloadImpl,
    };

    fn getJsonImpl(context: *anyopaque, gpa: Allocator, url: []const u8) Error!JsonResponse {
        const self: *FakeHttpClient = @ptrCast(@alignCast(context));
        const resp = self.responses.get(url) orelse {
            std.debug.print("FakeHttpClient: no expectation matched url: {s}\n", .{url});
            @panic("FakeHttpClient: unexpected URL");
        };
        if (resp.err) |e| return e;
        const body = try gpa.dupe(u8, resp.body);
        return .{ .status = resp.status, .body = body };
    }

    fn downloadImpl(context: *anyopaque, gpa: Allocator, url: []const u8, sink: *std.Io.Writer) Error!u16 {
        _ = gpa;
        const self: *FakeHttpClient = @ptrCast(@alignCast(context));
        const resp = self.responses.get(url) orelse {
            std.debug.print("FakeHttpClient: no expectation matched url: {s}\n", .{url});
            @panic("FakeHttpClient: unexpected URL");
        };
        if (resp.err) |e| return e;
        sink.writeAll(resp.body) catch return error.WriteFailed;
        return resp.status;
    }
};

test "FakeHttpClient.getJson returns scripted body and status" {
    var fake = FakeHttpClient.init(std.testing.allocator);
    defer fake.deinit();
    try fake.expect("https://example.com/", .{
        .status = 200,
        .body = "{\"tag_name\":\"v0.1.0\"}",
    });

    var resp = try fake.http().getJson(std.testing.allocator, "https://example.com/");
    defer resp.deinit(std.testing.allocator);

    try std.testing.expectEqual(@as(u16, 200), resp.status);
    try std.testing.expectEqualStrings("{\"tag_name\":\"v0.1.0\"}", resp.body);
}

test "FakeHttpClient.download streams scripted bytes into the sink" {
    var fake = FakeHttpClient.init(std.testing.allocator);
    defer fake.deinit();
    try fake.expect("https://example.com/asset.bin", .{
        .status = 200,
        .body = "asset bytes",
    });

    var captured: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer captured.deinit();

    const status = try fake.http().download(
        std.testing.allocator,
        "https://example.com/asset.bin",
        &captured.writer,
    );

    try std.testing.expectEqual(@as(u16, 200), status);
    try std.testing.expectEqualStrings("asset bytes", captured.written());
}

test "FakeHttpClient surfaces non-2xx status without erroring" {
    var fake = FakeHttpClient.init(std.testing.allocator);
    defer fake.deinit();
    try fake.expect("https://example.com/missing", .{ .status = 404, .body = "" });

    var resp = try fake.http().getJson(std.testing.allocator, "https://example.com/missing");
    defer resp.deinit(std.testing.allocator);
    try std.testing.expectEqual(@as(u16, 404), resp.status);
    try std.testing.expectEqualStrings("", resp.body);
}

test "FakeHttpClient.getJson injects scripted errors" {
    var fake = FakeHttpClient.init(std.testing.allocator);
    defer fake.deinit();
    try fake.expect("https://example.com/", .{ .err = error.NetworkError });

    try std.testing.expectError(
        Error.NetworkError,
        fake.http().getJson(std.testing.allocator, "https://example.com/"),
    );
}
