//! Sync helpers — Pull merge, Mutation Log decode + state transitions,
//! Mark-skipped curation, Remote configuration, and pending/failed count.
//!
//! Composes types from src/domain/ (MutationPayload, MutationView,
//! BackendItemSnapshot, Outcome, Failure) so the future sync engine
//! (src/sync/) and the tk remote / tk sync commands can use this module
//! without taking a dependency on the wider Repository Store surface in
//! repository.zig. Live in src/store/ because every operation here is
//! SQL on the items / mutations / item_ids / remotes / sync_cursors tables.

const std = @import("std");
const zqlite = @import("zqlite");

const migrations = @import("migrations.zig");
const repository = @import("repository.zig");
const sequences = @import("sequences.zig");
const Diagnostic = @import("diagnostic.zig").Diagnostic;

const BackendItemSnapshot = @import("../domain/backend_item_snapshot.zig").BackendItemSnapshot;
const ItemClass = @import("../domain/item_class.zig").ItemClass;
const ItemStatus = @import("../domain/status.zig").ItemStatus;
const MutationPayload = @import("../domain/mutation_payload.zig").MutationPayload;
const MutationType = @import("../domain/mutation_type.zig").MutationType;
const MutationView = @import("../domain/mutation_view.zig").MutationView;
const outcome_mod = @import("../domain/outcome.zig");
const Outcome = outcome_mod.Outcome;
const Failure = outcome_mod.Failure;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;

// ──────────────────────────────────────────────────────────────────────────────
// Sync helpers (Remote configuration, backend-snapshot merge, Mutation Log)
// ──────────────────────────────────────────────────────────────────────────────

/// Error set returned by `mergeBackendSnapshots`.
pub const MergeError = migrations.QueryError || zqlite.Error || error{
    OutOfMemory,
    /// A snapshot's `display_id` collided with an existing `item_ids.value`
    /// (a Display ID or Alias already claimed by another Item). The caller's
    /// optional `?*Diagnostic` carries the colliding Display ID so the engine
    /// can render `Display ID '<value>' already claimed by an existing Item`.
    DisplayIdCollision,
};

/// Merge a pull's `BackendItemSnapshot` slice into the Repository Store.
///
/// Runs inside its own `begin immediate` transaction. Per snapshot:
///   - A: no `(backend_kind, backend_key)` match → INSERT new `items` row
///     with `origin='backend'`, a fresh random internal ID, and a
///     `created_seq` allocated from `item_created_seq`. Also INSERT the
///     `item_ids` resolver row with `source='display'`. On a unique
///     violation against an existing Display ID the helper returns
///     `MergeError.DisplayIdCollision`; if `diag` is non-null it captures
///     the colliding Display ID for engine rendering.
///   - B: match exists AND a pending/failed Mutation references it → SKIP
///     UPDATE. The Mutation Log is the source of truth for in-flight edits.
///   - C: match exists with no in-flight Mutations → UPDATE title, body,
///     status, updated_at. Display ID is preserved.
///   - D: snapshot list shorter than local backend rows → v1 no-op
///     (deletion detection is deferred).
///
/// `BackendItemSnapshot` does not carry a Priority; backend items default to
/// `'P2'` so the `items` Ticket-Priority constraint is satisfied. Backends
/// will surface real priorities in a later slice.
///
/// `random` supplies the entropy for the internal stable `items.id`. The
/// injected `std.Random` follows the same convention as `createLocalTicket`
/// — Zig 0.16 removed the global `std.crypto.random`, so callers route their
/// `std.Random` (real or seeded test PRNG) into helpers that mint IDs.
pub fn mergeBackendSnapshots(
    conn: zqlite.Conn,
    gpa: std.mem.Allocator,
    random: std.Random,
    snapshots: []const BackendItemSnapshot,
    now: []const u8,
    diag: ?*Diagnostic,
) MergeError!void {
    try conn.execNoArgs("begin immediate");
    // On DisplayIdCollision the explicit `diag.capture(snap.display_id)` below
    // has already written the more useful value; the `collision_captured` flag
    // tells the errdefer block to skip the generic SQL errmsg capture so we
    // don't stomp the better diagnostic.
    var collision_captured: bool = false;
    errdefer {
        if (!collision_captured) migrations.captureError(conn, diag);
        conn.rollback();
    }

    for (snapshots) |snap| {
        const match = try conn.row(
            \\select id from items where backend_kind = ?1 and backend_key = ?2
        , .{ snap.backend_kind, snap.backend_key });

        if (match) |row| {
            defer row.deinit();
            const item_id = try gpa.dupe(u8, row.text(0));
            defer gpa.free(item_id);

            // Scenario B: skip if any pending/failed Mutation targets this Item.
            const pending = try conn.row(
                \\select 1 from mutations
                \\ where item_id = ?1 and item_class = ?2
                \\   and state in ('pending','failed')
                \\ limit 1
            , .{ item_id, snap.item_class.text() });
            if (pending) |p| {
                p.deinit();
                continue;
            }

            // Scenario C: overwrite title/body/status/updated_at, leave
            // Display ID alone.
            try conn.exec(
                \\update items
                \\   set title = ?2, body = ?3, status = ?4, updated_at = ?5
                \\ where id = ?1
            , .{
                item_id,
                snap.title,
                snap.body,
                snap.status.text(),
                now,
            });
            continue;
        }

        // Scenario A: INSERT new backend-origin Item.
        const id = try repository.generateInternalId(gpa, random);
        defer gpa.free(id);
        const created_seq = try sequences.next(conn, "item_created_seq");

        // Ticket Kind is required for tickets; backend snapshots that omit it
        // for a ticket-class Item will surface as a SQL `ConstraintCheck`
        // — the adapter contract owns shape correctness so we don't
        // pre-validate here.
        const ticket_kind_text: ?[]const u8 = if (snap.ticket_kind) |tk| tk.text() else null;
        const priority_text: ?[]const u8 = if (snap.item_class == .ticket) "P2" else null;

        try conn.exec(
            \\insert into items(
            \\  id, display_value, item_class, ticket_kind, priority, title, body,
            \\  origin, backend_kind, backend_key, status,
            \\  created_seq, created_at, updated_at
            \\) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'backend', ?8, ?9, ?10, ?11, ?12, ?12)
        , .{
            id,
            snap.display_id,
            snap.item_class.text(),
            ticket_kind_text,
            priority_text,
            snap.title,
            snap.body,
            snap.backend_kind,
            snap.backend_key,
            snap.status.text(),
            created_seq,
            now,
        });

        // INSERT the Display ID resolver row. A unique violation here means
        // the Display ID is already claimed by another Item — translate to
        // `MergeError.DisplayIdCollision` and surface the offending value
        // through the optional out-param.
        conn.exec(
            "insert into item_ids(value, source, item_id, created_at) values (?1, 'display', ?2, ?3)",
            .{ snap.display_id, id, now },
        ) catch |err| switch (err) {
            error.ConstraintUnique, error.ConstraintPrimaryKey => {
                if (diag) |d| d.capture(snap.display_id);
                collision_captured = true;
                return error.DisplayIdCollision;
            },
            else => return err,
        };
    }

    try conn.commit();
}

