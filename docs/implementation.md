# Implementation Plan

This document bridges the design docs to the first Zig implementation. It should stay practical and change when implementation pressure proves a better shape.

## Inputs

- Domain language: [../CONTEXT.md](../CONTEXT.md)
- CLI surface: [cli.md](./cli.md)
- Design decisions: [adr/](./adr/)
- Resolved questions: [design-questions.md](./design-questions.md)
- Prime command output: [../src/commands/prime.md](../src/commands/prime.md)

## Code Shape

Start with a small module layout:

```text
src/
  main.zig
  cli.zig
  commands/
    prime.zig
    init.zig
    add.zig
    list.zig
    next.zig
  domain/
    item.zig
    mutation.zig
    priority.zig
    status.zig
  git/
    discovery.zig
  proc/
    runner.zig
    fake.zig
  store/
    diagnostic.zig
    migrations.zig
  sync/
    engine.zig
  remote/
    github.zig
  worktree/
  testing/
    snapshot.zig
    txtar.zig
    script.zig
```

`src/git/` is a thin façade over Git subprocess invocations (`rev-parse`
path discovery is the first user; `tk worktree` will reuse it). It does
not own command-specific worktree logic — that belongs in `worktree/`
when those commands land. `src/proc/` houses the subprocess runner
abstraction and its test fakes (`FakeRunner` for scripted responses,
`ErrorInjectingRunner` for runner-error mapping). `src/store/` houses
both the Repository Store schema (migrations.zig) and the small
`Diagnostic` scratch buffer used to capture transient SQLite errors
across rollback boundaries.

Only add files when the slice needs them. The layout is a direction, not a scaffolding checklist.

## Boundaries

