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
    block.zig
    done.zig
    list.zig
    message.zig
    next.zig
    show.zig
    start.zig
    stop.zig
    unblock.zig
    update.zig
  domain/
    display_prefix.zig
    item_class.zig
    mutation_type.zig
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
    mutations.zig
    repository.zig
    sequences.zig
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

`src/store/repository.zig` owns the reusable Repository Store opener, the
store-facing write APIs (`createLocalTicket`, `updateItem`, `setItemStatus`,
`addDependency`, `removeDependency`), current-state read APIs for List Tree
rows, next ready Ticket selection, and single-item detail reads, and the
Display ID / Alias resolver (`resolveItemRef`, `resolveAsEpic`).
`src/store/mutations.zig` owns the typed `MutationPayload`
tagged union and the `appendMutation` outbox helper; both Repository Store
writes and future Backend Adapter callers append Mutations through it.
`src/store/sequences.zig` is a thin shared allocator for the named counters
in the `sequences` table, called by both `repository.zig` and
`mutations.zig` so neither has to depend on the other for sequence numbers.
`src/commands/message.zig` owns git-commit-style message input syntax for
both `-F` and repeatable `-m`; the parsed title and body are domain fields,
but the syntax is a command-input concern.

Only add files when the slice needs them. Future modules such as `worktree/`,
`sync/`, and `remote/` land with the slice that first needs them.

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
Promotion identity, or containment. It does not change for Dependency
additions/removals, Alias additions that do not change the current Display ID,
or Mutation state changes.
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
resolving them is deferred until that command surface has a stable way to
identify one External Blocker when several exist on the same item. Once that
CLI lands,
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

`tk worktree`, `tk worktree set <id>`, `tk worktree clear`, and
`tk worktree start <id> [path] [--no-status]` make up the v1 Workspace Scope
surface. Workspace Scope storage and discovery live in
`src/worktree/scope.zig`; per ADR 0001 the Repository Store remains untracked,
and per the v1 Worktree Config decision the scope itself is per-worktree git
config rather than a tracked file.

Workspace Scope is stored under the single git worktree config key `tk.scope`.
The stored value is the user-typed Display ID or Alias string (not the internal
stable `items.id`), which keeps `git config --worktree --get-all tk.*`
human-inspectable and lets the existing Alias resolver carry old references
through Promotion. `tk worktree set` and `tk worktree start` lazily enable
`extensions.worktreeConfig = true` at repo level on first use; `tk init` does
not touch repo-level git config. There is no persisted `tk.scopeSource` —
Workspace Scope Source is derived at read time from whether `tk.scope` is
present in the current worktree's config.

Workspace Scope discovery is a two-function split that mirrors the
`git/discovery.zig` ⇄ `store/repository.zig` boundary: `readGitSide(gpa, runner,
cwd)` runs the git subprocesses (`git config --worktree --get tk.scope` and,
on miss, `git symbolic-ref --short HEAD`) and returns raw strings;
`resolveAgainstStore(store, raw)` does the SQL match against `item_ids` and
returns a typed `Scope` (`.none`, `.configured`, or `.inferred`). `Deps` does
not grow a worktree service yet — callers compose the two free functions. Any
non-zero exit from `git config --worktree --get tk.scope` (key absent, extension
disabled, version differences) collapses to "no configured scope"; detached
HEAD collapses to "no inference".

Branch-name inference is prefix-strict: only branches matching `tk/<display-id>`
or `tk/<display-id>-<slug>` infer Workspace Scope. The Display ID portion is
variable-length (`project-1`, `PROJ-123`, `acme/proj-1`) and may itself contain
`-`, so extraction uses a single SQL query against `item_ids` that selects the
longest stored `value` for which the branch tail (after stripping `tk/`) is
either an exact match or has a `-` boundary directly after the value:
`select value from item_ids where (?1 = value or ?1 like value || '-%') collate
nocase order by length(value) desc limit 1`. The query reuses the existing
`item_ids` partial index, returns the canonical stored spelling, and uses
`collate nocase` so case differences in branch names still resolve. Anything
that fails the prefix check is "no inference"; users who want scope without the
`tk/` prefix run `tk worktree set <id>`. Branch-name inference is read-only and
lower precedence than configured Workspace Scope.