/// Error set returned by `loadApplicableMutations`.
pub const LoadApplicableError = migrations.QueryError || zqlite.Error || std.json.ParseError(std.json.Scanner) || error{
    OutOfMemory,
    /// SQL CHECK accepted a `mutation_type` text value that the Zig
    /// `MutationType` enum does not decode — fires when schema and enum
    /// drift (a future migration widened the CHECK without extending the
    /// enum).
    MutationTypeUnknown,
    /// `mutation_type` decoded to a `MutationType` value, but the
    /// `MutationPayload` union does not yet have a matching variant. Fires
    /// today for `promote_ticket`, `promote_epic`, `add_external_blocker`,
    /// and `resolve_external_blocker` — a forward-compatibility guard so
    /// future slices that grow `MutationPayload` cannot accidentally cause
    /// the engine to silently skip undecodable rows.
    MutationPayloadVariantMissing,
};

/// Decode the typed Mutation Log entries the engine should attempt to
/// (re)apply.
///
/// Returns Mutation Log rows in `state in ('pending','failed')` ordered by
/// `sequence`. Each row is joined to `items` so backend identifiers reach the
/// adapter without a second query. All returned slices are owned by `gpa`
/// and freed via `deinitMutationView`.
///
/// `mutation_type` is parsed via `MutationType.fromText`; an unknown value
/// surfaces as `error.MutationTypeUnknown`. Forward-compatible Mutation
/// kinds whose payload variants are not in `MutationPayload` yet
/// (`promote_ticket`, `promote_epic`, `add_external_blocker`,
/// `resolve_external_blocker`) return `error.MutationPayloadVariantMissing`
/// — the engine recognises the schema-allowed kind but has no projection.
pub fn loadApplicableMutations(
    conn: zqlite.Conn,
    gpa: std.mem.Allocator,
) LoadApplicableError![]MutationView {
    var list: std.ArrayList(MutationView) = .empty;
    errdefer {
        for (list.items) |v| deinitMutationView(v, gpa);
        list.deinit(gpa);
    }

    var rows = try conn.rows(
        \\select m.sequence, m.mutation_type, m.item_id, m.item_class,
        \\       m.payload_json, i.backend_kind, i.backend_key
        \\  from mutations m
        \\  join items i on i.id = m.item_id and i.item_class = m.item_class
        \\ where m.state in ('pending','failed')
        \\ order by m.sequence asc
    , .{});
    defer rows.deinit();

    while (rows.next()) |r| {
        const sequence = r.int(0);
        const type_text = r.text(1);
        const item_id_text = r.text(2);
        const item_class_text = r.text(3);
        const payload_text = r.text(4);
        const backend_kind_text = r.nullableText(5);
        const backend_key_text = r.nullableText(6);

        const mutation_type = MutationType.fromText(type_text) orelse
            return error.MutationTypeUnknown;
        const item_class = repository.enumFromText(ItemClass, item_class_text);

        const payload = try decodeMutationPayload(gpa, mutation_type, payload_text);
        errdefer freeMutationPayload(payload, gpa);

        const item_id = try gpa.dupe(u8, item_id_text);
        errdefer gpa.free(item_id);
        const backend_kind: ?[]const u8 = if (backend_kind_text) |s| try gpa.dupe(u8, s) else null;
        errdefer if (backend_kind) |s| gpa.free(s);
        const backend_key: ?[]const u8 = if (backend_key_text) |s| try gpa.dupe(u8, s) else null;
        errdefer if (backend_key) |s| gpa.free(s);

        try list.append(gpa, .{
            .sequence = sequence,
            .mutation_type = mutation_type,
            .item_id = item_id,
            .item_class = item_class,
            .payload = payload,
            .backend_kind = backend_kind,
            .backend_key = backend_key,
        });
    }
    if (rows.err) |err| return err;

    return list.toOwnedSlice(gpa);
}

/// Free the owned slices on a `MutationView` returned by
/// `loadApplicableMutations`. The view's payload variant determines which
/// nested slices must be freed.
pub fn deinitMutationView(view: MutationView, gpa: std.mem.Allocator) void {
    gpa.free(view.item_id);
    if (view.backend_kind) |s| gpa.free(s);
    if (view.backend_key) |s| gpa.free(s);
    freeMutationPayload(view.payload, gpa);
}

