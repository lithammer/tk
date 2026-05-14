# Implementation Plan

This document bridges the design docs to the Zig implementation. It describes
the current implementation baseline first, then the next vertical slices. Keep
completed slices as durable contracts here, not as historical task checklists.

## Inputs

- Domain language: [../CONTEXT.md](../CONTEXT.md)
- CLI surface: [cli.md](./cli.md)
- Design decisions: [adr/](./adr/)
- Resolved questions: [design-questions.md](./design-questions.md)
- Prime command output: [../src/commands/prime.md](../src/commands/prime.md)

## Code Shape

Current module layout:

```text
src/
  main.zig
  cli.zig
  clock.zig
  messages.zig
  commands/
    prime.zig
    prime.md
    init.zig
    add.zig
    message.zig
  domain/
    display_prefix.zig
    item_class.zig
    origin.zig
    priority.zig
    status.zig
    ticket_kind.zig
  git/
    discovery.zig
  proc/
    runner.zig
    fake.zig
  store/
    diagnostic.zig
    migrations.zig
    repository.zig
  testing/
    arg_iter.zig
    scenarios.zig
    script.zig
    smoke.zig
    test_cli.zig
    tmp_store.zig
    txtar.zig
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

`src/store/repository.zig` owns the reusable Repository Store opener and the
first store-facing write API, `createLocalTicket`. `src/commands/message.zig`
owns git-commit-style message input syntax; the parsed title and body are
domain fields, but the syntax is a command-input concern.

Only add files when the slice needs them. Future modules such as
`commands/list.zig`, `commands/next.zig`, `worktree/`, `sync/`, and `remote/`
land with the slice that first needs them.

## Boundaries

`main.zig` is a thin process shim: it builds a `Deps` struct from `std.process.Init` (Zig 0.16's "juicy main"), calls `runArgv(deps, args_iter) !u8`, catches any propagated error and translates it to exit code 3, then translates the returned code into the process exit code. Nothing else.

The Writers in `Deps` are pre-bound interface pointers: `main.zig` constructs `std.Io.File.stdout().writer(io, &buf)` and `std.Io.File.stderr().writer(io, &buf)` on its stack and hands `&fw.interface` to `Deps`. Lifetime contract: `runArgv` and command handlers must not retain the pointers past return. Tests use `std.Io.Writer.Allocating` and pass `&allocating.writer`.

`cli.zig` exports `runArgv(deps, args_iter: anytype) !u8`. It owns the top-level subcommand routing: a `SubCommand` enum, a tiny zig-clap spec for the top-level (`--help`, `--version`, `<command>`) using `terminating_positional`, and a switch that dispatches to each command's `run`. The `args_iter` parameter is `anytype` so the same dispatcher accepts `*std.process.Args.Iterator` from `main.zig` and a custom slice iterator from tests; this matches zig-clap's own convention. `cli.zig` does not perform filesystem, SQLite, git, or subprocess work.

Each `commands/<cmd>.zig` owns its zig-clap parameter spec and its `run(deps, args_iter: anytype) !u8` handler. Per-command parsing keeps each command's flags co-located with its handler. Each command also exports a `pub const meta: cli.CommandMeta = .{ .name = ..., .description = ... }`.

Adding a command is a single touchpoint. `cli.zig` holds an `all_commands` tuple of imported command modules; the `SubCommand` enum (via Zig 0.16's `@Enum` builtin), the dispatch switch (via `inline else` over the enum), the `Commands:` section in `tk --help`, and zig-clap's enumeration parser are all derived from that tuple at compile time. Creating `commands/<new>.zig` with `meta` and `run`, then adding `@import("commands/<new>.zig")` to `all_commands`, is enough to pick the new command up everywhere. Compile errors are early and specific if a module forgets `meta` or `run`.

Argument parsing uses [zig-clap](https://github.com/Hejsil/zig-clap). Hand-rolled parsing was considered but zig-clap covers our spec quirks (repeatable `-m`, named positionals, `--key=value`) and stays out of the boundary above (it returns typed structs; it does not own dispatch).

Command handlers execute parsed commands against explicit dependencies (`Deps`):

- `stdout: *std.Io.Writer`, `stderr: *std.Io.Writer`,
  `stdin: *std.Io.Reader`
- `gpa: std.mem.Allocator`, `io: std.Io`, `cwd: std.Io.Dir`
- injectable subprocess runner for Git common-dir discovery
- injectable UTC millisecond clock
- injectable random source for opaque internal IDs

`Deps` grows additively. Future commands may add a worktree service, sync
engine, Remote adapter registry, and `tty_stdout: bool` / color-policy helpers.

Exit codes returned by `runArgv`:

- `0` — success
- `1` — logical failure surfaced to the user (e.g. `tk next` finds nothing ready)
- `2` — usage error (unknown subcommand, bad flag combo, zig-clap diagnostic)
- `3` — unexpected internal error caught at the top of `main.zig` (e.g. `error.OutOfMemory` propagated from zig-clap)

The exit-3 catch-all in `main.zig` is always in scope: zig-clap can return
`error.OutOfMemory` regardless of which command runs, so `main.zig` translates
any propagated error to exit 3. Deterministic fault-injection tests for the
catch-all are deferred until they are realistic.

Domain logic should not depend on SQLite, filesystem paths, git, or subprocess execution.

## Output

Commands write their primary output to stdout and diagnostics to stderr. `tk prime` is the primary case driven by an agent harness (e.g. Claude Code's `SessionStart` hook); its markdown body must reach stdout, never stderr. `tk prime` writes nothing to stderr, succeeds outside a `tk init`'d repo, and normalises its output to exactly one trailing newline so concatenation into an agent context window stays clean.

Each command declares its own preconditions. There is no central "needs store" gate: `tk prime` requires nothing, `tk init` creates the store, and other commands fail-fast when the store is missing.

`tk list` starts with plain ASCII output so the List Tree shape, item markers,
and filtering semantics can settle before color policy enters the rendering
path. A later output-rendering slice should introduce `--color=auto|always|never`,
a fakeable `tty_stdout: bool` on `Deps`, and `NO_COLOR` / `CLICOLOR_FORCE`
handling across every command that emits styled output.

The initial `tk list` renderer should favor simple streaming over table layout:
decorative tree glyphs are part of the output contract, but columns are not
aligned and Origin is not rendered as a separate field.

## Storage

The Repository Store uses SQLite.

`tk init` creates the Repository Store at `<git-common-dir>/tk/ticket.db`, where
`<git-common-dir>` is Git's shared common directory for the repository. This
keeps the store untracked and shared across linked worktrees instead of
creating one store per worktree. Discover `<git-common-dir>` by running
`git rev-parse --path-format=absolute --git-common-dir` through an injectable
subprocess runner rather than parsing `.git` files directly.

Commands after `tk init` use a reusable Repository Store opener rather than
duplicating discovery and validation. The opener discovers the Git common dir,
opens `<git-common-dir>/tk/ticket.db`, enables required connection pragmas,
checks the Ticket application ID and known migration version, and returns typed
outcomes for Git discovery failure, missing store, invalid store, and
future-version store. Commands render those outcomes with their own command
prefix, while shared Git discovery phrasing stays in `src/git/discovery.zig`.

The subprocess runner returns a captured result (`exit code`, `stdout`,
`stderr`) rather than streaming directly to command output. Git discovery
parses Git stdout, classifies non-zero Git exits, preserves trimmed stderr when
Git provides it, and keeps the runner fakeable in command-handler tests.

`tk init` creates the `<git-common-dir>/tk/` directory if it is missing. On
Unix, create it with private permissions (`0700`) when possible because the
Repository Store may contain local work notes, Remote configuration, and sync
failures. If the directory already exists with broader permissions, `tk init`
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

Migration 1 creates the full v1 schema skeleton rather than a metadata-only
database: `schema_migrations`, `sequences`, `items`,
`store_config`, `item_ids`, `dependencies`, `external_blockers`, `mutations`,
`remotes`, and `sync_cursors`. Later slices populate and query those tables.
Tables use SQLite `strict` mode, and
primary-key tables without integer row identity use `without rowid` where it
fits the key shape. Each Repository Store connection must enable
`PRAGMA foreign_keys = on` and `PRAGMA busy_timeout = 5000`. `tk init` sets
`PRAGMA journal_mode = wal` for the store. The implementation does not tune
`synchronous`; keep SQLite's default unless measurements or durability
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
`updated_at` are local Repository Store timestamps; backend-native
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
check(key in ('display_prefix')), value text not null)`. `tk init` seeds
`display_prefix` from the repository basename after sanitizing
it to a lowercase local-prefix shape. The stored prefix is normalized lowercase;
case-insensitive lookup is handled on full Display IDs in `item_ids`. The default
heuristic lowercases, treats underscores as separators, splits on separators and punctuation, prefers the full
joined form when it is at most 12 characters, otherwise prefers the first two
words joined by `-` when that fits, and otherwise truncates the sanitized
basename to 12 characters. It does not strip vowels. If the result is empty or
starts with a digit, prefix it with `tk-`. Only the stored `display_prefix` is
compatibility-sensitive; the derivation algorithm may change later without
affecting existing stores. `tk init` is currently flagless; a future
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
containment cycles. The v1 schema does not need a containment cycle trigger
because v1 containment is structurally limited to Epic -> Ticket.

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
The current schema includes External Blockers, but the CLI for creating and
resolving them is deferred to the lifecycle/blocking slice so that command
surface can be designed separately. Once that CLI lands,
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
indexes on JSON payloads until a concrete query path needs them.