`tk worktree start <id>` creates a Ticket Branch named
`tk/<display-id>-<branch-slug>` and a git worktree at
`<parent-of-main-toplevel>/<repo-basename>.<display-id>-<path-slug>` by default.
The branch slug is the title sanitized to lowercase `[a-z0-9-]` (every other
character collapsed into a single `-`, leading and trailing `-` trimmed) and
capped at 40 characters at the last `-` boundary that fits; an empty result
drops the slug so the branch is just `tk/<display-id>`. The path slug uses the
same sanitizer with a tighter 30-character cap so paths stay short enough for
tab completion (length distribution from a real monorepo set the cap; ADR 0007
records the data). The Display ID portion of the path is lowercased and has
`/`, `:`, and `#` replaced with `-` for cross-filesystem safety, then
consecutive `-` collapsed. The explicit `[path]` positional is interpreted
relative to the current working directory and bypasses both default paths.
"Parent of main toplevel" is computed from the *main* worktree, not the
current one, so running `tk worktree start` from inside an existing linked
worktree still places the new worktree next to the main repo rather than
nesting inside the linked one.

`tk worktree start` executes a fixed sequence: (1) resolve `<id>` to the
internal stable ID; (2) reject `done` items uniformly per ADR 0006, even with
`--no-status`, so worktree creation does not become a backdoor around the
done-terminal rule; (3) reject if the default or explicit path already exists;
(4) reject if the would-be branch already exists; (5) run main-toplevel
discovery via `git/discovery.zig`; (6) ensure `extensions.worktreeConfig =
true` at repo level (idempotent); (7) `git worktree add -b <branch> <path>`
from the main toplevel using current HEAD as base (no `--base` flag in v1);
(8) `git -C <path> config --worktree tk.scope <stored-value>` against the new
worktree's per-worktree config; (9) call `repository.setItemStatus` with
target `active` unless `--no-status`; (10) print the success block. Steps 1–4
are pure preflight that catch every foreseeable error before any side effect.
Step 7 is the commit point: before it, `tk worktree start` refuses with no
filesystem or git state changes; after it, partial failures are surfaced as
recovery diagnostics rather than rolled back. A failure between steps 7 and 8
leaves a worktree whose scope is still discoverable through branch inference
(the branch matches `tk/<id>-<slug>` by construction); the diagnostic suggests
`tk worktree set <id>`. A failure between 8 and 9 leaves a fully scoped
worktree whose Ticket is not yet active; the diagnostic suggests `tk start
<id>`. v1 deliberately does not attempt to `git worktree remove` a successful
add, because the partial-state recovery paths are one-line follow-ups using
commands shipped in this same slice.

`tk worktree set <id>` overwrites silently when `tk.scope` is already
configured — same field-idempotent shape as `tk update`, `tk start`, and
`tk done`. `tk worktree clear` is a no-op success when no `tk.scope` key
exists: it runs `git config --worktree --unset tk.scope` and treats the
key-not-found exit code as success so users do not have to pre-check with
`tk worktree`.

`tk worktree` (no subcommand) prints a two-line status block on stdout. The
labels left-align at column eight so the values share an indent:

```text
Scope:  <display-id> - <title>
Source: configured
```

```text
Scope:  <display-id> - <title>
Source: inferred from branch '<branch-name>'
```

The Display ID rendered is always the *current* Display ID after Alias
resolution, not the stored string, so a worktree whose configured scope was a
local Display ID before Promotion shows the new backend Display ID after.
The branch name in the inferred-source hint is the raw current branch, kept
verbatim so the message stays diagnostically truthful. No scope of either kind
exits 0 with `No Workspace Scope.`; absence is descriptive, not an error. A
configured scope whose stored value no longer resolves through `item_ids`
exits 1 with `tk worktree: Workspace Scope '<stored>' is not a known Display
ID or Alias`, defending against manual `git config` edits — v1 reserves
Display IDs and Aliases indefinitely so this should only fire on hand-edited
config.

`tk worktree start` success output is one Beads-style four-line block on
stdout:

```text
Created worktree for Ticket: <display-id> - <title>
Status: active
Branch: tk/<display-id>-<branch-slug>
Path:   <absolute-path>
```