/// Decode a `payload_json` text column into the typed `MutationPayload`
/// variant the `MutationType` selects. All returned slices are owned by
/// `gpa` and freed via `freeMutationPayload`.
fn decodeMutationPayload(
    gpa: std.mem.Allocator,
    mutation_type: MutationType,
    payload_text: []const u8,
) LoadApplicableError!MutationPayload {
    return switch (mutation_type) {
        .update_ticket, .update_epic => blk: {
            const parsed = try std.json.parseFromSlice(MutationPayload.TitleBody, gpa, payload_text, .{});
            defer parsed.deinit();
            const title = try gpa.dupe(u8, parsed.value.title);
            errdefer gpa.free(title);
            const body = try gpa.dupe(u8, parsed.value.body);
            break :blk .{ .update_title_body = .{ .title = title, .body = body } };
        },
        .add_ticket_to_epic, .remove_ticket_from_epic => blk: {
            const parsed = try std.json.parseFromSlice(MutationPayload.EpicRef, gpa, payload_text, .{});
            defer parsed.deinit();
            const epic_id = try gpa.dupe(u8, parsed.value.epic_id);
            break :blk .{ .epic_ref = .{ .epic_id = epic_id } };
        },
        .set_item_status => blk: {
            const parsed = try std.json.parseFromSlice(MutationPayload.StatusChange, gpa, payload_text, .{});
            defer parsed.deinit();
            const status = try gpa.dupe(u8, parsed.value.status);
            break :blk .{ .item_status = .{ .status = status } };
        },
        .add_dependency, .remove_dependency => blk: {
            const parsed = try std.json.parseFromSlice(MutationPayload.DependencyRef, gpa, payload_text, .{});
            defer parsed.deinit();
            const blocking_id = try gpa.dupe(u8, parsed.value.blocking_id);
            break :blk .{ .dependency_ref = .{ .blocking_id = blocking_id } };
        },
        .promote_ticket,
        .promote_epic,
        .add_external_blocker,
        .resolve_external_blocker,
        => return error.MutationPayloadVariantMissing,
    };
}

fn freeMutationPayload(payload: MutationPayload, gpa: std.mem.Allocator) void {
    switch (payload) {
        .update_title_body => |p| {
            gpa.free(p.title);
            gpa.free(p.body);
        },
        .epic_ref => |p| gpa.free(p.epic_id),
        .item_status => |p| gpa.free(p.status),
        .dependency_ref => |p| gpa.free(p.blocking_id),
    }
}

/// Advance the v1 single Remote's Sync Cursor to `sequence`.
///
/// Caller holds an outer transaction; this helper does not begin or commit.
/// The v1 schema has exactly one Sync Cursor (the singleton `primary` row)
/// so this is an unconditional `update`.
pub fn advanceSyncCursor(
    conn: zqlite.Conn,
    sequence: i64,
    now: []const u8,
) (migrations.QueryError || zqlite.Error)!void {
    try conn.exec(
        \\update sync_cursors
        \\   set last_applied_sequence = ?1, updated_at = ?2
        \\ where remote_name = 'primary'
    , .{ sequence, now });
}

/// Error set returned by `applyMutationOutcome`.
pub const ApplyMutationOutcomeError = migrations.QueryError || zqlite.Error || error{
    OutOfMemory,
    /// No `mutations` row matches the supplied `sequence`.
    MutationNotFound,
    /// The matched row's prior state is `applied` or `skipped` — the engine
    /// must never request a transition out of these terminal states.
    MutationNotApplicable,
};

/// Persist the effect of an adapter `Outcome` against the Mutation Log row
/// at `sequence`.
///
/// Opens its own `begin immediate` transaction. The state transitions are:
///   - `pending` + `.success` → `applied`, clear `failure_json`,
///     advance the Sync Cursor.
///   - `pending` + `.failure { detail }` → `failed`, record
///     `{"detail":"..."}` in `failure_json`.
///   - `failed`  + `.success` → `applied`, clear `failure_json`,
///     advance the Sync Cursor.
///   - `failed`  + `.failure { detail }` → state unchanged, replace
///     `failure_json` with the new wrapped detail.
///   - Any other prior state → `error.MutationNotApplicable`.
/// Missing rows return `error.MutationNotFound`.
pub fn applyMutationOutcome(
    conn: zqlite.Conn,
    gpa: std.mem.Allocator,
    sequence: i64,
    outcome: Outcome,
    now: []const u8,
    diag: ?*Diagnostic,
) ApplyMutationOutcomeError!void {
    try conn.execNoArgs("begin immediate");
    errdefer {
        migrations.captureError(conn, diag);
        conn.rollback();
    }

    const state_row = (try conn.row(
        "select state from mutations where sequence = ?1",
        .{sequence},
    )) orelse return error.MutationNotFound;
    const prior_text = state_row.text(0);
    const prior_is_pending = std.mem.eql(u8, prior_text, "pending");
    const prior_is_failed = std.mem.eql(u8, prior_text, "failed");
    state_row.deinit();

    if (!prior_is_pending and !prior_is_failed) {
        return error.MutationNotApplicable;
    }

    switch (outcome) {
        .success => {
            try conn.exec(
                \\update mutations
                \\   set state = 'applied', failure_json = null,
                \\       state_changed_at = ?2
                \\ where sequence = ?1
            , .{ sequence, now });
            try advanceSyncCursor(conn, sequence, now);
        },
        .failure => |failure| {
            // escape_unicode = true escapes all non-ASCII bytes as \uXXXX so
            // arbitrary CLI stderr (potentially non-UTF-8 or containing
            // control bytes) survives the schema's `json_valid(failure_json)`
            // check.
            const failure_json = try std.json.Stringify.valueAlloc(
                gpa,
                .{ .detail = failure.detail },
                .{ .escape_unicode = true },
            );
            defer gpa.free(failure_json);

            if (prior_is_pending) {
                try conn.exec(
                    \\update mutations
                    \\   set state = 'failed', failure_json = ?2,
                    \\       state_changed_at = ?3
                    \\ where sequence = ?1
                , .{ sequence, failure_json, now });
            } else {
                // prior state was 'failed' — keep state, refresh failure_json.
                try conn.exec(
                    \\update mutations
                    \\   set failure_json = ?2, state_changed_at = ?3
                    \\ where sequence = ?1
                , .{ sequence, failure_json, now });
            }
        },
    }

    try conn.commit();
}

/// Error set returned by `markMutationSkipped`.
pub const MarkSkippedError = migrations.QueryError || zqlite.Error || error{
    /// No `mutations` row matches the supplied `sequence`.
    MutationNotFound,
    /// The matched row's prior state is not `failed`. Skipping is only a
    /// curation tool for mutations the backend has already rejected.
    MutationNotFailed,
};

