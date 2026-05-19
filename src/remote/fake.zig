//! Script-queue Backend Adapter for engine tests.
//!
//! Mirrors `src/proc/fake.zig` but operates on the `remote.Adapter` contract.
//! Each script entry is consumed in order; an unmatched call panics so a test
//! that forgot to declare an interaction fails immediately instead of
//! receiving a silent default.

const std = @import("std");
const Allocator = std.mem.Allocator;

const adapter_mod = @import("adapter.zig");
const Adapter = adapter_mod.Adapter;
const ApplyError = adapter_mod.ApplyError;
const BackendItemSnapshot = adapter_mod.BackendItemSnapshot;
const Diagnostic = @import("../store/diagnostic.zig").Diagnostic;
const Failure = adapter_mod.Failure;
const MutationView = adapter_mod.MutationView;
const MutationType = @import("../domain/mutation_type.zig").MutationType;
const Outcome = adapter_mod.Outcome;
const PullError = adapter_mod.PullError;
const Receipt = adapter_mod.Receipt;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;

/// Scripted response for one `pullBackendItems` call.
pub const PullResponse = union(enum) {
    /// Success: the fake `gpa.dupe`s each contained string and returns a new
    /// slice the caller owns (matching the real adapter ownership contract).
    snapshots: []const BackendItemSnapshot,
    /// Adapter-level rejection: the fake returns `PullError.PullFailed` and
    /// writes this text into the `?*Diagnostic` out-param (via `capture`).
    recorded_failure: []const u8,
    /// Environment failure: the fake returns this bare error tag directly.
    /// Use for `ExecutableNotFound`, `SpawnFailed`, `OutOfMemory`.
    env_failure: PullError,
};

/// Scripted response for one `applyMutation` call.
pub const ApplyResponse = union(enum) {
    /// Mutation accepted — fake returns `Outcome.success` with an empty Receipt.
    success: void,
    /// Mutation rejected — fake `gpa.dupe`s this detail and returns
    /// `Outcome.failure` whose `detail` is owned by the caller.
    recorded_failure: []const u8,
    /// Environment failure: bare error tag returned directly.
    env_failure: ApplyError,
};

/// Recorded `applyMutation` invocation captured by `FakeAdapter` for test
/// assertions.
pub const ApplyCall = struct {
    /// Mutation Log sequence of the invoked entry.
    sequence: i64,
    /// Mutation kind passed to the fake.
    mutation_type: MutationType,
    /// Internal stable `items.id` duped via the FakeAdapter's `gpa` and
    /// freed in `FakeAdapter.deinit`.
    item_id: []u8,
    /// JSON-stringified copy of the payload variant for assertions. Owned by
    /// the FakeAdapter's `gpa` and freed in `FakeAdapter.deinit`.
    payload_text: []u8,
};

