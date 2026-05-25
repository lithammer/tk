# Architecture

This document maps how the tk codebase is organized — which directory
owns which role, and the durable invariants the Repository Store preserves.
It is intentionally compact: per [ADR
0008](./docs/adr/0008-keep-implementation-doc-compact.md), shipped slice
checklists should not live here once code, tests, command help, `CONTEXT.md`,
and ADRs carry the durable contracts. Onboarding pointers live in
`README.md`; agent-facing conventions (code documentation, error handling,
testing) live in `AGENTS.md`; domain vocabulary lives in `CONTEXT.md`; the
command reference lives in `tk --help`, `tk <command> --help`, and
`man/tk.1`.

## Module Map

```text
src/
  main.zig                 process entrypoint
  cli.zig                  top-level dispatch and shared Deps
  messages.zig             stable user-visible substrings
  commands/                per-command parsing and handlers
  domain/                  pure domain enums and helpers (incl. sync contract
                           types: MutationPayload, MutationView,
                           BackendItemSnapshot, Outcome, Diagnostic)
  git/                     Git subprocess discovery façade
  proc/                    subprocess runner abstraction and fakes
  remote/                  Backend Adapter trait, factory, and FakeAdapter
  store/                   Repository Store, migrations, Mutation Log, and
                           sync helpers
  sync/                    sync engine orchestration
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
  validation, rendering, and calls into store/worktree/git/remote/sync helpers.
- `domain/` stays pure: no SQLite, filesystem paths, Git, subprocesses, or
  command rendering. Houses the tk vocabulary types and the
  infrastructure-free sync contract types shared by `store/`, `remote/`,
  and `sync/`.
- `proc/` captures subprocess output for callers to classify. Commands should
  not stream Git or Backend Adapter subprocess output directly to user writers.
- `git/` classifies Git discovery outcomes and keeps shared Git diagnostic
  phrasing out of command modules.
- `store/` owns Repository Store opening, migrations, current-state reads and
  writes, Display ID / Alias resolution, sequence allocation, and Mutation Log
  persistence. `store/sync.zig` exposes the SQL helpers the sync engine and
  the `tk sync` / `tk remote` commands compose against.
- `remote/` owns the type-erased Backend Adapter trait (mirroring
  `proc.Runner`), the factory that dispatches by configured backend kind, and
  the FakeAdapter used by engine tests. It imports `store/`, `proc/`, and
  `domain/` but never `sync/`.
- `sync/` owns the engine that composes Adapter Pull and Apply with the
  store's sync helpers. Single entry point `sync.engine.runSync`; the engine
  reaches the database only through `store/sync.zig` helpers, never via raw
  SQL.
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

Query subcommands may overload `0` and `1` as a yes/no result code in the
style of `diff -q` or `grep -q` — for example, `tk self-update --check`
exits `1` to mean "newer release available", not "command failed". Real
failures from these subcommands still surface on stderr; scripts that need
to distinguish "newer" from "broken" check whether stderr is empty. Each
such command spells the convention out in its own help text.

## Repository Store Contracts

The Repository Store is SQLite, per ADRs
[0001](./docs/adr/0001-untracked-repository-store.md),
[0003](./docs/adr/0003-use-current-state-store-with-mutation-outbox.md), and
[0005](./docs/adr/0005-use-sqlite-for-the-repository-store.md). `tk init`
creates it at `<git-common-dir>/tk/tk.db`; later commands open that store
through the shared opener instead of duplicating discovery and validation.

Migration SQL files are the source of truth for exact table columns and checks.
Important stable contracts:

- `schema_migrations` and `PRAGMA user_version` track migrations.
- `PRAGMA application_id = 0x544B4442` identifies tk stores.
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
  and read views exist; command surface is tracked by `tk-19`.
- `mutations` stores durable backend intent with a monotonic Mutation Sequence,
  state, JSON payload, and optional Mutation Failure JSON. The persisted
  failure JSON shape is `{"detail": "..."}` ([ADR
  0009](./docs/adr/0009-sync-failure-taxonomy.md)); `tk-11` graduates this
  into a typed discriminated union.
- `remotes` and `sync_cursors` hold the v1 singleton Remote model.
- `store_config.display_prefix` controls newly generated local Display IDs.
  Custom prefix configuration is tracked by `tk-22`.

Write commands use `BEGIN IMMEDIATE` and commit current-state changes together
with any required Mutation appends. Origin gates Mutations: Local items update
current state only until Promotion; Backend items append backend-applicable
Mutations in the same transaction as the visible state change. Priority remains
a Local Field and does not emit Mutations.

`done` is terminal in v1 per
[ADR 0006](./docs/adr/0006-done-is-terminal-in-v1.md). Store-facing status
changes route through `setItemStatus`, and the schema trigger backstops future
writers.

## IDs

Items have random opaque internal stable IDs. Display IDs and Aliases are the
user-facing lookup keys and are globally unique across Tickets and Epics.

Local Display IDs use the stored Repository Store prefix plus one shared
sequence for Tickets and Epics: `<store-prefix>-<n>`. The prefix identifies the
local repository context, not item class. Containment lives in `items`, not in
Display ID structure.

## Workspace Scope and Worktrees

Workspace Scope is local-only and stored in git Worktree Config in v1. Worktree
Config scope takes precedence over read-only branch-name inference. Ticket
Branches use `tk/<display-id>-<slug>` so scope remains inferable after manual
worktree creation and after Promotion through Aliases.

`tk worktree start` creates a scoped git worktree and marks the item `active` by
default unless `--no-status` is used. The default path layout is recorded in
[ADR 0007](./docs/adr/0007-default-worktree-path-layout.md). Missing preflight
checks are tracked by `tk-15`; configurable path layout is tracked by
`tk-16`.

Workspace Scope is a selection context, not an implicit item target. Commands
that inspect, update, or promote a specific item require explicit Display IDs;
agents should pass IDs selected by `tk next` or `tk list`.

## Release Targets

`tk` ships prebuilt binaries for six target triples produced by a `zig build
release` step that cross-compiles from a single `ubuntu-latest` runner with
Zig 0.16.0 pinned exactly:

- `x86_64-linux-musl` and `aarch64-linux-musl` — fully static
- `x86_64-linux-gnu` — dynamic glibc, floor `2.28`
- `aarch64-macos` — dynamic `libSystem`, `-mmacos-version-min=11.0`
- `x86_64-windows-gnu` and `aarch64-windows-gnu` — `-static-libgcc` with
  dynamic `msvcrt`

Cross-compile rationale, linkage choices, the L2 reproducibility level, and
the best-effort policy for `aarch64-windows-gnu` are recorded in [ADR
0011](./docs/adr/0011-single-host-cross-compile-release.md). Smoke
verification runs the cross-compiled artifact on a matching native GHA runner
through a minimal `tk init / add / list` scenario; smoke failure (or runner
unavailability for `windows-11-arm`) gates artifact upload, so a given GitHub
Release may omit `aarch64-windows-gnu`.