/// Transition a `failed` Mutation Log entry into `skipped`.
///
/// Opens its own `begin immediate` transaction. Refuses to skip a Mutation
/// that is not in the `failed` state — operators only skip a Mutation after
/// the backend rejected it. Skipping clears no metadata; the operator's
/// audit trail (the latest `failure_json`) is preserved so `tk sync log`
/// can render why the Mutation was abandoned.
pub fn markMutationSkipped(
    conn: zqlite.Conn,
    sequence: i64,
    now: []const u8,
    diag: ?*Diagnostic,
) MarkSkippedError!void {
    try conn.execNoArgs("begin immediate");
    errdefer {
        migrations.captureError(conn, diag);
        conn.rollback();
    }

    const state_row = (try conn.row(
        "select state from mutations where sequence = ?1",
        .{sequence},
    )) orelse return error.MutationNotFound;
    const is_failed = std.mem.eql(u8, state_row.text(0), "failed");
    state_row.deinit();

    if (!is_failed) {
        return error.MutationNotFailed;
    }

    try conn.exec(
        \\update mutations
        \\   set state = 'skipped', state_changed_at = ?2
        \\ where sequence = ?1
    , .{ sequence, now });

    try conn.commit();
}

/// Error set returned by `setRemote`.
pub const SetRemoteError = migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Upsert the v1 singleton Remote configuration row.
///
/// Opens its own `begin immediate` transaction. On first insert the helper
/// also seeds the matching `sync_cursors` row at `last_applied_sequence = 0`.
/// On a subsequent update the existing `remotes.created_at` is preserved and
/// the Sync Cursor is left untouched — the sync engine owns its lifecycle.
pub fn setRemote(
    conn: zqlite.Conn,
    backend_kind: []const u8,
    config_json: []const u8,
    now: []const u8,
    diag: ?*Diagnostic,
) SetRemoteError!void {
    try conn.execNoArgs("begin immediate");
    errdefer {
        migrations.captureError(conn, diag);
        conn.rollback();
    }

    const existing = try conn.row(
        "select 1 from remotes where name = 'primary'",
        .{},
    );

    if (existing) |row| {
        row.deinit();
        try conn.exec(
            \\update remotes
            \\   set backend_kind = ?1, config_json = ?2, updated_at = ?3
            \\ where name = 'primary'
        , .{ backend_kind, config_json, now });
    } else {
        try conn.exec(
            \\insert into remotes(name, backend_kind, config_json, created_at, updated_at)
            \\values ('primary', ?1, ?2, ?3, ?3)
        , .{ backend_kind, config_json, now });
        try conn.exec(
            \\insert into sync_cursors(remote_name, backend_kind, last_applied_sequence, updated_at)
            \\values ('primary', ?1, 0, ?2)
        , .{ backend_kind, now });
    }

    try conn.commit();
}

/// Clear the v1 singleton Remote configuration plus its Sync Cursor.
///
/// The `sync_cursors` row is deleted first because the FK to `remotes` is
/// `on delete restrict`. The helper does NOT check whether pending or failed
/// Mutations remain — that gate is the caller's job (see `tk remote remove`).
/// Calling this with neither row present is a no-op.
pub fn clearRemote(
    conn: zqlite.Conn,
    diag: ?*Diagnostic,
) (migrations.QueryError || zqlite.Error)!void {
    try conn.execNoArgs("begin immediate");
    errdefer {
        migrations.captureError(conn, diag);
        conn.rollback();
    }

    try conn.exec(
        "delete from sync_cursors where remote_name = 'primary'",
        .{},
    );
    try conn.exec(
        "delete from remotes where name = 'primary'",
        .{},
    );

    try conn.commit();
}

/// Loaded copy of the singleton Remote configuration plus its Sync Cursor.
///
/// `backend_kind` and `config_json` are owned by `gpa`; free both via
/// `deinit`.
pub const RemoteRow = struct {
    backend_kind: []u8,
    config_json: []u8,
    last_applied_sequence: i64,

    pub fn deinit(self: RemoteRow, gpa: std.mem.Allocator) void {
        gpa.free(self.backend_kind);
        gpa.free(self.config_json);
    }
};

/// Error set returned by `getRemote`.
pub const GetRemoteError = migrations.QueryError || zqlite.Error || error{OutOfMemory};

/// Read the v1 singleton Remote configuration plus its Sync Cursor.
///
/// Returns `null` when no Remote is configured. Joins `remotes` with
/// `sync_cursors` on `remote_name = 'primary'` so callers get the cursor
/// without a second round-trip.
pub fn getRemote(
    conn: zqlite.Conn,
    gpa: std.mem.Allocator,
) GetRemoteError!?RemoteRow {
    const row = try conn.row(
        \\select r.backend_kind, r.config_json, c.last_applied_sequence
        \\  from remotes r
        \\  join sync_cursors c on c.remote_name = r.name
        \\ where r.name = 'primary'
    , .{}) orelse return null;
    defer row.deinit();

    const backend_kind = try gpa.dupe(u8, row.text(0));
    errdefer gpa.free(backend_kind);
    const config_json = try gpa.dupe(u8, row.text(1));

    return .{
        .backend_kind = backend_kind,
        .config_json = config_json,
        .last_applied_sequence = row.int(2),
    };
}

/// Count Mutation Log entries in `pending` or `failed` state.
///
/// Used by `tk remote remove` to refuse clearing a Remote with in-flight
/// Mutations, and by `tk sync` to detect whether anything needs adapter
/// dispatch on this run.
pub fn pendingOrFailedMutationCount(
    conn: zqlite.Conn,
) (migrations.QueryError || zqlite.Error)!i64 {
    if (try conn.row(
        "select count(*) from mutations where state in ('pending','failed')",
        .{},
    )) |r| {
        defer r.deinit();
        return r.int(0);
    }
    return 0;
}

// ──────────────────────────────────────────────────────────────────────────────
// Sync helper tests
// ──────────────────────────────────────────────────────────────────────────────