/// Strict, script-queue Backend Adapter for engine tests.
///
/// `pull_script` and `apply_script` are consumed in order. Each call pops the
/// next entry and increments the corresponding index. Overflowing either
/// script panics — `@panic("FakeAdapter: pull script exhausted")` /
/// `@panic("FakeAdapter: apply script exhausted")`. Zig's testing
/// infrastructure cannot catch panics in v0.16, so callers must ensure their
/// scripts are exactly the right length; the panic exists to surface test
/// bugs loudly.
pub const FakeAdapter = struct {
    /// Allocator used for FakeAdapter-owned bookkeeping (the captured-applies
    /// list and its duped strings). Distinct from the per-call `gpa` argument
    /// used to honor the ownership contract toward callers.
    gpa: Allocator,
    /// Scripted pull responses, consumed in order from index 0.
    pull_script: []const PullResponse,
    /// Scripted apply responses, consumed in order from index 0.
    apply_script: []const ApplyResponse,
    /// Cursor into `pull_script`.
    pull_index: usize = 0,
    /// Cursor into `apply_script`.
    apply_index: usize = 0,
    /// Recorded apply invocations in call order. Each entry owns its
    /// `item_id` and `payload_text` via `gpa` until `deinit`.
    captured_applies: std.ArrayList(ApplyCall) = .empty,

    /// Bind the fake to its scripts and bookkeeping allocator.
    pub fn init(
        gpa: Allocator,
        pull_script: []const PullResponse,
        apply_script: []const ApplyResponse,
    ) FakeAdapter {
        return .{
            .gpa = gpa,
            .pull_script = pull_script,
            .apply_script = apply_script,
        };
    }

    /// Free captured-applies entries and the backing list.
    pub fn deinit(self: *FakeAdapter) void {
        for (self.captured_applies.items) |call| {
            self.gpa.free(call.item_id);
            self.gpa.free(call.payload_text);
        }
        self.captured_applies.deinit(self.gpa);
    }

    /// Return the type-erased adapter view.
    pub fn adapter(self: *FakeAdapter) Adapter {
        return .{ .context = self, .vtable = &vtable };
    }

    const vtable: Adapter.VTable = .{
        .pullBackendItems = pullImpl,
        .applyMutation = applyImpl,
    };

    fn buildApplyCall(gpa: Allocator, view: MutationView) error{OutOfMemory}!ApplyCall {
        const item_id = try gpa.dupe(u8, view.item_id);
        errdefer gpa.free(item_id);
        const payload_text = switch (view.payload) {
            inline else => |v| try std.json.Stringify.valueAlloc(gpa, v, .{}),
        };
        return .{
            .sequence = view.sequence,
            .mutation_type = view.mutation_type,
            .item_id = item_id,
            .payload_text = payload_text,
        };
    }

    fn pullImpl(
        context: *anyopaque,
        gpa: Allocator,
        diag: ?*Diagnostic,
    ) PullError![]BackendItemSnapshot {
        const self: *FakeAdapter = @ptrCast(@alignCast(context));
        if (self.pull_index >= self.pull_script.len) {
            @panic("FakeAdapter: pull script exhausted");
        }
        const response = self.pull_script[self.pull_index];
        self.pull_index += 1;

        switch (response) {
            .snapshots => |items| {
                const out = try gpa.alloc(BackendItemSnapshot, items.len);
                // Track how many entries are fully initialised so an OOM
                // mid-loop frees just the prefix and the outer slice.
                var initialised: usize = 0;
                errdefer {
                    for (out[0..initialised]) |snap| snap.deinit(gpa);
                    gpa.free(out);
                }
                while (initialised < items.len) : (initialised += 1) {
                    const src = items[initialised];
                    const backend_kind = try gpa.dupe(u8, src.backend_kind);
                    errdefer gpa.free(backend_kind);
                    const backend_key = try gpa.dupe(u8, src.backend_key);
                    errdefer gpa.free(backend_key);
                    const display_id = try gpa.dupe(u8, src.display_id);
                    errdefer gpa.free(display_id);
                    const title = try gpa.dupe(u8, src.title);
                    errdefer gpa.free(title);
                    const body = try gpa.dupe(u8, src.body);
                    errdefer gpa.free(body);
                    const backend_updated_at = try gpa.dupe(u8, src.backend_updated_at);
                    out[initialised] = .{
                        .backend_kind = backend_kind,
                        .backend_key = backend_key,
                        .display_id = display_id,
                        .item_class = src.item_class,
                        .ticket_kind = src.ticket_kind,
                        .title = title,
                        .body = body,
                        .status = src.status,
                        .backend_updated_at = backend_updated_at,
                    };
                }
                return out;
            },
            .recorded_failure => |text| {
                if (diag) |d| d.capture(text);
                return error.PullFailed;
            },
            .env_failure => |err| return err,
        }
    }

    fn applyImpl(
        context: *anyopaque,
        gpa: Allocator,
        view: MutationView,
        now: []const u8,
    ) ApplyError!Outcome {
        _ = now;
        const self: *FakeAdapter = @ptrCast(@alignCast(context));

        // Record the call before consulting the script so even env_failure
        // and recorded_failure paths leave evidence in `captured_applies`.
        // Reserve the slot first so the subsequent dupes can safely transfer
        // ownership without leaving an errdefer in scope that would double-
        // free on a later return-with-error (e.g. the env_failure arm below).
        try self.captured_applies.ensureUnusedCapacity(self.gpa, 1);
        const recorded = try buildApplyCall(self.gpa, view);
        self.captured_applies.appendAssumeCapacity(recorded);

        if (self.apply_index >= self.apply_script.len) {
            @panic("FakeAdapter: apply script exhausted");
        }
        const response = self.apply_script[self.apply_index];
        self.apply_index += 1;

        switch (response) {
            .success => return .{ .success = .{} },
            .recorded_failure => |text| {
                const detail = try gpa.dupe(u8, text);
                return .{ .failure = .{ .detail = detail } };
            },
            .env_failure => |err| return err,
        }
    }
};

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