Timestamp fields store UTC millisecond strings in the format
`YYYY-MM-DDTHH:MM:SS.sssZ`. Commands generate timestamps through an injectable
clock rather than SQLite defaults so tests and sync behavior remain
deterministic.

Current Ticket and Epic state is stored directly. The Mutation Log is an outbox
for replayable backend intent, not the primary read model and not a general
audit log of every local edit.

Any command that changes backend-backed state must update current state and
append the corresponding Mutation in one SQLite transaction. Pre-Promotion
changes to Local Tickets and Local Epics update only current Repository Store
state; Promotion snapshots the current local state when converting the item in
place.

Repository Store writes use `BEGIN IMMEDIATE` by default. Write commands know
they will write, so they should acquire the write lock before reading sequence
values or making dependent state changes. Read-only commands can use ordinary
reads or explicit read transactions when their query shape needs them. The
current `createLocalTicket` helper opens `BEGIN IMMEDIATE`, allocates the
Display ID and creation-order sequences, inserts `items` and `item_ids`, and
commits the transaction. Extract a shared write-transaction helper once a
second write path needs the same shape; do not add nested transactions.

A single command may append more than one Mutation, of different Mutation Types, in the same transaction. For example, `tk update --parent` edits Epic membership through `add_ticket_to_epic` (and `remove_ticket_from_epic` when moving between Epics), while editing title/body through `update_ticket` in the same call. Command-to-Mutation is many-to-one, not one-to-one.