test "mergeBackendSnapshots: scenario A inserts a new backend-origin item" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    const snapshots = [_]BackendItemSnapshot{
        .{
            .backend_kind = "github",
            .backend_key = "1",
            .display_id = "gh-1",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "First",
            .body = "Body",
            .status = .open,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };
    try mergeBackendSnapshots(conn, gpa, prng.random(), &snapshots, "2026-05-19T00:00:00Z", null);

    const r = (try conn.row(
        \\select i.display_value, i.title, i.body, i.status, i.origin, i.backend_kind, i.backend_key,
        \\       (select source from item_ids where value = i.display_value)
        \\  from items i where i.display_value = 'gh-1'
    , .{})) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("gh-1", r.text(0));
    try std.testing.expectEqualStrings("First", r.text(1));
    try std.testing.expectEqualStrings("Body", r.text(2));
    try std.testing.expectEqualStrings("open", r.text(3));
    try std.testing.expectEqualStrings("backend", r.text(4));
    try std.testing.expectEqualStrings("github", r.text(5));
    try std.testing.expectEqualStrings("1", r.text(6));
    try std.testing.expectEqualStrings("display", r.text(7));
}

test "mergeBackendSnapshots: scenario B skips items with pending or failed mutations" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Local title",
        .body = "Local body",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t2",
        .display = "gh-2",
        .title = "Local title 2",
        .body = "Local body 2",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"x\",\"body\":\"\"}",
        .state = "pending",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 2,
        .mutation_type = "update_ticket",
        .item_id = "t2",
        .payload_json = "{\"title\":\"y\",\"body\":\"\"}",
        .state = "failed",
        .failure_json = "{\"detail\":\"boom\"}",
    });

    const snapshots = [_]BackendItemSnapshot{
        .{
            .backend_kind = "github",
            .backend_key = "1",
            .display_id = "gh-1",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "Remote title",
            .body = "Remote body",
            .status = .active,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
        .{
            .backend_kind = "github",
            .backend_key = "2",
            .display_id = "gh-2",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "Remote title 2",
            .body = "Remote body 2",
            .status = .active,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };
    try mergeBackendSnapshots(conn, gpa, prng.random(), &snapshots, "2026-05-19T00:00:00Z", null);

    const r1 = (try conn.row("select title, status from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer r1.deinit();
    try std.testing.expectEqualStrings("Local title", r1.text(0));
    try std.testing.expectEqualStrings("open", r1.text(1));

    const r2 = (try conn.row("select title, status from items where id = 't2'", .{})) orelse return error.ExpectedRow;
    defer r2.deinit();
    try std.testing.expectEqualStrings("Local title 2", r2.text(0));
    try std.testing.expectEqualStrings("open", r2.text(1));
}

test "mergeBackendSnapshots: scenario C overwrites title/body/status when no in-flight mutations" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Old title",
        .body = "Old body",
        .status = "open",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    const snapshots = [_]BackendItemSnapshot{
        .{
            .backend_kind = "github",
            .backend_key = "1",
            .display_id = "gh-1",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "New title",
            .body = "New body",
            .status = .active,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };
    try mergeBackendSnapshots(conn, gpa, prng.random(), &snapshots, "2026-05-19T05:00:00Z", null);

    const r = (try conn.row(
        "select title, body, status, display_value, updated_at from items where id = 't1'",
        .{},
    )) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("New title", r.text(0));
    try std.testing.expectEqualStrings("New body", r.text(1));
    try std.testing.expectEqualStrings("active", r.text(2));
    // Display ID is preserved by scenario C.
    try std.testing.expectEqualStrings("gh-1", r.text(3));
    try std.testing.expectEqualStrings("2026-05-19T05:00:00Z", r.text(4));
}

test "mergeBackendSnapshots: scenario D leaves local backend rows alone when snapshot is shorter" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Title 1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t2",
        .display = "gh-2",
        .title = "Title 2",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 2,
    });

    const snapshots = [_]BackendItemSnapshot{};
    try mergeBackendSnapshots(conn, gpa, prng.random(), &snapshots, "2026-05-19T00:00:00Z", null);

    const count_row = (try conn.row("select count(*) from items", .{})) orelse return error.ExpectedRow;
    defer count_row.deinit();
    try std.testing.expectEqual(@as(i64, 2), count_row.int(0));
}

test "mergeBackendSnapshots: DisplayIdCollision captures display_id in diag and rolls back" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    // A local-origin item already claims the Display ID the snapshot wants.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "local",
        .display = "gh-1",
        .title = "Local",
        .created_seq = 1,
    });
    // The fixture writes a row with created_seq=1 without touching the
    // sequences counter. Advance the counter so the helper's allocation
    // doesn't collide on items.created_seq.
    try conn.exec("update sequences set value = 1 where name = 'item_created_seq'", .{});

    const snapshots = [_]BackendItemSnapshot{
        .{
            .backend_kind = "github",
            .backend_key = "1",
            .display_id = "gh-1",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "Remote",
            .body = "",
            .status = .open,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };

    var diag: Diagnostic = .{};
    try std.testing.expectError(
        MergeError.DisplayIdCollision,
        mergeBackendSnapshots(conn, gpa, prng.random(), &snapshots, "2026-05-19T00:00:00Z", &diag),
    );

    try std.testing.expectEqualStrings("gh-1", diag.message());

    // Rollback left only the pre-existing item.
    const count_row = (try conn.row("select count(*) from items", .{})) orelse return error.ExpectedRow;
    defer count_row.deinit();
    try std.testing.expectEqual(@as(i64, 1), count_row.int(0));
}