The `Status:` line is omitted entirely when `--no-status` is set; printing
`Status: open` would be misleading since the *worktree* was created, not the
Ticket's status changed. Epics render the verb-noun pair as `Created worktree
for Epic:`. The path is absolute so agents can `cd` to it without computing
relative paths from their current working directory.

`tk next` is the first consumer of Workspace Scope discovery in this slice.
It calls `readGitSide` plus `resolveAgainstStore`, then passes the resulting
`Scope` straight into `repository.nextReadyTicket(... .scope = .{ .display_arg
= ... })`. The "scoped empty selection" diagnostic
(`tk next: no ready Tickets in Workspace Scope`) now activates when
`Source` is `configured` or `inferred`. Loosening the still-required `<id>`
positional to optional on `tk show`, `tk update`, `tk start`, `tk stop`, and
`tk done` is deferred to a focused follow-up slice; this slice keeps the
behavior change to one command so the discovery primitive can stabilise
against `tk next` first.

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
- `tk list` command/store tests for List Tree rendering, readiness and Origin
  filtering, Dependency and External Blocker behavior, blocked glyph rendering,
  empty reads, usage errors, missing-store diagnostics, and `tk list --help`
  scenario coverage.
- `tk next` command/store tests for ready Ticket selection, Priority and
  creation-order sorting, scoped Display ID and Alias resolution, missing-store
  diagnostics, empty ready results, and `tk next --help` scenario coverage.
- `tk show` command/store tests for Ticket and Epic detail rendering, parent
  and child sub-sections, Dependency and External Blocker rows, Display ID and
  Alias resolution, missing-store and unknown-id diagnostics, and `tk show
  --help` scenario coverage.
- `tk update` command/store tests for title and body edits, local-only
  Priority writes, Epic parent set/clear, field-idempotent invocations, the
  no-Mutation invariant for local-origin rows, the `set_item_status`-free
  outbox for Backend-origin rows, mutual-exclusion and class-level validation
  errors, missing-store, unknown-id, and unknown-parent diagnostics, forced
  write rollback, and `tk update --help` scenario coverage.
- `tk done` command/store tests for local Ticket and Epic completion, Alias
  resolution, the `set_item_status` Mutation emitted for Backend-origin rows,
  field-idempotent invocations that leave `updated_at` unchanged and emit no
  Mutation for both Local and Backend origins, blocker-completion releasing
  the `ready`/`blocked` views, forced write rollback that reverts both
  current state and the `mutation_seq` allocation, missing-store and
  unknown-id diagnostics, and `tk done --help` scenario coverage.
- `tk block` and `tk unblock` command/store tests for Dependency readiness
  effects, same-backend `add_dependency` / `remove_dependency` Mutations,
  idempotent existing-edge handling, self-edge rejection, done-item rejection,
  cycle diagnostics, Backend/Local and mixed-Backend gating, and `--help`
  scenario coverage.
- subprocess smoke tests for real `tk init`, real `tk add`, real `tk list`,
  and real `tk next` after `git init`.

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

The current binary implements `tk prime`, `tk init`,
`tk add -F <file | ->`, `tk list`, `tk next`, `tk show`, `tk update`,
`tk done`, `tk start`, `tk stop`, `tk block`, and `tk unblock`.

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

`tk list` opens the existing Repository Store and renders the global List Tree
with plain text tree glyphs. The default view includes open and active rows.
`--ready`, `--blocked`, and `--active` are mutually exclusive readiness
filters; `--local` and `--remote` are mutually exclusive stored Origin filters
and may compose with one readiness filter. Done-item browsing is deferred until
there is a concrete workflow for limiting or windowing old completed work. The read API in
`store.repository.listRows` pushes readiness, blocking, Origin filtering, and
Epic container retention into SQL, while `src/commands/list.zig` owns final
tree rendering and footer counts.

Readiness is derived from current Repository Store state. Ready work is open
Tickets without unresolved Dependencies or External Blockers. Blocked work is
open or active Tickets with an unresolved Dependency or External Blocker; Epics
may render as containers but are not selected as blocked work. Parent Epic
status does not change child Ticket readiness, so an otherwise ready child
under a done Epic still matches `--ready`. Dependencies block only while the
Blocking Item is not `done`.

`tk list` output uses `○`, `◐`, and `✓` for `open`, `active`, and `done`.
Ticket rows render `● <priority>` plus `[bug]` for bug Tickets; task Tickets
omit a kind marker. Epic rows render `[epic]` and no Priority. Blocked rows
render `⊘` after the Display ID. Output ends with a separator,
rendered-row totals by Item Status, the status legend, and a blocked legend
line (`Blocked: ⊘ blocked`). Empty reads succeed with exit code `0` and print
the filter-specific empty message.
The blocked glyph is an overlay on any rendered Ticket or Epic with unresolved
Dependencies or External Blockers. It does not change row inclusion by itself:
`tk list --blocked` still selects open or active blocked Tickets only, while
default and active views may show blocked Epics when those Epics otherwise
render.

`tk next` opens the existing Repository Store and selects one ready Ticket
from current state. Ready work is the same Repository Store concept used by
`tk list --ready`: Item Status `open`, no unresolved Dependencies, and no
External Blockers. The Repository Store read API owns Priority ordering,
`created_seq` tie breaks, and scoped selection by Display ID or Alias. Backend
Tickets use local import order for that tie break, not backend-native creation
time. Selection is deterministic and does not randomize among candidates.
Ticket Kind does not affect ordering. Assignees are not readiness or ordering
inputs; Assignee support is deferred and may be omitted entirely. `tk next`
does not explain skipped candidates or ranking reasons; inspection belongs to
`tk list --ready`, `tk list --blocked`, and `tk show`. `tk next` does not
filter by Origin; Local Tickets and Backend Tickets compete in the same
ready-work ordering. Mutation Log, Mutation Failure, and Sync Cursor state are
not readiness inputs for `tk next`; sync health belongs to `tk sync log`, not
`tk next` warnings. `tk next` is read-only and does not change Item Status; `tk
worktree start` owns starting work and marking a Ticket active by default.
Because Workspace Scope discovery has not landed yet, the command currently
passes no scope and therefore searches all ready Tickets. `tk next` is flagless
in v1 and accepts no positional scope argument; global ready-work inspection
uses `tk list --ready`. Whether `tk list` defaults to Workspace Scope is
deferred from v1.
The Repository Store API already supports scoped ready-Ticket selection by
Display ID or Alias so the future worktree slice can supply Workspace Scope
without changing selection semantics.
Parent Epic status, Dependencies, and External Blockers do not hide otherwise
ready child Tickets.

`tk next` renders one flush-left Display ID. The selected Ticket's Priority and
title stay out of stdout so agents and scripts can use `id=$(tk next)` and read
the full item through `tk show "$id"`. There is no JSON or structured-output
mode in v1; the Display ID line is the machine interface:

```text
<display-id>
```

If no ready Ticket matches repository-wide selection, `tk next` exits `1` and
writes `tk next: no ready Tickets` to stderr. Once Workspace Scope discovery
lands, scoped empty selection uses
`tk next: no ready Tickets in Workspace Scope` so the diagnostic does not imply
the whole Repository Store has no ready work.

`tk show <id>` renders one Ticket or Epic with its full current state in a
Beads-style layout: a `<status-glyph> <display-id> · <title>   [<facet> ·
<STATUS>]` header where the facet is `● <priority>` for Tickets and `EPIC`
for Epics; an `Origin: <origin>[ · Kind: <kind>]` metadata line where
Backend items render `Origin: github (#<key>)` or `Origin: jira (<key>)`;
a `Created: YYYY-MM-DD · Updated: YYYY-MM-DD` date line truncated from the
stored ISO timestamps; and optional `DESCRIPTION`, `PARENT` (for Tickets)
or `TICKETS` (for Epics), `BLOCKED BY`, `BLOCKING`, and `EXTERNAL BLOCKERS`
sections separated by one blank line. Empty sections are omitted entirely
to keep the layout readable for minimal items. The positional `<id>` is
required while Workspace Scope discovery is deferred; resolved through the
same Display ID / Alias resolver as every other item argument. Unknown id
exits `1` with `tk show: '<id>' is not a known Display ID or Alias`.
`BLOCKED BY` and `BLOCKING` show only unresolved Dependencies (joined Item
Status not `done`); `EXTERNAL BLOCKERS` lists rows where `resolved_at is
null`. Aliases are not yet rendered (no Promotion has happened in v1).

`tk update <id>` is the first command that writes to the Mutation Log. It
edits title/body, local-only Priority, and Epic membership. Argument shape:
`-m <paragraph>...` (repeatable, git-commit-style; first paragraph is the
title) or `-F <file | ->` (mutually exclusive with `-m`), `--priority
P0..P4`, and `--parent <epic-id> | --no-parent` (mutually exclusive). At
least one editing flag is required; no editing intent is a usage error.
The positional `<id>` is required while Workspace Scope is deferred.
`--priority` on an Epic target and `--parent` or `--no-parent` on an Epic
target are usage errors. `--parent <id>` resolves through the same Display
ID / Alias resolver and must resolve to an Epic; "resolves to a Ticket"
gets a user-facing diagnostic instead of a raw FK error. The success line
is `Updated Ticket: <display-id> - <title>` or `Updated Epic: <display-id>
- <title>`, always emitted on a syntactically-valid update even when the
result is field-idempotent.

`tk update`'s Mutation rules: **Origin gates Mutations.** Local Ticket
and Local Epic edits update current Repository Store state only; they
never advance `mutation_seq`. Backend Ticket and Backend Epic title/body
and parent edits append Mutations in the same transaction as the
current-state write. **Priority is a Local Field** and never emits a
Mutation, even for Backend items; `--priority` changes `items.priority`
and bumps `updated_at` only. **`update_ticket` and `update_epic` carry
the full `{title, body}` snapshot** in `payload_json`, never a partial
patch, so Mutation Apply stays idempotent under retries. **Moving
between Epics emits two Mutations** in one transaction sequenced
remove-then-add: `remove_ticket_from_epic {epic_id: <old>}` then
`add_ticket_to_epic {epic_id: <new>}`. `epic_id` is the internal stable
`items.id`, not the Display ID, so Promotion cannot break the reference.
**Field idempotence at the row level**: identical input is dropped (no
UPDATE, no Mutation append, `updated_at` unchanged). The command still
prints `Updated Ticket: …` on field-idempotent invocations so agents
need not parse the diff to confirm the call was accepted.

`tk update`'s message parsing reuses `src/commands/message.zig`:
`parseFromParagraphs` joins repeatable `-m` slices with a single blank
line and delegates to `parse`, so CRLF/CR normalization, title folding,
and body trim semantics are identical to `tk add -F`.

The typed Mutation infrastructure lives in three small modules.
`src/domain/mutation_type.zig` exports the `MutationType` enum whose tag
names match the SQL `check(mutation_type in (...))` spellings exactly so
text round-trips without a separate map. `src/store/mutations.zig`
exports `MutationPayload` — a shape-keyed tagged union (`update_title_body`
for `update_ticket`/`update_epic`; `epic_ref` for
`add_ticket_to_epic`/`remove_ticket_from_epic`; `item_status` for
`set_item_status`; `dependency_ref` for `add_dependency`/`remove_dependency`;
future slices extend it) plus
`appendMutation(conn, gpa, mutation_type, item_id, item_class,
payload, now)`. The function must run inside the caller's active `begin
immediate` transaction; it allocates `mutation_seq` from
`sequences.next`, serializes the payload to JSON via
`std.json.Stringify.valueAlloc`, and inserts one row with state
`pending`. `src/store/sequences.zig` exposes `next(conn, name)` — the
shared allocator for the named counters in the `sequences` table —
called by both `createLocalTicket` and `appendMutation` so neither has
to depend on the other for sequence numbers.

`tk show` and `tk list` share `ItemStatus.glyph()` from
`src/domain/status.zig` so the `○`/`◐`/`✓` rendering stays in lockstep
across commands. `src/commands/show.zig` keeps the row-shape rendering
local (the Beads-style layout is `tk show`'s contract, not a shared
concern); `src/commands/update.zig` shares no rendering with other
commands.

`tk show`'s store read API is `repository.showItem(store, gpa,
display_arg) -> ?ItemDetail`. `ItemDetail` carries the item row plus
allocator-owned `parent: ?ItemSummary`, `children: []ItemSummary` (for
Epics), `blocked_by: []ItemSummary`, `blocking: []ItemSummary`, and
`external_blockers: []ExternalBlockerSummary`. The read runs the main
item lookup plus four related-row queries; it does not open a
transaction (snapshot consistency across the related reads is a
deliberate non-goal for a read-only display command in this slice).

`tk update`'s store write API is `repository.updateItem(store, gpa,
clock, UpdateRequest) -> UpdateError!UpdateOutcome`. The request carries
the already-resolved internal id, the item's `ItemClass`, and optional
edits for title, body, priority, and a `ParentOp` (`unchanged | clear |
set: <internal-id>`). The function opens `begin immediate`, reads the
current row inside the transaction, computes per-field deltas against
the stored values, writes only the changed columns plus `updated_at`,
and — for Backend-origin items — calls `mutations.appendMutation`
sequenced remove-then-add-then-update. The current row is held with
`defer` until function exit so OOM during the column copy-out does not
leak the SQLite cursor before the `errdefer store.conn.rollback()`
fires.

`tk done <id>` is the minimum lifecycle command for dogfooding. The
positional `<id>` is required while Workspace Scope discovery is deferred,
matching `tk show` and `tk update`, and resolves through the same Display ID /
Alias resolver. The command marks one Ticket or Epic `done` and prints
`Marked Ticket done: <display-id> - <title>` or
`Marked Epic done: <display-id> - <title>`. Local-origin writes update current
state only. Backend-origin writes update current state and append one pending
`set_item_status` Mutation in the same transaction with JSON payload
`{"status":"done"}`. The payload deliberately stores only the target status,
not prior status or transition intent; that surviving risk is accepted until
Backend Adapters prove they need richer lifecycle semantics.

`tk done` uses the status-generic Repository Store helper
`repository.setItemStatus(store, gpa, clock, SetStatusRequest) ->
SetStatusError!SetStatusOutcome`. The request carries the resolved internal
id and target `ItemStatus`; the returned snapshot carries copied Display ID,
title, persisted `ItemClass`, and effective status. The helper opens `begin
immediate`, reads `origin`, `status`, `item_class`, `display_value`, and
`title` inside the transaction, copies the Display ID and title while the
SQLite row cursor is alive, and returns the row's persisted `ItemClass` rather
than request metadata. Field-idempotent calls commit without UPDATE, leave
`updated_at` unchanged, and emit no Mutation. Changed Backend-origin rows call
`mutations.appendMutation` after the current-state UPDATE and before commit,
so allocation or SQLite failure rolls back both current state and the outbox
sequence allocation. Any request that would move a `done` row to a non-`done`
Item Status short-circuits with the `.locked_done` outcome arm, carrying the
persisted `ItemClass` for caller-side rendering; ADR 0006 makes `done`
terminal in v1, and migration 002's `items_no_escape_from_done` trigger
backstops the rule for any future writer that bypasses `setItemStatus`.

`tk start <id>` and `tk stop <id>` are the symmetric lifecycle commands.
`tk start` writes Item Status `active`; `tk stop` writes Item Status `open`.
Both resolve the positional `<id>` through the same Display ID / Alias
resolver as `tk done`, route through `repository.setItemStatus`, and print
`Marked Ticket active: <display-id> - <title>` / `Marked Epic active: …` or
`Marked Ticket open: <display-id> - <title>` / `Marked Epic open: …` on
success. Local-origin writes update current state only; Backend-origin
writes append one pending `set_item_status` Mutation in the same
transaction with JSON payload `{"status":"active"}` or `{"status":"open"}`.
Field-idempotent calls (`tk start` on an already-active row, `tk stop` on
an already-open row) match `tk done`'s behavior: the success line still
prints, no Mutation is appended, and `updated_at` is unchanged. The
`.locked_done` outcome arm surfaces as `tk start: cannot start a done
<Ticket|Epic>` or `tk stop: cannot stop a done <Ticket|Epic>` on stderr
with exit 1; per ADR 0006 done is terminal in v1, so neither command can
revive a completed Ticket or Epic, and the schema-level
`items_no_escape_from_done` trigger from migration 002 enforces the
constraint for any writer that bypasses the pre-read short-circuit. v1
intentionally has no single-active invariant — multiple Tickets and Epics
may be `active` simultaneously, and readiness selection by `tk next` and
`tk list --ready` is not gated by `tk start`. `tk worktree start` (future
worktree slice) will route through `setItemStatus` and inherit the `.locked_done`
outcome.

Dependency Mutations are owned by the Blocked Item. When a Dependency
change emits `add_dependency` or `remove_dependency`, `mutations.item_id`
is the internal stable ID of the Blocked Item, `mutations.item_class` is
that item's class, and the payload carries the Blocking Item's internal
stable ID as `{"blocking_id":"<internal-id>"}`. This matches the
user-visible effect: the Blocked Item's readiness changes, while the Blocking
Item is the referenced prerequisite.

Origin gating follows the Blocked Item. A Local Blocked Item may depend on
either a Local or Backend Blocking Item and emits no Mutation. A Backend
Blocked Item may depend on a Backend Blocking Item from the same
`backend_kind` and emits the Dependency Mutation in the same transaction as
the current-state edge change. A Backend Blocked Item cannot depend on a
Local Blocking Item, or on a Backend Blocking Item from another
`backend_kind`, in v1 because the target Backend Adapter would have no
single backend identity to apply; `tk block` should reject those combinations
with user-facing diagnostics rather than deferring the failure to sync.

Dependency writes are desired-state operations. `tk block` succeeds when the
edge already exists, and `tk unblock` succeeds when the edge is already
absent. These field-idempotent calls leave current state unchanged, do not
append Mutations, do not advance `mutation_seq`, and do not change either
item's `updated_at`. Changed Dependency edges also leave both items'
`updated_at` unchanged; the Dependency row and any emitted Mutation carry
their own timestamps.

`tk block` creates only live blocking relationships: it rejects the command
when either the Blocked Item or the Blocking Item is already `done`. `tk
unblock` may remove an existing Dependency regardless of either item's Item
Status so old or imported edges can always be cleaned up.

`tk unblock` also allows cleanup of existing edges whose Origin or backend-kind
combination would be rejected for new `tk block` creation. Removal emits a
`remove_dependency` Mutation only when the pair is backend-applicable by the
same Origin rules used for `add_dependency`; otherwise the removal is local
current-state cleanup.

`tk block <same-id> <same-id>` and `tk unblock <same-id> <same-id>` are
rejected before any write with `tk block: an item cannot depend on itself` or
`tk unblock: an item cannot depend on itself`.

Unknown ID diagnostics are role-specific: `tk block: blocked item '<id>' is
not a known Display ID or Alias` / `tk unblock: blocked item '<id>' is not a
known Display ID or Alias`, and the same shape with `blocking item` for the
second argument. Resolve and report the Blocked Item first, then the Blocking
Item, so argument-order mistakes produce stable output.

`tk block` validation diagnostics keep the same roles: `tk block: blocked item
'<id>' is done`, `tk block: blocking item '<id>' is done`, `tk block:
Backend blocked item '<blocked-id>' cannot depend on Local blocking item
'<blocking-id>'`, and `tk block: Backend blocked item '<blocked-id>' cannot
depend on blocking item '<blocking-id>' from another Backend kind`.

Cycle rejection is an expected domain outcome, not a generic SQLite failure.
The store write helper should check for the would-be cycle inside the same
`begin immediate` transaction and return a typed outcome that `tk block`
renders as `tk block: Dependency would create a cycle`. The schema trigger
remains a backstop for future writers or race-shaped mistakes that bypass the
preflight.

`tk block` and `tk unblock` success output is one flush-left line:
`Added Dependency: <blocked-id> blocked by <blocking-id>` or
`Removed Dependency: <blocked-id> no longer blocked by <blocking-id>`. The
IDs are rendered as current Display IDs after Alias resolution.

## Next Slices

Continue in small vertical slices:

1. Worktree scope
   - Implement the four `tk worktree` subcommands and the
     `src/worktree/scope.zig` discovery primitive per the Worktrees design
     section above.
   - Wire discovery into `tk next` as the first consumer; defer optional
     `<id>` for the other lifecycle commands.

2. Remote and sync skeleton
   - Implement `tk remote`.
   - Implement `tk sync log`.
   - Add fake remote adapter tests before real `gh` or `acli` behavior.

## Deferred

- Dynamic `tk prime` sections.
- External Blocker create/resolve CLI. The Repository Store and read views
  already model External Blockers, but the command surface needs a stable way
  to identify one External Blocker when several exist on the same item.
- Promotion behavior for existing Local Dependencies. The blocking slice
  defines current-state Dependencies and immediate Dependency Mutations only;
  the Promotion slice must decide whether backend-applicable Local
  Dependencies are snapped into backend intent when a Local Blocked Item is
  promoted.
- Comments, labels, and assignees. Assignee support is not assumed to land.
  Labels remain descriptive facets only and must not replace Priority, Ticket
  Kind, Epic membership, Item Status, or blocking concepts.
- Custom local Display ID prefix configuration, such as `tk init --prefix`,
  unless the repository-basename default proves too implicit before item
  creation lands.
- Force sync or conflict resolution.
- Multiple remotes.
- Non-git Workspace Scope storage.
- Cross-repository local import/export.