`main.zig` is a thin process shim: it builds a `Deps` struct from `std.process.Init` (Zig 0.16's "juicy main"), calls `runArgv(deps, args_iter) !u8`, catches any propagated error and translates it to exit code 3, then translates the returned code into the process exit code. Nothing else.

The Writers in `Deps` are pre-bound interface pointers: `main.zig` constructs `std.Io.File.stdout().writer(io, &buf)` and `std.Io.File.stderr().writer(io, &buf)` on its stack and hands `&fw.interface` to `Deps`. Lifetime contract: `runArgv` and command handlers must not retain the pointers past return. Tests use `std.Io.Writer.Allocating` and pass `&allocating.writer`.

`cli.zig` exports `runArgv(deps, args_iter: anytype) !u8`. It owns the top-level subcommand routing: a `SubCommand` enum, a tiny zig-clap spec for the top-level (`--help`, `--version`, `<command>`) using `terminating_positional`, and a switch that dispatches to each command's `run`. The `args_iter` parameter is `anytype` so the same dispatcher accepts `*std.process.Args.Iterator` from `main.zig` and a custom slice iterator from tests; this matches zig-clap's own convention. `cli.zig` does not perform filesystem, SQLite, git, or subprocess work.

Each `commands/<cmd>.zig` owns its zig-clap parameter spec and its `run(deps, args_iter: anytype) !u8` handler. Per-command parsing keeps each command's flags co-located with its handler. Each command also exports a `pub const meta: cli.CommandMeta = .{ .name = ..., .description = ... }`.

Adding a command is a single touchpoint. `cli.zig` holds an `all_commands` tuple of imported command modules; the `SubCommand` enum (via Zig 0.16's `@Enum` builtin), the dispatch switch (via `inline else` over the enum), the `Commands:` section in `tk --help`, and zig-clap's enumeration parser are all derived from that tuple at compile time. Creating `commands/<new>.zig` with `meta` and `run`, then adding `@import("commands/<new>.zig")` to `all_commands`, is enough to pick the new command up everywhere. Compile errors are early and specific if a module forgets `meta` or `run`.

Argument parsing uses [zig-clap](https://github.com/Hejsil/zig-clap). Hand-rolled parsing was considered but zig-clap covers our spec quirks (repeatable `-m`, named positionals, `--key=value`) and stays out of the boundary above (it returns typed structs; it does not own dispatch).

Command handlers execute parsed commands against explicit dependencies (`Deps`):

- `stdout: *std.Io.Writer`, `stderr: *std.Io.Writer`, `gpa: std.mem.Allocator` (slice #1)
- `cwd: std.fs.Dir` and an injectable subprocess runner for Git common-dir
  discovery (slice #2)
- An injectable clock for UTC millisecond timestamps (slice #2)
- Repository Store
- Worktree service
- Sync engine
- Remote adapter registry
- `tty_stdout: bool` and color-policy helpers (introduced when the first colored command lands; see Output)

`Deps` grows additively. Slice #1 ships only `stdout`/`stderr`/`gpa`; later slices add fields as the commands they introduce need them.

Exit codes returned by `runArgv`:

- `0` — success
- `1` — logical failure surfaced to the user (e.g. `tk next` finds nothing ready)
- `2` — usage error (unknown subcommand, bad flag combo, zig-clap diagnostic)
- `3` — unexpected internal error caught at the top of `main.zig` (e.g. `error.OutOfMemory` propagated from zig-clap)

The exit-3 catch-all in `main.zig` is in scope from slice #1, not deferred — zig-clap can return `error.OutOfMemory` regardless of which command runs, so `main.zig` must translate any propagated error to exit 3 from day one. Deterministic fault-injection tests for the catch-all are deferred until they are realistic.

Domain logic should not depend on SQLite, filesystem paths, git, or subprocess execution.

## Output

Commands write their primary output to stdout and diagnostics to stderr. `tk prime` is the primary case driven by an agent harness (e.g. Claude Code's `SessionStart` hook); its markdown body must reach stdout, never stderr. `tk prime` writes nothing to stderr, succeeds outside a `tk init`'d repo, and normalises its output to exactly one trailing newline so concatenation into an agent context window stays clean.

Each command declares its own preconditions. There is no central "needs store" gate: `tk prime` requires nothing, `tk init` creates the store, and other commands fail-fast when the store is missing.

The first colored command (`tk list` in slice #4) introduces a `--color=auto|always|never` flag (`auto` is the default), a `tty_stdout: bool` on `Deps` set from `init` and fakeable in tests, and honours `NO_COLOR` and `CLICOLOR_FORCE`. Slice #1 adds none of this — `tk prime` emits raw markdown unconditionally.

## Storage

The Repository Store uses SQLite.

`tk init` creates the Repository Store at `<git-common-dir>/tk/ticket.db`, where
`<git-common-dir>` is Git's shared common directory for the repository. This
keeps the store untracked and shared across linked worktrees instead of
creating one store per worktree. Discover `<git-common-dir>` by running
`git rev-parse --path-format=absolute --git-common-dir` through an injectable
subprocess runner rather than parsing `.git` files directly.

The subprocess runner returns a captured result (`exit code`, `stdout`,
`stderr`) rather than streaming directly to command output. `tk init` parses
Git stdout, maps non-zero Git exit into its own diagnostic, and keeps the runner
fakeable in command-handler tests.

`tk init` creates the `<git-common-dir>/tk/` directory if it is missing. On
Unix, create it with private permissions (`0700`) when possible because the
Repository Store may contain local work notes, Remote configuration, and sync
failures. If the directory already exists with broader permissions, slice #2
uses it as-is and does not chmod it.

`tk init` requires a git repository in v1. Outside a git repository, it returns
exit code 1 and writes a diagnostic to stderr. Non-git Repository Store
discovery is deferred with non-git Workspace Scope storage.
It does not require commits, a clean working tree, a configured Git remote, a
default branch, or Git user identity; an empty `git init` repository is enough.

`tk init` is idempotent. If the Repository Store already exists and is current
or can be migrated, it returns exit code 0 and does not recreate the database.
If the target file exists but is not a valid Ticket SQLite store, it returns
exit code 1 with a diagnostic instead of replacing it.

On success, `tk init` writes `Initialized Repository Store at <path>` to
stdout. When the store already exists, it writes
`Repository Store already initialized at <path>` to stdout.

Slice #2 creates the full v1 schema skeleton rather than a metadata-only
database. Migration 1 creates `schema_migrations`, `sequences`, `items`,
`store_config`, `item_ids`, `dependencies`, `external_blockers`, `mutations`,
`remotes`, and `sync_cursors`. Later slices populate and query those tables.
Tables use SQLite `strict` mode, and
primary-key tables without integer row identity use `without rowid` where it
fits the key shape. Each Repository Store connection must enable
`PRAGMA foreign_keys = on` and `PRAGMA busy_timeout = 5000`. `tk init` sets
`PRAGMA journal_mode = wal` for the store. Slice #2 does not tune
`synchronous`; keep SQLite's default unless later measurements or durability
requirements justify changing it.

Migrations are tracked in `schema_migrations(version integer primary key,
applied_at text not null)`. `PRAGMA user_version` mirrors the latest applied
migration for quick inspection. Migration 1 sets `PRAGMA application_id =
0x544B4442` (`TKDB`) so an existing SQLite file can be identified as a Ticket
Repository Store. `tk init` checks the application ID when opening an existing
store and fails rather than replacing a non-Ticket SQLite database. `tk init`
applies missing migrations in ascending order inside transactions. If the store
records a future migration version newer than the binary knows, `tk init`
returns exit code 1 and reports that the store was created by a newer Ticket
version.

Current Ticket and Epic state lives in one `items` table. The table uses
`id text primary key` as the internal stable ID, `display_value text not null
collate nocase` as the current Display ID, and `item_class text not null
check(item_class in ('ticket','epic'))` to distinguish Tickets from Epics. It
stores nullable
`ticket_kind text check(ticket_kind in ('task','bug'))`, nullable `priority text
check(priority in ('P0','P1','P2','P3','P4'))`, `title text not null
check(length(title) > 0)`, `body text not null default ''`, `container_id text`,
`container_class text`, `origin text not null check(origin in
('local','backend'))`, nullable `backend_kind text`, nullable `backend_key
text`, `created_seq integer not null unique`, `created_at text not null`, and
`updated_at text not null`. `status text not null check(status in
('open','active','done'))` stores the shared Item Status for both Tickets and
Epics. Table checks require `ticket_kind` and `priority` for Tickets, and
require both to be null for Epics. Origin constraints require
local items to have no backend identity and backend items to have both
`backend_kind` and `backend_key`. A unique constraint on `(backend_kind,
backend_key)` prevents two live items from representing the same Backend item.
`created_seq` is the deterministic creation-order source for `tk next`;
timestamps are metadata for display, debugging, and backend import. Every item
gets a `created_seq` when it first enters the Repository Store; for backend
pulls this is import order, not backend-native creation time. `created_at` and
`updated_at` are local Repository Store timestamps in slice #2; backend-native
created/updated timestamps are deferred until Backend Pull has a concrete
reader for them. `items.updated_at` changes when the item's current fields
change from the user's point of view, including title/body, status, priority,
Promotion identity, or containment. It does not change for incoming
Dependencies, Alias additions that do not change the current Display ID, or
Mutation state changes.
`body` accepts arbitrary non-null text, including an empty string. Message
normalization happens at command input boundaries such as `tk add` and
`tk update`, not through schema validation. Blocking is represented separately
from Item Status; readiness checks exclude Tickets with unresolved Dependencies
or External Blockers.

Local monotonic counters live in a fixed-name `sequences` table:
`name text primary key check(name in
('item_created_seq','display_seq','mutation_seq'))` and `value integer
not null check(value >= 0)`. Migration 1 seeds all names with value `0`.
Allocation is update-only inside the caller's write transaction:
`update sequences set value = value + 1 where name = ? returning value`. A
missing sequence row is treated as store corruption rather than silently
recreated.

Repository Store configuration lives in `store_config(key text primary key
check(key in ('display_prefix')), value text not null)`. Slice #2 seeds
`display_prefix` during `tk init` from the repository basename after sanitizing
it to a lowercase local-prefix shape. The stored prefix is normalized lowercase;
case-insensitive lookup is handled on full Display IDs in `item_ids`. The default
heuristic lowercases, treats underscores as separators, splits on separators and punctuation, prefers the full
joined form when it is at most 12 characters, otherwise prefers the first two
words joined by `-` when that fits, and otherwise truncates the sanitized
basename to 12 characters. It does not strip vowels. If the result is empty or
starts with a digit, prefix it with `tk-`. Only the stored `display_prefix` is
compatibility-sensitive; the derivation algorithm may change later without
affecting existing stores. `tk init` remains flagless in slice #2; a future
`tk init --prefix` or equivalent prefix-configuration command may be added
before meaningful local IDs exist if the default proves too implicit.

Containment uses the adjacency-list columns `items.container_id` and
`items.container_class`, not a separate membership table. In v1, table
constraints require Epics to have no container and require contained Tickets to
use `container_class = 'epic'`. A composite foreign key from
`(container_id, container_class)` to `items(id, item_class)` makes SQLite
enforce that the container exists and has the recorded class. This keeps v1
Epic membership tight while leaving a future subticket migration able to relax
the constraint to allow Ticket containers and add a recursive trigger to reject
containment cycles. Slice #2 does not need a containment cycle trigger because
v1 containment is structurally limited to Epic -> Ticket.

Display IDs and Aliases are globally unique lookup keys across Tickets and
Epics. Promotion changes the visible Display ID without changing the internal
stable ID.

`item_ids` is the single resolver table for both current Display IDs and
Aliases: `value text primary key collate nocase`, `source text not null
check(source in ('display','alias'))`, `item_id text not null references
items(id) on delete restrict deferrable initially deferred`, and `created_at
text not null`. Display IDs and Aliases are ASCII identifiers; `collate nocase`
keeps lookup case-insensitive while preserving the original value spelling for
rendering. Their allowed character set is alphanumerics plus `.`, `_`, `/`,
`-`, `:`, and `#`; enforce this with a simple SQL `check` and command-level
validation.
`items.display_value` is the authoritative current Display ID for rendering,
and `items.display_source` is a generated helper column whose value is always
`'display'`. A deferred composite foreign key from
`(display_value, id, display_source)` to `item_ids(value, item_id, source)`
makes SQLite enforce that every item has a matching current Display ID resolver
row; the referenced unique key uses the same `nocase` collation. A partial
unique index on `item_ids(item_id) where source = 'display'` enforces at most
one current Display ID per item, while the deferred foreign key enforces at
least one. Promotion updates
`items.display_value`, changes the previous resolver row to `source = 'alias'`,
and inserts the backend Display ID as the new `source = 'display'` row in one
transaction. Display IDs and Aliases are reserved indefinitely; v1 has no item
delete command, and future deletion semantics must retire resolver rows rather
than freeing their values for reuse.

Dependencies use a dedicated directional edge table, not a generic
relationship table: `dependencies(blocking_id text not null references
items(id) on delete restrict, blocked_id text not null references items(id) on
delete restrict, created_at text not null, primary key(blocking_id, blocked_id),
check(blocking_id <> blocked_id))`. A trigger uses a recursive CTE to reject
dependency cycles on insert or update. The directional column names match the
domain's Blocking Item and Blocked Item concepts and avoid ambiguous
`source`/`target` relationship semantics. A Dependency is resolved when its
Blocking Item has Item Status `done`; readiness derives unresolved Dependencies
from the Blocking Item's current status rather than storing duplicate
resolution state on the edge.

External Blockers use a separate table:
`external_blockers(id text primary key, item_id text not null references
items(id) on delete restrict, reason text not null check(length(reason) > 0),
created_at text not null, resolved_at text)`. External Blocker IDs are random
opaque internal row IDs, not Display IDs. Unresolved External Blockers are those
with `resolved_at is null`; they are resolved explicitly rather than by an item
status transition. Dependencies and External Blockers intentionally live in
separate tables because Dependencies derive resolution from the Blocking Item's
status, while External Blockers store explicit resolution state.
Slice #2 includes External Blockers in the schema only. The CLI for creating
and resolving External Blockers is deferred to a later slice so the lifecycle
and blocking command surface can be designed separately. Once that CLI lands,
creating or resolving an External Blocker updates `external_blockers` and
appends `add_external_blocker` or `resolve_external_blocker` in the same
transaction.

Mutations use `sequence integer primary key`, allocated from
`sequences('mutation_seq')`, plus `mutation_type text not null` constrained to
the V1 Mutation Type set, `item_id text not null`, `item_class text not null
check(item_class in ('ticket','epic'))`, `payload_json text not null
check(json_valid(payload_json))`, `state text not null check(state in
('pending','failed','skipped','applied'))`, nullable `failure_json text
check(failure_json is null or json_valid(failure_json))`, `created_at text not
null`, and `state_changed_at text not null`. A composite foreign key from
`(item_id, item_class)` to `items(id, item_class)` keeps the mutation target
class consistent. On insert, Mutations start as `pending` with
`state_changed_at = created_at`. `failure_json` must be null for `pending` and
`applied` Mutations, must be non-null for `failed` Mutations, and may remain on
`skipped` Mutations to explain what was skipped. A successful retry clears
`failure_json` while setting state to `applied`. Retry history is deferred; if
v1 later needs attempt history, add a separate sync-attempt table instead of
adding multiple nullable transition timestamps to `mutations`.

`sync_cursors` stores per-Remote sync progress even though v1 supports only one
configured Remote. It uses `remote_name text primary key`, `backend_kind text
not null`, `last_applied_sequence integer not null default 0`, and `updated_at
text not null`; v1 uses `remote_name = 'primary'`.

`remotes` stores configured Remote state and starts empty after `tk init`. It
uses `name text primary key check(name = 'primary')`, `backend_kind text not
null check(backend_kind in ('github','jira'))`, `config_json text not null
check(json_valid(config_json))`, `created_at text not null`, and `updated_at
text not null`. `sync_cursors.remote_name` references `remotes(name)` once a
Remote is configured. `sync_cursors` also starts empty after `tk init`; a later
`tk remote set` creates the `remotes('primary')` row and its matching
`sync_cursors('primary')` row together.

Migration 1 adds only indexes tied to known v1 query paths: containment lookup
by `items.container_id`, `tk next` ordering for open Tickets by
`(priority, created_seq)`, mutation lookup by `(state, sequence)`, and
dependency lookup by both `blocked_id` and `blocking_id`, and unresolved
External Blockers by `item_id where resolved_at is null`. Do not add speculative
indexes on JSON payloads in slice #2.

Timestamp fields store UTC millisecond strings in the format
`YYYY-MM-DDTHH:MM:SS.sssZ`. Commands generate timestamps through an injectable
clock rather than SQLite defaults so tests and sync behavior remain
deterministic.

Current Ticket and Epic state is stored directly. The Mutation Log is an outbox for replayable backend intent, not the primary read model.

Any command that changes syncable state must update current state and append the corresponding Mutation in one SQLite transaction.

A single command may append more than one Mutation, of different Mutation Types, in the same transaction. For example, `tk update --parent` edits Epic membership through `add_ticket_to_epic` (and `remove_ticket_from_epic` when moving between Epics), while editing title/body through `update_ticket` in the same call. Command-to-Mutation is many-to-one, not one-to-one.

Workspace Scope is not stored in SQLite in v1. It is stored in git worktree config.

## IDs

Store items by an internal stable ID. Internal IDs are random opaque values
(for example, 128-bit random encoded as lowercase hex or URL-safe base32), not
sequential counters. `created_seq` handles creation ordering, and Display IDs
and Aliases are lookup keys.

Local Display IDs use one stored Repository Store prefix plus one shared
sequence for Tickets and Epics: `<store-prefix>-<n>`. The prefix identifies the
local repository context, not the item's class. Slice #2 stores the prefix in
the Repository Store during `tk init`; the default is derived from the repository
basename and sanitized to the Display ID character set. Local Display IDs do not
use dotted hierarchy suffixes; containment lives in `items.container_id`, not in
the Display ID string.
`display_prefix` controls generated local Display IDs only; cross-repository
local import/export semantics are not part of v1.
After Promotion, the visible Display ID becomes the adapter-defined
backend-native Display ID, and the old local Display ID remains an Alias.

This deliberately borrows Beads' useful repository-prefix idea while avoiding
Beads-style dotted child IDs such as `src-caf.1`. Display IDs should remain
stable labels; hierarchy must come from the Repository Store's containment data.

Promotion replaces the visible Display ID with the backend Display ID and keeps the old local Display ID as an Alias.

All command arguments that accept item IDs must resolve either a Display ID or an Alias.

## Remote Adapters

Remote adapters expose only:

- Backend Pull
- Mutation Apply

The sync engine owns ordering, Sync Cursors, retries, failures, skips, and conflict policy.

Adapters call external CLIs such as `gh` and `acli` through an injectable subprocess runner. Tests should use fake runners rather than real services by default.

## Worktrees

`tk worktree start <id> [path] [--no-status]` creates a Ticket Branch, creates a git worktree, and stores Workspace Scope in git worktree config.

`tk worktree` reports configured, inferred, or absent Workspace Scope.

Branch-name inference is read-only and lower precedence than worktree config.

## Testing

Use layered tests:

- Zig unit tests for pure domain behavior.
- Command-handler tests with fake stores and fake subprocess runners.
- SQLite tests with a temp database and real migrations.
- Inline snapshots for small rendered outputs.
- txtar-based CLI scenario tests with a small script runner inspired by `rsc.io/script` and Rust's `trycmd`. Each scenario file has a `-- script --` section with one or more `tk` invocations plus `-- expected/stdout --`, `-- expected/stderr --`, and `-- expected/exit --` sections. Scenarios run in-process against a synthesized `Deps` struct. Subprocess smoke tests against the linked binary land from slice #2 onward, when `build.zig` already has the build-options wiring needed to inject the binary's path.

In-process scenarios cover dispatch, output formatting, and exit codes. They cannot detect a bug in the embed wiring of the linked exe, because the test binary embeds the same bytes via the same module import — the assertion is structurally tautological. The subprocess smoke tests landing in slice #2 are the dedicated check for the linked exe's embed correctness.

Exit codes 0 and 2 are exercised in slice #1: `0` via the prime scenario, `2` via a unit test in `cli.zig` that synthesizes an iterator over `["bogus"]` and asserts the return value is 2 with non-empty stderr (no text assertion; zig-clap diagnostic format is not version-stable).

Slice #2 tests cover `tk init` in a temp git repository, an idempotent second
run, outside-git failure, and an invalid existing `ticket.db` that must not be
replaced. Migration tests assert table creation, `application_id`,
`user_version`, WAL, foreign-key enforcement, and representative constraints.
Representative SQL-level constraint tests attempt invalid writes for Epic
ticket fields, Ticket required fields, invalid statuses, contained Tickets
pointing at non-Epics, dependency self-edges and cycles, duplicate item ID
values, External Blockers without reasons, and items without a matching current
Display ID resolver row.
Slice #2 also adds one subprocess smoke test against the linked `tk` executable
in a temp git repository, covering argv wiring, Git subprocess discovery,
filesystem writes, and SQLite linkage.

Avoid testing everything through subprocess CLI scenarios. Keep most behavior fast and local to domain or command-handler tests.

Fixture wiring uses per-test `@embedFile` so a missing fixture is a build error. If the scenario set outgrows manual test functions (roughly 30–50 fixtures), introduce a `build.zig` step that walks `tests/scenarios/` and generates a Zig manifest the runner iterates over.

The `-- script --` tokenizer follows [`rogpeppe/go-internal/testscript`](https://github.com/rogpeppe/go-internal/blob/master/testscript/testscript.go) (the maintained successor to `rsc.io/script`): whitespace splits args, `'…'` quotes a literal chunk, `''` inside a quoted chunk is a literal `'`, `#` starts a comment, and `$NAME` / `${NAME}` expand against the runner's env map. No double quotes, no backslash escapes. Output comparison is byte-exact — no whitespace normalisation. Setting `TK_UPDATE=1` rewrites the `expected/*` sections of each fixture in place, preserving section order and any non-expected sections (like `-- input/foo.md --`).

Each scenario gets a fresh per-script work directory exposed as `$WORK` in the env map (matching testscript). From slice #2 onward, the runner also passes a `std.fs.Dir` handle for `$WORK` via `Deps.cwd`. The work directory is removed unconditionally after the scenario; setting `TK_TESTWORK=1` opts in to preserving it for debugging. Failure output substitutes the literal string `$WORK` for the actual path so messages stay readable; re-run with `TK_TESTWORK=1` to keep artefacts.

## First Slices

Implement in small vertical slices:

1. `tk prime`
   - `build.zig` declares the `tk` exe and fetches zig-clap. `commands/prime.zig` embeds its command-owned static output by relative path, so no anonymous import wiring is needed for Prime. Single `zig build test` target.
   - `main.zig` implements `pub fn main(init: std.process.Init) !void`, builds `Deps { stdout, stderr, gpa }` from `init.io` and `init.gpa`, iterates argv via `init.minimal.args.iterateAllocator(init.gpa)` (skipping argv[0]), calls `runArgv`, catches any propagated error and translates it to exit 3, then translates the returned `u8` into the process exit code.
   - `cli.zig` exports `runArgv(deps, args_iter: anytype) !u8` with one `SubCommand` arm, plus top-level `--help` (stdout, exit 0) and `--version` (stdout, exit 0; `const VERSION = "v0.0.1";` lives here). Unknown subcommand and zig-clap parse failure return exit 2 with the clap diagnostic on stderr. Inline test verifies routing + exit 2.
   - `commands/prime.zig` reads `@embedFile("prime.md")`, trims trailing whitespace via `std.mem.trimEnd`, writes the bytes plus exactly one `\n` to `deps.stdout`, and returns 0. Never writes to `deps.stderr`. Works outside a `tk init`'d repo. Inline test captures output via `std.Io.Writer.Allocating`.
   - `src/testing/script.zig` ships a minimal txtar runner: parse sections, tokenize each `-- script --` line testscript-style, build a `Deps` with `std.Io.Writer.Allocating`-backed writers, dispatch through `runArgv`, byte-exact compare against `expected/stdout`/`expected/stderr`/`expected/exit`. `TK_UPDATE=1` rewrites the `expected/*` sections in place, preserving section order.
   - One fixture (`scenarios/prime/basic.txtar`) covers `tk prime` end-to-end. A second fixture (`scenarios/_harness/preserve_sections.txtar`) exercises `TK_UPDATE`'s order-preservation property by including a non-`expected/*` section that must survive a rewrite. No subprocess smoke test in slice #1 — deferred to slice #2.

2. `tk init`
   - Create the Repository Store.
   - Run SQLite migrations.
   - Add temp-dir integration tests.
   - Add `tk init --help` from the command-local zig-clap spec.
   - Keep `tk init` flagless in slice #2; no `--force`, `--store`, or `--quiet`.

3. `tk add -F -`
   - Parse git-commit-style message input.
   - Create local task Tickets with Priority `P2`.
   - Append `create_ticket` Mutation in the same transaction.

4. `tk list`
   - Render the List Tree for local Tickets and Epics.
   - Preserve the List Tree shape for filtered views such as `--ready` and
     `--blocked`, including non-empty Epics as containers.
   - Add fixture tests for Epics, child Tickets, unparented Tickets, statuses, kinds, and priorities.

5. `tk next`
   - Select ready Tickets by Priority and creation order within Workspace Scope.
   - Ready means Item Status `open` and no unresolved Dependencies or External
     Blockers.
   - If Workspace Scope is a Ticket, select that Ticket only when it is ready.
     If Workspace Scope is an Epic, search ready child Tickets within that Epic;
     v1 containment means direct child Tickets only. `--all` ignores Workspace
     Scope.
   - Exclude Epics.
   - Add dependency and scope tests.

6. `tk show` and `tk update`
   - Render a single Ticket or Epic with its current fields.
   - Edit title/body, local-only Priority, and Epic membership.
   - `--parent` and `--no-parent` append `add_ticket_to_epic` or `remove_ticket_from_epic` Mutations; title/body edits append `update_ticket` or `update_epic`. A single invocation may emit more than one Mutation in one transaction.

7. Lifecycle and blocking
   - Implement `tk start`, `tk stop`, `tk done`, `tk block`, and `tk unblock`.
   - Implement item-backed Dependencies and External Blocker create/resolve CLI.
   - Enforce dependency cycle rejection.

8. Worktree scope
   - Implement `tk worktree`, `set`, `clear`, and `start`.
   - Use git worktree config.

9. Remote and sync skeleton
   - Implement `tk remote`.
   - Implement `tk sync log`.
   - Add fake remote adapter tests before real `gh` or `acli` behavior.

## Deferred

- Rewriting `man/tk.1` from the canonical CLI spec.
- Dynamic `tk prime` sections.
- Comments, labels, and assignees. Labels remain descriptive facets only and
  must not replace Priority, Ticket Kind, Epic membership, Item Status, or
  blocking concepts.
- Custom local Display ID prefix configuration, such as `tk init --prefix`,
  unless the repository-basename default proves too implicit before item
  creation lands.
- Force sync or conflict resolution.
- Multiple remotes.
- Non-git Workspace Scope storage.
- Cross-repository local import/export.