test "loadApplicableMutations: returns pending+failed in sequence order with typed payloads" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Ticket 1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "gh-10",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Epic",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "10",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t2",
        .display = "gh-2",
        .title = "Ticket 2",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 3,
    });

    // Pending update_ticket (sequence 1).
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"New\",\"body\":\"Body\"}",
        .state = "pending",
    });
    // Failed add_ticket_to_epic (sequence 2).
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 2,
        .mutation_type = "add_ticket_to_epic",
        .item_id = "t1",
        .payload_json = "{\"epic_id\":\"e1\"}",
        .state = "failed",
        .failure_json = "{\"detail\":\"boom\"}",
    });
    // Skipped set_item_status — must be excluded.
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 3,
        .mutation_type = "set_item_status",
        .item_id = "t1",
        .payload_json = "{\"status\":\"active\"}",
        .state = "skipped",
    });
    // Applied add_dependency — must be excluded.
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 4,
        .mutation_type = "add_dependency",
        .item_id = "t1",
        .payload_json = "{\"blocking_id\":\"t2\"}",
        .state = "applied",
    });
    // Pending set_item_status (sequence 5).
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 5,
        .mutation_type = "set_item_status",
        .item_id = "t2",
        .payload_json = "{\"status\":\"done\"}",
        .state = "pending",
    });
    // Pending add_dependency (sequence 6).
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 6,
        .mutation_type = "add_dependency",
        .item_id = "t1",
        .payload_json = "{\"blocking_id\":\"t2\"}",
        .state = "pending",
    });
    // Pending update_epic (sequence 7) — covers Epic class path.
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 7,
        .mutation_type = "update_epic",
        .item_id = "e1",
        .item_class = "epic",
        .payload_json = "{\"title\":\"Epic v2\",\"body\":\"\"}",
        .state = "pending",
    });

    const views = try loadApplicableMutations(conn, gpa);
    defer {
        for (views) |v| deinitMutationView(v, gpa);
        gpa.free(views);
    }

    try std.testing.expectEqual(@as(usize, 5), views.len);
    try std.testing.expectEqual(@as(i64, 1), views[0].sequence);
    try std.testing.expectEqual(MutationType.update_ticket, views[0].mutation_type);
    try std.testing.expectEqualStrings("t1", views[0].item_id);
    try std.testing.expectEqualStrings("github", views[0].backend_kind.?);
    try std.testing.expectEqualStrings("1", views[0].backend_key.?);
    switch (views[0].payload) {
        .update_title_body => |p| {
            try std.testing.expectEqualStrings("New", p.title);
            try std.testing.expectEqualStrings("Body", p.body);
        },
        else => return error.UnexpectedPayloadVariant,
    }

    try std.testing.expectEqual(@as(i64, 2), views[1].sequence);
    try std.testing.expectEqual(MutationType.add_ticket_to_epic, views[1].mutation_type);
    switch (views[1].payload) {
        .epic_ref => |p| try std.testing.expectEqualStrings("e1", p.epic_id),
        else => return error.UnexpectedPayloadVariant,
    }

    try std.testing.expectEqual(@as(i64, 5), views[2].sequence);
    try std.testing.expectEqual(MutationType.set_item_status, views[2].mutation_type);
    switch (views[2].payload) {
        .item_status => |p| try std.testing.expectEqualStrings("done", p.status),
        else => return error.UnexpectedPayloadVariant,
    }

    try std.testing.expectEqual(@as(i64, 6), views[3].sequence);
    try std.testing.expectEqual(MutationType.add_dependency, views[3].mutation_type);
    switch (views[3].payload) {
        .dependency_ref => |p| try std.testing.expectEqualStrings("t2", p.blocking_id),
        else => return error.UnexpectedPayloadVariant,
    }

    try std.testing.expectEqual(@as(i64, 7), views[4].sequence);
    try std.testing.expectEqual(MutationType.update_epic, views[4].mutation_type);
    try std.testing.expectEqual(ItemClass.epic, views[4].item_class);
    switch (views[4].payload) {
        .update_title_body => |p| try std.testing.expectEqualStrings("Epic v2", p.title),
        else => return error.UnexpectedPayloadVariant,
    }
}

test "loadApplicableMutations: promote_ticket row returns MutationPayloadVariantMissing" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "tk-1",
        .title = "Local ticket",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "promote_ticket",
        .item_id = "t1",
        .payload_json = "{}",
        .state = "pending",
    });

    try std.testing.expectError(
        error.MutationPayloadVariantMissing,
        loadApplicableMutations(conn, gpa),
    );
}