Workspace Scope is not stored in SQLite in v1. It is stored in git worktree config.

## IDs

Store items by an internal stable ID. Internal IDs are random opaque values,
not sequential counters. The first implementation uses 128-bit random values
encoded as lowercase hex. `created_seq` handles creation ordering, and Display
IDs and Aliases are lookup keys.

Local Display IDs use one stored Repository Store prefix plus one shared
sequence for Tickets and Epics: `<store-prefix>-<n>`. The prefix identifies the
local repository context, not the item's class. `tk init` stores the prefix in
the Repository Store; the default is derived from the repository
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
- txtar-based CLI scenario tests with a small script runner inspired by
  `rsc.io/script` and Rust's `trycmd`. Each scenario file has a `-- script --`
  section with one or more `tk` invocations plus `-- expected/stdout --`,
  `-- expected/stderr --`, and `-- expected/exit --` sections. Scenarios run
  in-process against a synthesized `Deps` struct.
- Subprocess smoke tests against the linked binary for argv wiring, embedded
  payloads, Git subprocess discovery, filesystem writes, and SQLite linkage.

In-process scenarios cover dispatch, output formatting, and exit codes. They
cannot detect a bug in the embed wiring of the linked exe, because the test
binary embeds the same bytes via the same module import — the assertion is
structurally tautological. Subprocess smoke tests are the dedicated check for
linked-executable behavior.

