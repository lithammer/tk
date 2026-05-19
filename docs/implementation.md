# Implementation Plan

This document maps the current Zig implementation. It is intentionally compact:
per [ADR 0008](./adr/0008-keep-implementation-doc-compact.md), shipped slice
checklists should not live here after code, tests, `CONTEXT.md`, `docs/cli.md`,
and ADRs carry the durable contracts.

## Inputs

- Domain language: [../CONTEXT.md](../CONTEXT.md)
- CLI surface: [cli.md](./cli.md)
- Design decisions: [adr/](./adr/)
- Resolved questions: [design-questions.md](./design-questions.md)
- Prime command output: [../src/commands/prime.md](../src/commands/prime.md)
- Current backlog: `./zig-out/bin/tk list`

## Current Module Map

```text
src/
  main.zig                 process entrypoint
  cli.zig                  top-level dispatch and shared Deps
  messages.zig             stable user-visible substrings
  commands/                per-command parsing and handlers
  domain/                  pure domain enums and helpers
  git/                     Git subprocess discovery façade
  proc/                    subprocess runner abstraction and fakes
  store/                   Repository Store, migrations, Mutations
  worktree/                Workspace Scope storage and discovery
  testing/                 CLI harnesses, txtar runner, smoke tests
```

Only add modules when a slice needs them. Prefer moving reusable behavior into a
small boundary module after the second caller proves the shape.

## Boundaries

- `main.zig` is a thin process shim. It builds real `cli.Deps`, calls
  `runArgv`, maps unexpected propagated errors to exit code `3`, and does not
  own command logic.
- `cli.zig` owns top-level routing only. The command tuple is the single place
  to register a command module; help text and dispatch derive from it.
- `commands/<cmd>.zig` owns that command's zig-clap spec, command-specific
  validation, rendering, and calls into store/worktree/git helpers.
- `domain/` stays pure: no SQLite, filesystem paths, Git, subprocesses, or
  command rendering.
- `proc/` captures subprocess output for callers to classify. Commands should
  not stream Git or Backend Adapter subprocess output directly to user writers.
- `git/` classifies Git discovery outcomes and keeps shared Git diagnostic
  phrasing out of command modules.
- `store/` owns Repository Store opening, migrations, current-state reads and
  writes, Display ID / Alias resolution, sequence allocation, and Mutation Log
  persistence.
- `worktree/` owns Workspace Scope storage, branch-name inference, and slug
  derivation. Commands compose its free functions rather than growing a service
  object prematurely.

`cli.Deps` carries explicit dependencies: stdout/stderr/stdin writers, general
allocator, `std.Io`, cwd handle, subprocess runner, UTC millisecond clock, and
random source. Writers are borrowed for one command invocation and must not be
retained past return. `Deps` grows additively as slices need more injectable
boundaries.

Exit codes returned by command dispatch:

- `0` success
- `1` logical failure surfaced to the user
- `2` usage error
- `3` unexpected internal error caught at the process boundary

## Repository Store Contracts

The Repository Store is SQLite, per ADRs
[0001](./adr/0001-untracked-repository-store.md),
[0003](./adr/0003-use-current-state-store-with-mutation-outbox.md), and
[0005](./adr/0005-use-sqlite-for-the-repository-store.md). `tk init` creates it
at `<git-common-dir>/tk/ticket.db`; later commands open that store through the
shared opener instead of duplicating discovery and validation.

Migration SQL files are the source of truth for exact table columns and checks.
Important stable contracts:

- `schema_migrations` and `PRAGMA user_version` track migrations.
- `PRAGMA application_id = 0x544B4442` identifies Ticket stores.
- Connections enable foreign keys and a busy timeout; `tk init` enables WAL.
- `items` stores current Ticket/Epic state. Current state is the read model;
  the Mutation Log is an outbox, not an event-sourced source of truth.
- `item_ids` resolves current Display IDs and Aliases case-insensitively.
  Promotion changes the current Display ID and preserves the old one as an
  Alias.
- `dependencies` stores directional Blocking Item -> Blocked Item edges and
  rejects cycles. Dependency resolution derives from the Blocking Item's
  current Item Status.
- `external_blockers` stores blockers with explicit resolution state. The store
  and read views exist; command surface is tracked by `ticket-19`.
- `mutations` stores durable backend intent with a monotonic Mutation Sequence,
  state, JSON payload, and optional Mutation Failure JSON.
- `remotes` and `sync_cursors` are present for the v1 single Remote model;
  command and sync wiring is tracked by `ticket-17`.
- `store_config.display_prefix` controls newly generated local Display IDs.
  Custom prefix configuration is tracked by `ticket-22`.

Write commands use `BEGIN IMMEDIATE` and commit current-state changes together
with any required Mutation appends. Origin gates Mutations: Local items update
current state only until Promotion; Backend items append backend-applicable
Mutations in the same transaction as the visible state change. Priority remains
a Local Field and does not emit Mutations.

`done` is terminal in v1 per
[ADR 0006](./adr/0006-done-is-terminal-in-v1.md). Store-facing status changes
route through `setItemStatus`, and the schema trigger backstops future writers.

## IDs

Items have random opaque internal stable IDs. Display IDs and Aliases are the
user-facing lookup keys and are globally unique across Tickets and Epics.

Local Display IDs use the stored Repository Store prefix plus one shared
sequence for Tickets and Epics: `<store-prefix>-<n>`. The prefix identifies the
local repository context, not item class. Containment lives in `items`, not in
Display ID structure.

## Command Status

Current command modules exist for:

- `tk prime`
- `tk init`
- `tk add`
- `tk list`
- `tk next`
- `tk show`
- `tk update`
- `tk start`, `tk stop`, `tk done`
- `tk block`, `tk unblock`
- `tk worktree`, `tk worktree set`, `tk worktree clear`, `tk worktree start`

`docs/cli.md` describes the v1 target surface. Some target commands or flags
remain backlog work; use `./zig-out/bin/tk list` and each ticket body for the
active implementation plan.

Commands write primary output to stdout and diagnostics to stderr. Stable
user-observable substrings live in `src/messages.zig` so command code and tests
share phrasing.

## Workspace Scope and Worktrees

Workspace Scope is local-only and stored in git Worktree Config in v1. Worktree
Config scope takes precedence over read-only branch-name inference. Ticket
Branches use `tk/<display-id>-<slug>` so scope remains inferable after manual
worktree creation and after Promotion through Aliases.

`tk worktree start` creates a scoped git worktree and marks the item `active` by
default unless `--no-status` is used. The default path layout is recorded in
[ADR 0007](./adr/0007-default-worktree-path-layout.md). Missing preflight checks
are tracked by `ticket-15`; configurable path layout is tracked by `ticket-16`.

Workspace Scope is a selection context, not an implicit item target. Commands
that inspect, update, or promote a specific item require explicit Display IDs;
agents should pass IDs selected by `tk next` or `tk list`.

## Remote Adapters and Sync

Backend Adapters expose only Backend Pull and Mutation Apply. The sync engine
owns ordering, Sync Cursors, retries, failures, skips, and conflict policy.
Adapters call external CLIs such as `gh` and `acli` through the injectable
subprocess runner.

The module split is one-way: `src/sync/` (engine, log views) imports
`src/remote/` (adapter trait, factory, fake) and `src/store/`; `src/remote/`
imports `src/store/` and `src/proc/` but never `src/sync/`. Adapters and the
engine reach the database through `src/store/repository.zig` helpers rather
than touching SQL directly. The adapter trait is type-erased (`context:
*anyopaque` + `vtable`), mirroring `proc.Runner`; the engine decodes
`mutations.payload_json` into a typed `MutationView` so adapters switch on the
variant rather than parsing JSON.

Sync failures are split into three categories per [ADR
0009](./adr/0009-sync-failure-taxonomy.md), distinguished by what the engine
does with them rather than by speculative classification of subprocess output:

- **Catastrophic env failures** (`ExecutableNotFound`, `SpawnFailed`,
  `OutOfMemory`) are bare error tags; engine renders to stderr and exits 1
  with no state change.
- **Pull failed mid-sync** (`PullError.PullFailed` + `?*Diagnostic` carrying
  captured stderr) is rendered and stops the sync run; no mutation transitions
  because Pull is not tied to a specific Mutation row.
- **Apply failed for a specific Mutation** (`Outcome.failure { detail }`) is
  persisted to `mutations.failure_json` and stops the sync run. `Failure`'s
  `detail` field is the forward-compatible placeholder that `ticket-11`
  graduates into a typed discriminated union without changing the engine
  persistence step.

Backend Pull merges snapshots into `items` under one transaction per [ADR
0010](./adr/0010-pull-merge-skips-items-with-pending-mutations.md): if any
`pending` or `failed` Mutation targets the snapshot's `(item_id, item_class)`,
skip the whole row and let Apply reconcile; otherwise overwrite `title`,
`body`, `status`, `updated_at`. Absence from the snapshot list is a no-op —
v1 does not infer deletions. Container relations (Epic membership) are not
returned by Pull in v1.

The Remote/sync skeleton is tracked by `ticket-17`; the persisted Mutation
Failure / Adapter Failure record shape is tracked by `ticket-11`.

## Testing

Use layered tests:

- Zig unit tests for pure domain behavior and store helpers.
- Command-handler tests with fake stores, fake subprocess runners, fake clocks,
  and allocating writers.
- SQLite migration/store tests against temp databases.
- Inline snapshots for small rendered outputs.
- txtar-based CLI scenarios for multi-step command behavior.
- Subprocess smoke tests for linked-binary wiring, embedded payloads, Git
  subprocess discovery, filesystem writes, and SQLite linkage.

Avoid testing everything through subprocess scenarios. Prefer the narrowest
layer that can observe the behavior.

The txtar script runner follows the `testscript`-style tokenizer documented in
`src/testing/script.zig`: whitespace splitting, single-quote literals, comments
with `#`, `$NAME` / `${NAME}` env expansion, byte-exact output comparison, and
`TK_UPDATE=1` snapshot rewriting.

## Next Slices

Continue in small vertical slices. Backlog ownership lives in Local Tickets;
this section only names the current implementation queue.

1. `ticket-17` — Remote and sync skeleton. Implement the v1 `tk remote` surface,
   `tk sync log`, and fake Backend Adapter tests before real `gh` or `acli`
   behavior. Coordinate with `ticket-11` for the persisted Mutation Failure /
   Adapter Failure shape.

## Deferred Backlog

Deferred implementation work is tracked as Local Tickets so it remains visible
to `tk list` / `tk next` instead of living only in this document. The ticket body
is the source of truth for each deferred item.

- `ticket-18` — Dynamic `tk prime` sections.
- `ticket-19` — External Blocker create/resolve CLI.
- `ticket-20` — Promotion behavior for existing Local Dependencies.
- `ticket-21` — Comments, labels, and assignees.
- `ticket-22` — Custom local Display ID prefix configuration.
- `ticket-23` — Force sync or conflict resolution.
- `ticket-24` — Multiple Remotes.
- `ticket-25` — Non-git Workspace Scope storage.
- `ticket-26` — Cross-repository local import/export.
- `ticket-16` — Configurable worktree path layout for `tk worktree start`.