test "applyMutationOutcome: pending+success → applied, advances Sync Cursor" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"x\",\"body\":\"\"}",
        .state = "pending",
    });

    try applyMutationOutcome(conn, gpa, 1, .{ .success = .{} }, "2026-05-19T01:00:00Z", null);

    const r = (try conn.row(
        "select state, failure_json, state_changed_at from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("applied", r.text(0));
    try std.testing.expectEqual(@as(?[]const u8, null), r.nullableText(1));
    try std.testing.expectEqualStrings("2026-05-19T01:00:00Z", r.text(2));

    const cur = (try conn.row(
        "select last_applied_sequence, updated_at from sync_cursors where remote_name = 'primary'",
        .{},
    )) orelse return error.ExpectedRow;
    defer cur.deinit();
    try std.testing.expectEqual(@as(i64, 1), cur.int(0));
    try std.testing.expectEqualStrings("2026-05-19T01:00:00Z", cur.text(1));
}

test "applyMutationOutcome: pending+failure → failed records detail" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"x\",\"body\":\"\"}",
        .state = "pending",
    });

    const detail = try gpa.dupe(u8, "permission denied");
    defer gpa.free(detail);
    try applyMutationOutcome(
        conn,
        gpa,
        1,
        .{ .failure = .{ .detail = detail } },
        "2026-05-19T02:00:00Z",
        null,
    );

    const r = (try conn.row(
        "select state, failure_json, state_changed_at from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("failed", r.text(0));
    try std.testing.expectEqualStrings("{\"detail\":\"permission denied\"}", r.text(1));
    try std.testing.expectEqualStrings("2026-05-19T02:00:00Z", r.text(2));

    // Sync Cursor must NOT advance on failure.
    const cur = (try conn.row(
        "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
        .{},
    )) orelse return error.ExpectedRow;
    defer cur.deinit();
    try std.testing.expectEqual(@as(i64, 0), cur.int(0));
}

test "applyMutationOutcome: failed+success → applied, advances Sync Cursor" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"x\",\"body\":\"\"}",
        .state = "failed",
        .failure_json = "{\"detail\":\"old\"}",
    });

    try applyMutationOutcome(conn, gpa, 1, .{ .success = .{} }, "2026-05-19T03:00:00Z", null);

    const r = (try conn.row(
        "select state, failure_json from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("applied", r.text(0));
    try std.testing.expectEqual(@as(?[]const u8, null), r.nullableText(1));

    const cur = (try conn.row(
        "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
        .{},
    )) orelse return error.ExpectedRow;
    defer cur.deinit();
    try std.testing.expectEqual(@as(i64, 1), cur.int(0));
}

test "applyMutationOutcome: failed+failure refreshes detail without state change" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"x\",\"body\":\"\"}",
        .state = "failed",
        .failure_json = "{\"detail\":\"old\"}",
    });

    const detail = try gpa.dupe(u8, "still broken");
    defer gpa.free(detail);
    try applyMutationOutcome(
        conn,
        gpa,
        1,
        .{ .failure = .{ .detail = detail } },
        "2026-05-19T04:00:00Z",
        null,
    );

    const r = (try conn.row(
        "select state, failure_json, state_changed_at from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("failed", r.text(0));
    try std.testing.expectEqualStrings("{\"detail\":\"still broken\"}", r.text(1));
    try std.testing.expectEqualStrings("2026-05-19T04:00:00Z", r.text(2));

    const cur = (try conn.row(
        "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
        .{},
    )) orelse return error.ExpectedRow;
    defer cur.deinit();
    try std.testing.expectEqual(@as(i64, 0), cur.int(0));
}

test "markMutationSkipped: failed → skipped" {
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"x\",\"body\":\"\"}",
        .state = "failed",
        .failure_json = "{\"detail\":\"boom\"}",
    });

    try markMutationSkipped(conn, 1, "2026-05-19T05:00:00Z", null);

    const r = (try conn.row(
        "select state, state_changed_at from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("skipped", r.text(0));
    try std.testing.expectEqualStrings("2026-05-19T05:00:00Z", r.text(1));
}

test "markMutationSkipped: pending → MutationNotFailed" {
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"x\",\"body\":\"\"}",
        .state = "pending",
    });

    try std.testing.expectError(
        error.MutationNotFailed,
        markMutationSkipped(conn, 1, "2026-05-19T05:00:00Z", null),
    );

    const r = (try conn.row("select state from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("pending", r.text(0));
}

test "markMutationSkipped: missing sequence → MutationNotFound" {
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try std.testing.expectError(
        error.MutationNotFound,
        markMutationSkipped(conn, 42, "2026-05-19T05:00:00Z", null),
    );
}

test "setRemote then getRemote: round-trip and replace preserves created_at" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try setRemote(conn, "github", "{\"owner\":\"o\",\"repo\":\"r\"}", "2026-05-19T00:00:00Z", null);

    {
        const got = (try getRemote(conn, gpa)) orelse return error.ExpectedRemote;
        defer got.deinit(gpa);
        try std.testing.expectEqualStrings("github", got.backend_kind);
        try std.testing.expectEqualStrings("{\"owner\":\"o\",\"repo\":\"r\"}", got.config_json);
        try std.testing.expectEqual(@as(i64, 0), got.last_applied_sequence);
    }

    const meta1 = (try conn.row(
        "select created_at, updated_at from remotes where name = 'primary'",
        .{},
    )) orelse return error.ExpectedRow;
    const created_at_1 = try gpa.dupe(u8, meta1.text(0));
    defer gpa.free(created_at_1);
    meta1.deinit();
    try std.testing.expectEqualStrings("2026-05-19T00:00:00Z", created_at_1);

    // Replace: updated_at advances, created_at is preserved, Sync Cursor untouched.
    try setRemote(conn, "jira", "{\"project\":\"PROJ\"}", "2026-05-19T06:00:00Z", null);

    {
        const got = (try getRemote(conn, gpa)) orelse return error.ExpectedRemote;
        defer got.deinit(gpa);
        try std.testing.expectEqualStrings("jira", got.backend_kind);
        try std.testing.expectEqualStrings("{\"project\":\"PROJ\"}", got.config_json);
        // Sync Cursor is left as-is on update; the existing row still says 0.
        try std.testing.expectEqual(@as(i64, 0), got.last_applied_sequence);
    }

    const meta2 = (try conn.row(
        "select created_at, updated_at from remotes where name = 'primary'",
        .{},
    )) orelse return error.ExpectedRow;
    defer meta2.deinit();
    try std.testing.expectEqualStrings("2026-05-19T00:00:00Z", meta2.text(0));
    try std.testing.expectEqualStrings("2026-05-19T06:00:00Z", meta2.text(1));
}

test "getRemote: returns null when no Remote configured" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try std.testing.expectEqual(@as(?RemoteRow, null), try getRemote(conn, gpa));
}

test "clearRemote: deletes both rows; no-op when neither exists" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    // No-op when nothing is configured.
    try clearRemote(conn, null);

    try setRemote(conn, "github", "{\"owner\":\"o\",\"repo\":\"r\"}", "2026-05-19T00:00:00Z", null);
    try clearRemote(conn, null);

    try std.testing.expectEqual(@as(?RemoteRow, null), try getRemote(conn, gpa));

    const remotes_count = (try conn.row("select count(*) from remotes", .{})) orelse return error.ExpectedRow;
    defer remotes_count.deinit();
    try std.testing.expectEqual(@as(i64, 0), remotes_count.int(0));

    const cursors_count = (try conn.row("select count(*) from sync_cursors", .{})) orelse return error.ExpectedRow;
    defer cursors_count.deinit();
    try std.testing.expectEqual(@as(i64, 0), cursors_count.int(0));
}

test "pendingOrFailedMutationCount: 0, 1 pending, 1 failed, 0 after marking skipped" {
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try std.testing.expectEqual(@as(i64, 0), try pendingOrFailedMutationCount(conn));

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"x\",\"body\":\"\"}",
        .state = "pending",
    });
    try std.testing.expectEqual(@as(i64, 1), try pendingOrFailedMutationCount(conn));

    try conn.exec(
        \\update mutations
        \\   set state = 'failed', failure_json = '{"detail":"x"}'
        \\ where sequence = 1
    , .{});
    try std.testing.expectEqual(@as(i64, 1), try pendingOrFailedMutationCount(conn));

    try markMutationSkipped(conn, 1, "2026-05-19T07:00:00Z", null);
    try std.testing.expectEqual(@as(i64, 0), try pendingOrFailedMutationCount(conn));
}

