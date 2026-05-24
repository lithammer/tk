//! Type-erased HTTP client trait used by `tk self-update`.
//!
//! Mirrors `proc.Runner`'s vtable shape so commands receive HTTP capability
//! through `cli.Deps` and tests can substitute the `FakeHttpClient` from
//! `http/fake.zig`. Two methods are exposed: `getJson` for buffered small
//! responses (the GitHub Releases API) and `download` for streamed asset
//! downloads to a caller-supplied writer.
//!
//! The real implementation wraps `std.http.Client.fetch`. Non-2xx statuses
//! are surfaced through the response value (status is part of the result),
//! not through the error set, because `fetch` itself does not fail on
//! non-2xx and `tk self-update` needs to distinguish 404 (asset missing)
//! from 5xx (transient) by status code.

const std = @import("std");
const Allocator = std.mem.Allocator;

/// Error set exposed to command handlers by any HTTP client implementation.
///
/// HTTP status codes (including non-2xx) are *not* in this set — they ride
/// the `JsonResponse.status` field and `download`'s return value so callers
/// can branch on the code.
pub const Error = error{
    /// TCP connect failure, DNS failure, or any other transport-layer issue
    /// short of TLS-specific failure.
    NetworkError,
    /// TLS handshake or certificate validation failure.
    TlsError,
    /// Server returned a response that the client could not parse (truncated
    /// headers, invalid content-encoding, etc.).
    MalformedResponse,
    /// The caller-supplied sink writer failed during `download`.
    WriteFailed,
    OutOfMemory,
};

/// Buffered response for `getJson`.
pub const JsonResponse = struct {
    /// HTTP status code from the final response in the redirect chain.
    status: u16,
    /// Response body bytes. Owned by the allocator passed to `getJson`.
    body: []u8,

    /// Free `body` with the allocator passed to `getJson`.
    pub fn deinit(self: *JsonResponse, gpa: Allocator) void {
        gpa.free(self.body);
    }
};

/// Type-erased HTTP client.
pub const Http = struct {
    context: *anyopaque,
    vtable: *const VTable,

    /// HTTP client implementation hooks.
    pub const VTable = struct {
        get_json: *const fn (context: *anyopaque, gpa: Allocator, url: []const u8) Error!JsonResponse,
        download: *const fn (context: *anyopaque, gpa: Allocator, url: []const u8, sink: *std.Io.Writer) Error!u16,
    };

    /// Fetch `url`, buffer the body, return `{ status, body }`.
    pub fn getJson(self: Http, gpa: Allocator, url: []const u8) Error!JsonResponse {
        return self.vtable.get_json(self.context, gpa, url);
    }

    /// Fetch `url`, stream the body into `sink`, return the HTTP status code.
    pub fn download(self: Http, gpa: Allocator, url: []const u8, sink: *std.Io.Writer) Error!u16 {
        return self.vtable.download(self.context, gpa, url, sink);
    }
};

/// Real HTTP client backed by `std.http.Client.fetch`.
///
/// Follows up to 10 redirects (covers GitHub's `releases/latest/download/`
/// CDN redirect), sends a caller-supplied User-Agent (typically
/// `tk/<version> (<triple>)`), and resolves TLS via system root certificates
/// through `std.crypto.Certificate.Bundle.rescan` — works on musl-static
/// builds without OpenSSL.
pub const RealHttp = struct {
    gpa: Allocator,
    io: std.Io,
    user_agent: []const u8,
    client: std.http.Client,

    /// Build a real HTTP client. Caller owns the returned value and must
    /// call `deinit` to release the connection pool and TLS bundle.
    pub fn init(gpa: Allocator, io: std.Io, user_agent: []const u8) RealHttp {
        return .{
            .gpa = gpa,
            .io = io,
            .user_agent = user_agent,
            .client = .{ .allocator = gpa, .io = io },
        };
    }

    /// Release the underlying `std.http.Client`.
    pub fn deinit(self: *RealHttp) void {
        self.client.deinit();
    }

    /// Return the type-erased `Http` view passed through `cli.Deps`.
    pub fn http(self: *RealHttp) Http {
        return .{ .context = self, .vtable = &vtable };
    }

    const vtable: Http.VTable = .{
        .get_json = getJsonImpl,
        .download = downloadImpl,
    };

    fn getJsonImpl(context: *anyopaque, gpa: Allocator, url: []const u8) Error!JsonResponse {
        const self: *RealHttp = @ptrCast(@alignCast(context));
        var body: std.Io.Writer.Allocating = .init(gpa);
        errdefer body.deinit();
        const result = self.client.fetch(.{
            .location = .{ .url = url },
            .response_writer = &body.writer,
            .redirect_behavior = .init(10),
            .headers = .{ .user_agent = .{ .override = self.user_agent } },
        }) catch |err| return mapFetchError(err);
        const slice = body.toOwnedSlice() catch return error.OutOfMemory;
        return .{ .status = @intCast(@intFromEnum(result.status)), .body = slice };
    }

    fn downloadImpl(context: *anyopaque, gpa: Allocator, url: []const u8, sink: *std.Io.Writer) Error!u16 {
        _ = gpa;
        const self: *RealHttp = @ptrCast(@alignCast(context));
        const result = self.client.fetch(.{
            .location = .{ .url = url },
            .response_writer = sink,
            .redirect_behavior = .init(10),
            .headers = .{ .user_agent = .{ .override = self.user_agent } },
        }) catch |err| return mapFetchError(err);
        return @intCast(@intFromEnum(result.status));
    }

    /// Map the wide `std.http.Client.fetch` error set into our small
    /// taxonomy. Status-code-bearing failures (4xx/5xx) do not appear
    /// here — `fetch` returns success with a non-2xx `result.status`,
    /// and the wrapper surfaces that through the response value.
    ///
    /// Server- or response-shape failures (redirect loops, malformed
    /// headers, oversized headers, unsupported transfer/compression)
    /// route to `MalformedResponse` rather than `NetworkError` so the
    /// caller's diagnostic does not blame the network for a problem
    /// the user cannot fix by switching connectivity.
    fn mapFetchError(err: anyerror) Error {
        return switch (err) {
            error.OutOfMemory => error.OutOfMemory,
            error.TlsInitializationFailed,
            error.CertificateBundleLoadFailure,
            => error.TlsError,
            error.WriteFailed => error.WriteFailed,
            error.UnsupportedCompressionMethod,
            error.StreamTooLong,
            error.HttpHeadersInvalid,
            error.HttpHeadersOversize,
            error.HttpChunkInvalid,
            error.HttpChunkTruncated,
            error.HttpHeaderContinuationsUnsupported,
            error.HttpTransferEncodingUnsupported,
            error.HttpConnectionHeaderUnsupported,
            error.HttpRedirectLocationInvalid,
            error.HttpRedirectLocationMissing,
            error.HttpRedirectLocationOversize,
            error.HttpContentEncodingUnsupported,
            error.RedirectRequiresResend,
            error.TooManyHttpRedirects,
            error.UnsupportedUriScheme,
            => error.MalformedResponse,
            else => error.NetworkError,
        };
    }
};