test "FakeAdapter: pull returns scripted snapshots" {
    const gpa = std.testing.allocator;

    const scripted = [_]BackendItemSnapshot{
        .{
            .backend_kind = "github",
            .backend_key = "1",
            .display_id = "gh-1",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "First",
            .body = "Body one",
            .status = .open,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
        .{
            .backend_kind = "github",
            .backend_key = "2",
            .display_id = "gh-2",
            .item_class = .epic,
            .ticket_kind = null,
            .title = "Second",
            .body = "Body two",
            .status = .active,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };
    const pull_script = [_]PullResponse{.{ .snapshots = &scripted }};
    const apply_script = [_]ApplyResponse{};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    const got = try a.pullBackendItems(gpa, null);
    defer {
        for (got) |snap| snap.deinit(gpa);
        gpa.free(got);
    }

    try std.testing.expectEqual(@as(usize, 2), got.len);
    try std.testing.expectEqualStrings("github", got[0].backend_kind);
    try std.testing.expectEqualStrings("1", got[0].backend_key);
    try std.testing.expectEqualStrings("gh-1", got[0].display_id);
    try std.testing.expectEqual(.ticket, got[0].item_class);
    try std.testing.expectEqual(@as(?TicketKind, .task), got[0].ticket_kind);
    try std.testing.expectEqualStrings("First", got[0].title);
    try std.testing.expectEqualStrings("Body one", got[0].body);
    try std.testing.expectEqual(.open, got[0].status);
    try std.testing.expectEqualStrings("2026-05-19T00:00:00Z", got[0].backend_updated_at);
    try std.testing.expectEqualStrings("github", got[1].backend_kind);
    try std.testing.expectEqualStrings("2", got[1].backend_key);
    try std.testing.expectEqualStrings("gh-2", got[1].display_id);
    try std.testing.expectEqual(.epic, got[1].item_class);
    try std.testing.expectEqual(@as(?TicketKind, null), got[1].ticket_kind);
    try std.testing.expectEqualStrings("Second", got[1].title);
    try std.testing.expectEqualStrings("Body two", got[1].body);
    try std.testing.expectEqual(.active, got[1].status);
    try std.testing.expectEqualStrings("2026-05-19T00:00:00Z", got[1].backend_updated_at);
}

test "FakeAdapter: pull returns empty snapshot slice" {
    const gpa = std.testing.allocator;

    const empty: []const BackendItemSnapshot = &.{};
    const pull_script = [_]PullResponse{.{ .snapshots = empty }};
    const apply_script = [_]ApplyResponse{};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    const got = try a.pullBackendItems(gpa, null);
    defer gpa.free(got);

    try std.testing.expectEqual(@as(usize, 0), got.len);
}

test "FakeAdapter: pull recorded_failure returns PullFailed + diag" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{.{ .recorded_failure = "gh: HTTP 502" }};
    const apply_script = [_]ApplyResponse{};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    var diag: Diagnostic = .{};
    const a = fake.adapter();
    try std.testing.expectError(error.PullFailed, a.pullBackendItems(gpa, &diag));
    try std.testing.expect(std.mem.indexOf(u8, diag.message(), "HTTP 502") != null);
}

test "FakeAdapter: pull recorded_failure with null diag still returns PullFailed" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{.{ .recorded_failure = "gh: ignored" }};
    const apply_script = [_]ApplyResponse{};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    try std.testing.expectError(error.PullFailed, a.pullBackendItems(gpa, null));
}