Current coverage includes:

- `tk prime` scenario coverage, top-level dispatch tests, and exit-code tests.
- `tk init` command tests for temp git repositories, idempotent init,
  outside-git failure, and invalid existing stores.
- migration tests for table creation, `application_id`, `user_version`, WAL,
  foreign-key enforcement, and representative schema constraints.
- `tk add` command/store tests for file and stdin input, message parsing,
  missing-store and Git diagnostics, local Ticket state, Display ID allocation,
  `item_ids`, store-busy diagnostics, and the no-Mutation invariant.
- subprocess smoke tests for real `tk init` and real `tk add` after `git init`.

Avoid testing everything through subprocess CLI scenarios. Keep most behavior fast and local to domain or command-handler tests.

Fixture wiring uses per-test `@embedFile` so a missing fixture is a build error. If the scenario set outgrows manual test functions (roughly 30–50 fixtures), introduce a `build.zig` step that walks `tests/scenarios/` and generates a Zig manifest the runner iterates over.

The `-- script --` tokenizer follows [`rogpeppe/go-internal/testscript`](https://github.com/rogpeppe/go-internal/blob/master/testscript/testscript.go) (the maintained successor to `rsc.io/script`): whitespace splits args, `'…'` quotes a literal chunk, `''` inside a quoted chunk is a literal `'`, `#` starts a comment, and `$NAME` / `${NAME}` expand against the runner's env map. No double quotes, no backslash escapes. Output comparison is byte-exact — no whitespace normalisation. Setting `TK_UPDATE=1` rewrites the `expected/*` sections of each fixture in place, preserving section order and any non-expected sections (like `-- input/foo.md --`).

Each scenario gets a fresh per-script work directory exposed as `$WORK` in the
env map (matching testscript). The runner also passes a `std.Io.Dir` handle for
`$WORK` via `Deps.cwd`. The work directory is removed unconditionally after the
scenario; setting `TK_TESTWORK=1` opts in to preserving it for debugging.
Failure output substitutes the literal string `$WORK` for the actual path so
messages stay readable; re-run with `TK_TESTWORK=1` to keep artefacts.

The scenario runner supports testscript-compatible `stdin <source>`
directives. `stdin <path>` reads a file from the scenario work directory, so
existing `-- input/... --` sections can feed the next `tk` command. `stdin
stdout` and `stdin stderr` feed the aggregate stdout or stderr captured so far
in the script, matching Go testscript parity. The stdin payload applies to the
next `tk` command only and then resets to empty. Do not add inline heredoc
syntax. If a scenario sets stdin and no later `tk` command consumes it, fail
the scenario with `script: stdin set but no tk command consumed it`. If a
scenario sets stdin again before it is consumed, fail with `script: stdin
already set` rather than silently replacing the pending payload. If `stdin
<path>` names a missing file, fail immediately with `script: stdin source not
found: <path>`; do not convert a missing fixture into empty stdin.

## Completed Baseline

The current binary implements `tk prime`, `tk init`, and
`tk add -F <file | ->`.

`tk prime` is command-owned static output embedded from
`src/commands/prime.md`. It trims trailing whitespace, emits exactly one final
newline to stdout, writes nothing to stderr on success, and works outside an
initialized Repository Store.

`tk init` is flagless except `--help`. It discovers Git's common directory
through the injectable subprocess runner, creates `<git-common-dir>/tk/`, opens
`ticket.db`, classifies fresh/ours/foreign stores before mutating them, applies
migrations, seeds `store_config.display_prefix`, and writes one stdout status
line. Shared Git discovery diagnostics are rendered through
`src/git/discovery.zig`, with the caller supplying the command name. `tk init`
is idempotent for current Ticket stores and refuses non-Ticket SQLite files or
stores from a newer migration version.

`tk add` accepts exactly one `-F` / `--file` source. `-F -` and `--file -` read
stdin; `-F <file>`, `--file <file>`, and `--file=<file>` read from `Deps.cwd`.
The command reads the whole payload, parses it as git-commit-style message
input, rejects empty titles and NUL bytes, opens the existing Repository Store,
then creates one Local Ticket. Input errors surface before Git discovery or
SQLite precondition errors.

`src/commands/message.zig` owns message parsing. It normalizes CRLF and CR to
LF, trims leading and trailing blank lines from the whole message, trims and
folds title lines with single spaces, and preserves body text after trimming
outer blank lines. It returns allocator-owned title/body slices in
`ParsedMessage`; callers map parser errors to command diagnostics.

`store.repository.createLocalTicket` is the first store-facing write API. It
takes typed domain inputs, generates a 128-bit lowercase-hex internal ID from
the injected random source, calls the injected clock once, uses `BEGIN
IMMEDIATE`, allocates `display_seq` and `item_created_seq` inside the
transaction, inserts `items` and `item_ids`, and commits. It creates a local
task Ticket with Priority `P2`, Item Status `open`, `origin = 'local'`, and no
backend identity. It leaves `mutations` unchanged and does not advance
`mutation_seq`; Promotion is the first backend-intent boundary. Git discovery
failures and Repository Store precondition failures are represented separately
so `tk add` can report outside-git as a Git problem and missing `ticket.db` as
an initialization problem.

`tk add` success output is:

```text
Created Ticket: <display-id> - <title>
Priority: P2
Status: open
```

All three lines are flush-left and ASCII. Stable user-observable strings live
in `src/messages.zig`, including missing-store, empty-message, NUL-message,
file-read, stdin-read, generic create-failure, store-busy retry, and success
labels. SQLite Busy/Locked error tags render as
`tk add: Repository Store is busy; retry the command`.

The command surface intentionally remains narrow: no `--bug`, `--epic`,
`--priority`, `--parent`, repeatable `-m`, or editor mode yet.

## Next Slices

Continue in small vertical slices:

1. `tk list`
   - Render the List Tree for local Tickets and Epics.
   - Query the real Repository Store read model even though the current public
     create path only produces unparented local task Tickets.
   - Keep this slice global by default. Workspace Scope discovery, scoped
     defaults, and scoped-output labeling land with the worktree scope slice.
     When scoped list defaults land, `--all` ignores the active Workspace Scope.
     `--all` preserves the normal List Tree regardless of mixed Epic and child
     statuses.
   - Preserve the List Tree shape for filtered views such as `--ready` and
     `--blocked`, including non-empty Epics as containers.
   - Treat `--all`, `--ready`, `--blocked`, and `--active` as mutually exclusive.
   - Treat `--ready` as open Tickets only: active Tickets are already in
     progress and are not ready work.
   - Treat `--blocked` as open or active Tickets with unresolved Dependencies
     or External Blockers; Epics may render as containers but are not selected
     as blocked work in v1.
   - Treat `--active` as active Tickets and active Epics, while retaining
     inactive Epics as containers for active child Tickets.
   - Implement `--local` and `--remote` against stored Origin now, even though
     Promotion and Backend Pull are not implemented yet. Seed backend-origin
     rows directly in list tests.
   - Compose readiness and Origin filters as an AND. Container Epics must pass
     the Origin filter to render. If the Origin filter hides a containing Epic,
     render matching child Tickets as top-level rows.
   - Order top-level rows by `created_seq` ascending and order each Epic's
     child Tickets by `created_seq` ascending. Display Priority without using
     it as a `tk list` sort key.
   - Render rows with decorative tree glyphs, compact markers, and one-space
     field separation rather than width-aligned columns.
   - Use `○`, `◐`, and `✓` for `open`, `active`, and `done` Item Status,
     followed by Display ID. Ticket rows then render literal priority marker
     `●`, Priority, optional `[bug]`, and title. Epic rows render `[epic]`
     and title without Priority.
   - Render titles as-is on one line; do not truncate or wrap in this slice.
   - Do not introduce a new escaping policy in this slice. Render stored titles
     literally and leave terminal escape handling to the existing follow-up.
   - Do not render Origin as a separate row field; Local or Backend origin is
     normally inferred from the Display ID shape.
   - End output with a separator, a rendered item count by Item Status, and a
     status legend for the status glyphs. Filtered views count rendered rows,
     including container Epics retained for matching child Tickets.
   - Empty results are successful reads with exit code `0`: the default view
     prints `No open or active items.`, and filtered views print a
     filter-specific empty message.
   - `tk list --help` describes every available flag for the command. Keep row
     examples and the status legend out of help; those belong in `docs/cli.md`
     and scenario fixtures.
   - Open the Repository Store through the shared opener. Missing store,
     foreign store, and future-version store are exit code `1` logical
     precondition failures with `tk list:` diagnostics. Render Git discovery
     failures through `discovery.renderFailure(..., "list", ...)`.
   - Put stable `tk list` diagnostics and labels in `src/messages.zig`,
     including missing-store text, empty-list messages, `Total: `, and
     `Status: `. Renderer glyphs may stay local to the list renderer.
   - Add a narrow Repository Store read API for list rows in
     `src/store/repository.zig`. Push filtering and container-retention work
     into SQL where practical, including readiness, Origin filters, and Epics
     retained for matching child Tickets. Keep final tree rendering in
     `commands/list.zig`.
   - Compute ready and blocked filters from the current `dependencies` and
     `external_blockers` tables now, even though the blocking CLI lands later.
     A Dependency blocks only while its Blocking Item is not `done`. Seed those
     tables directly in list tests.
     `--blocked` matches either unresolved Dependencies or unresolved External
     Blockers on open or active Tickets; done Tickets are not blocked work.
     `--ready` requires neither blocker kind.
   - Add layered tests: store tests for SQL/filter behavior, focused
     tree-renderer tests over in-memory rows, and CLI scenario fixtures for the
     user-facing output contract. Cover Epics, child Tickets, unparented
     Tickets, statuses, kinds, origins, priorities, and filtered-out parent
     promotion by seeding store state directly; do not widen `tk add` as part
     of this slice.
   - Use raw SQL fixture helpers, likely in `src/testing/tmp_store.zig`, to seed
     Epics, backend-origin rows, Dependencies, and External Blockers for tests.
     Do not add production write APIs for those concepts before their command
     slices land.

2. `tk next`
   - Select ready Tickets by Priority and creation order within Workspace Scope.
   - Ready means Item Status `open` and no unresolved Dependencies or External
     Blockers.
   - If Workspace Scope is a Ticket, select that Ticket only when it is ready.
     If Workspace Scope is an Epic, search ready child Tickets within that Epic;
     v1 containment means direct child Tickets only. `--all` ignores Workspace
     Scope.
   - Exclude Epics.
   - Add dependency and scope tests.

3. `tk show` and `tk update`
   - Render a single Ticket or Epic with its current fields.
   - Edit title/body, local-only Priority, and Epic membership.
   - `--parent` and `--no-parent` append `add_ticket_to_epic` or `remove_ticket_from_epic` Mutations; title/body edits append `update_ticket` or `update_epic`. A single invocation may emit more than one Mutation in one transaction.

4. Lifecycle and blocking
   - Implement `tk start`, `tk stop`, `tk done`, `tk block`, and `tk unblock`.
   - Implement item-backed Dependencies and External Blocker create/resolve CLI.
   - Enforce dependency cycle rejection.
   - Add the blocked glyph to `tk list` rendering as a blocking/readiness
     overlay, not as a fourth Item Status.

5. Worktree scope
   - Implement `tk worktree`, `set`, `clear`, and `start`.
   - Use git worktree config.

6. Remote and sync skeleton
   - Implement `tk remote`.
   - Implement `tk sync log`.
   - Add fake remote adapter tests before real `gh` or `acli` behavior.

## Deferred

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