test "pendingOrFailedMutationCount: counts both pending and failed simultaneously" {
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "pending",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 2,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"B\",\"body\":\"\"}",
        .state = "failed",
        .failure_json = "{\"detail\":\"x\"}",
    });

    try std.testing.expectEqual(@as(i64, 2), try pendingOrFailedMutationCount(conn));
}

test "applyMutationOutcome: applied prior returns MutationNotApplicable, state and cursor unchanged" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
        .last_applied_sequence = 0,
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "applied",
    });

    try std.testing.expectError(
        error.MutationNotApplicable,
        applyMutationOutcome(conn, gpa, 1, .{ .success = .{} }, "2026-05-19T00:00:00Z", null),
    );

    // State still 'applied', cursor still 0.
    const state_row = (try conn.row("select state from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer state_row.deinit();
    try std.testing.expectEqualStrings("applied", state_row.text(0));
    const cursor_row = (try conn.row("select last_applied_sequence from sync_cursors where remote_name = 'primary'", .{})) orelse return error.ExpectedRow;
    defer cursor_row.deinit();
    try std.testing.expectEqual(@as(i64, 0), cursor_row.int(0));
}

test "applyMutationOutcome: skipped prior returns MutationNotApplicable" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "skipped",
    });

    try std.testing.expectError(
        error.MutationNotApplicable,
        applyMutationOutcome(conn, gpa, 1, .{ .success = .{} }, "2026-05-19T00:00:00Z", null),
    );

    const row = (try conn.row("select state from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("skipped", row.text(0));
}

test "applyMutationOutcome: missing sequence returns MutationNotFound" {
    const gpa = std.testing.allocator;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try std.testing.expectError(
        error.MutationNotFound,
        applyMutationOutcome(conn, gpa, 999, .{ .success = .{} }, "2026-05-19T00:00:00Z", null),
    );

    // Connection still usable (no dangling transaction).
    const row = (try conn.row("select count(*) from mutations", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqual(@as(i64, 0), row.int(0));
}

test "applyMutationOutcome: failure_json round-trips non-UTF-8 detail" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "pending",
    });

    // Lone 0xFF byte plus a backslash would otherwise break json_valid.
    const detail = try gpa.dupe(u8, &[_]u8{ 'b', 'a', 'd', 0xFF, '\\' });
    defer gpa.free(detail);

    try applyMutationOutcome(
        conn,
        gpa,
        1,
        .{ .failure = .{ .detail = detail } },
        "2026-05-19T00:00:00Z",
        null,
    );

    // Row transitioned to failed and failure_json passes json_valid.
    const row = (try conn.row(
        "select state, json_valid(failure_json) from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("failed", row.text(0));
    try std.testing.expectEqual(@as(i64, 1), row.int(1));
}

test "mergeBackendSnapshots: mid-loop collision rolls back successful insert" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    // Local item already claims `gh-2` — the second snapshot will collide.
    try TmpStore.insertFixtureItem(conn, .{
        .id = "local",
        .display = "gh-2",
        .title = "Local",
        .created_seq = 1,
    });
    try conn.exec("update sequences set value = 1 where name = 'item_created_seq'", .{});

    const snapshots = [_]BackendItemSnapshot{
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
        .{
            .backend_kind = "github",
            .backend_key = "2",
            .display_id = "gh-2",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "Second",
            .body = "",
            .status = .open,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };

    var diag: Diagnostic = .{};
    try std.testing.expectError(
        error.DisplayIdCollision,
        mergeBackendSnapshots(conn, gpa, prng.random(), &snapshots, "2026-05-19T00:00:00Z", &diag),
    );

    // The first snapshot's INSERT was rolled back along with the second
    // snapshot's constraint failure — only the pre-existing local item remains.
    const count_row = (try conn.row("select count(*) from items", .{})) orelse return error.ExpectedRow;
    defer count_row.deinit();
    try std.testing.expectEqual(@as(i64, 1), count_row.int(0));

    // The created_seq counter should be unchanged after rollback.
    const seq_row = (try conn.row("select value from sequences where name = 'item_created_seq'", .{})) orelse return error.ExpectedRow;
    defer seq_row.deinit();
    try std.testing.expectEqual(@as(i64, 1), seq_row.int(0));
}

test "mergeBackendSnapshots: applied or skipped mutations do not block overwrite" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Old",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try conn.exec("update sequences set value = 1 where name = 'item_created_seq'", .{});

    // applied mutation does not block.
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"x\",\"body\":\"\"}",
        .state = "applied",
    });
    // skipped mutation does not block.
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 2,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"y\",\"body\":\"\"}",
        .state = "skipped",
    });

    const snapshots = [_]BackendItemSnapshot{
        .{
            .backend_kind = "github",
            .backend_key = "1",
            .display_id = "gh-1",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "New Title From Backend",
            .body = "New body",
            .status = .done,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };

    try mergeBackendSnapshots(conn, gpa, prng.random(), &snapshots, "2026-05-19T00:00:00Z", null);

    const row = (try conn.row(
        "select title, body, status from items where id = 't1'",
        .{},
    )) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("New Title From Backend", row.text(0));
    try std.testing.expectEqualStrings("New body", row.text(1));
    try std.testing.expectEqualStrings("done", row.text(2));
}

test "loadApplicableMutations: unknown mutation_type returns MutationTypeUnknown" {
    const gpa = std.testing.allocator;
    const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });

    // Schema CHECK normally rejects unknown mutation_type values; bypass it
    // to exercise the schema-drift error path.
    try conn.execNoArgs("pragma ignore_check_constraints = on");
    defer conn.execNoArgs("pragma ignore_check_constraints = off") catch {};
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "not_a_real_type",
        .item_id = "t1",
        .payload_json = "{}",
        .state = "pending",
    });

    try std.testing.expectError(
        error.MutationTypeUnknown,
        loadApplicableMutations(conn, gpa),
    );
}