test "FakeAdapter: pull advances the script across multiple calls" {
    const gpa = std.testing.allocator;

    const scripted = [_]BackendItemSnapshot{
        .{
            .backend_kind = "github",
            .backend_key = "1",
            .display_id = "gh-1",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "First",
            .body = "",
            .status = .open,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };
    const pull_script = [_]PullResponse{
        .{ .snapshots = &scripted },
        .{ .env_failure = error.ExecutableNotFound },
    };
    const apply_script = [_]ApplyResponse{};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    const got = try a.pullBackendItems(gpa, null);
    defer {
        for (got) |snap| snap.deinit(gpa);
        gpa.free(got);
    }
    try std.testing.expectEqual(@as(usize, 1), got.len);

    try std.testing.expectError(error.ExecutableNotFound, a.pullBackendItems(gpa, null));
    try std.testing.expectEqual(@as(usize, 2), fake.pull_index);
}

test "FakeAdapter: pull env_failure returns bare error tag" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{.{ .env_failure = error.ExecutableNotFound }};
    const apply_script = [_]ApplyResponse{};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    try std.testing.expectError(error.ExecutableNotFound, a.pullBackendItems(gpa, null));
}

test "FakeAdapter: apply success returns Outcome.success" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{};
    const apply_script = [_]ApplyResponse{.success};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    const outcome = try a.applyMutation(gpa, .{
        .sequence = 1,
        .mutation_type = .update_ticket,
        .item_id = "t1",
        .item_class = .ticket,
        .payload = .{ .update_title_body = .{ .title = "T", .body = "B" } },
        .backend_kind = "github",
        .backend_key = "1",
    }, "2026-05-19T00:00:00.000Z");

    try std.testing.expect(outcome == .success);
}

test "FakeAdapter: apply records the call with payload" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{};
    const apply_script = [_]ApplyResponse{.success};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    const outcome = try a.applyMutation(gpa, .{
        .sequence = 7,
        .mutation_type = .update_ticket,
        .item_id = "t1",
        .item_class = .ticket,
        .payload = .{ .update_title_body = .{ .title = "T", .body = "B" } },
        .backend_kind = "github",
        .backend_key = "1",
    }, "2026-05-19T00:00:00.000Z");
    try std.testing.expect(outcome == .success);

    try std.testing.expectEqual(@as(usize, 1), fake.captured_applies.items.len);
    const recorded = fake.captured_applies.items[0];
    try std.testing.expectEqual(@as(i64, 7), recorded.sequence);
    try std.testing.expectEqual(MutationType.update_ticket, recorded.mutation_type);
    try std.testing.expectEqualStrings("t1", recorded.item_id);
    try std.testing.expect(std.mem.indexOf(u8, recorded.payload_text, "\"title\":\"T\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, recorded.payload_text, "\"body\":\"B\"") != null);
}

test "FakeAdapter: apply recorded_failure returns Outcome.failure" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{};
    const apply_script = [_]ApplyResponse{.{ .recorded_failure = "validation: title required" }};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    const outcome = try a.applyMutation(gpa, .{
        .sequence = 3,
        .mutation_type = .set_item_status,
        .item_id = "t1",
        .item_class = .ticket,
        .payload = .{ .item_status = .{ .status = "done" } },
        .backend_kind = "github",
        .backend_key = "1",
    }, "2026-05-19T00:00:00.000Z");

    try std.testing.expect(outcome == .failure);
    defer outcome.failure.deinit(gpa);
    try std.testing.expectEqualStrings("validation: title required", outcome.failure.detail);

    // Failure path must still record evidence in captured_applies.
    try std.testing.expectEqual(@as(usize, 1), fake.captured_applies.items.len);
    try std.testing.expectEqual(@as(i64, 3), fake.captured_applies.items[0].sequence);
    try std.testing.expectEqualStrings("t1", fake.captured_applies.items[0].item_id);
}

test "FakeAdapter: apply env_failure returns bare error tag" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{};
    const apply_script = [_]ApplyResponse{.{ .env_failure = error.SpawnFailed }};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    try std.testing.expectError(error.SpawnFailed, a.applyMutation(gpa, .{
        .sequence = 1,
        .mutation_type = .update_ticket,
        .item_id = "t1",
        .item_class = .ticket,
        .payload = .{ .update_title_body = .{ .title = "T", .body = "B" } },
        .backend_kind = "github",
        .backend_key = "1",
    }, "2026-05-19T00:00:00.000Z"));

    // env_failure path must still record evidence in captured_applies.
    try std.testing.expectEqual(@as(usize, 1), fake.captured_applies.items.len);
    try std.testing.expectEqual(@as(i64, 1), fake.captured_applies.items[0].sequence);
    try std.testing.expectEqualStrings("t1", fake.captured_applies.items[0].item_id);
}

test "FakeAdapter: apply records epic_ref payload as JSON" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{};
    const apply_script = [_]ApplyResponse{.success};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    const outcome = try a.applyMutation(gpa, .{
        .sequence = 2,
        .mutation_type = .add_ticket_to_epic,
        .item_id = "t1",
        .item_class = .ticket,
        .payload = .{ .epic_ref = .{ .epic_id = "e-internal" } },
        .backend_kind = "github",
        .backend_key = "1",
    }, "2026-05-19T00:00:00.000Z");
    try std.testing.expect(outcome == .success);

    const recorded = fake.captured_applies.items[0];
    try std.testing.expect(std.mem.indexOf(u8, recorded.payload_text, "\"epic_id\":\"e-internal\"") != null);
}

test "FakeAdapter: apply records dependency_ref payload as JSON" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{};
    const apply_script = [_]ApplyResponse{.success};

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    const outcome = try a.applyMutation(gpa, .{
        .sequence = 4,
        .mutation_type = .add_dependency,
        .item_id = "t1",
        .item_class = .ticket,
        .payload = .{ .dependency_ref = .{ .blocking_id = "b-internal" } },
        .backend_kind = "github",
        .backend_key = "1",
    }, "2026-05-19T00:00:00.000Z");
    try std.testing.expect(outcome == .success);

    const recorded = fake.captured_applies.items[0];
    try std.testing.expect(std.mem.indexOf(u8, recorded.payload_text, "\"blocking_id\":\"b-internal\"") != null);
}

test "FakeAdapter: apply advances the script across multiple calls" {
    const gpa = std.testing.allocator;

    const pull_script = [_]PullResponse{};
    const apply_script = [_]ApplyResponse{
        .success,
        .{ .recorded_failure = "second call failed" },
    };

    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const a = fake.adapter();
    const first = try a.applyMutation(gpa, .{
        .sequence = 1,
        .mutation_type = .update_ticket,
        .item_id = "t1",
        .item_class = .ticket,
        .payload = .{ .update_title_body = .{ .title = "A", .body = "" } },
        .backend_kind = "github",
        .backend_key = "1",
    }, "2026-05-19T00:00:00.000Z");
    try std.testing.expect(first == .success);

    const second = try a.applyMutation(gpa, .{
        .sequence = 2,
        .mutation_type = .update_ticket,
        .item_id = "t2",
        .item_class = .ticket,
        .payload = .{ .update_title_body = .{ .title = "B", .body = "" } },
        .backend_kind = "github",
        .backend_key = "2",
    }, "2026-05-19T00:00:00.000Z");
    try std.testing.expect(second == .failure);
    defer second.failure.deinit(gpa);
    try std.testing.expectEqualStrings("second call failed", second.failure.detail);

    try std.testing.expectEqual(@as(usize, 2), fake.apply_index);
    try std.testing.expectEqual(@as(usize, 2), fake.captured_applies.items.len);
}
