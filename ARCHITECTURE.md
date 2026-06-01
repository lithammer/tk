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
crates/tk/src/
  main.rs                  process entrypoint
  cli.rs                   top-level dispatch and shared Deps
  commands/                per-command clap-derive Args and handlers
  domain/                  pure domain enums and helpers (incl. sync contract
                           types: MutationPayload, MutationView,
                           BackendItemSnapshot, ApplyOutcome)
  git/                     Git subprocess discovery façade
  proc.rs                  subprocess runner trait and fakes
  remote/                  Backend Adapter trait, factory, and FakeAdapter
  render/                  terminal-rendering subsystem (palette, styler,
                           sanitize)
  store/                   Repository Store, migrations, Mutation Log, and
                           sync helpers
  sync.rs                  sync engine orchestration
crates/tk/tests/
  scenarios.rs             CLI scenario harness (insta + assert_cmd)
```

Only add modules when a slice needs them. Prefer moving reusable behavior into a
small boundary module after the second caller proves the shape.

## Boundaries

- `main.rs` is a thin process shim. It builds real `cli::Deps`, calls
  `cli::run_argv`, maps unexpected propagated errors to exit code `3`, and does
  not own command logic.
- `cli.rs` owns top-level routing only. The `Command` enum is the single place
  to register a command module; clap derives help text and dispatch from it.
- `commands/<cmd>.rs` owns that command's clap-derive `Args` struct,
  command-specific validation, rendering, and calls into
  store/worktree/git/remote/sync helpers.
- `domain/` stays pure: no SQLite, filesystem paths, Git, subprocesses, or
  command rendering. Houses the tk vocabulary types and the
  infrastructure-free sync contract types shared by `store/`, `remote/`,
  and `sync`.
- `proc.rs` captures subprocess output for callers to classify. Commands should
  not stream Git or Backend Adapter subprocess output directly to user writers.
- `git/` classifies Git discovery outcomes and keeps shared Git diagnostic
  phrasing out of command modules.
- `store/` owns Repository Store opening, migrations, current-state reads and
  writes, Display ID / Alias resolution, sequence allocation, and Mutation Log
  persistence. `store/sync.rs` exposes the SQL helpers the sync engine and
  the `tk sync` / `tk remote` commands compose against.
- `remote/` owns the type-erased Backend Adapter trait (mirroring
  `ProcRunner`), the factory that dispatches by configured backend kind, and
  the FakeAdapter used by engine tests. It imports `store/`, `proc`, and
  `domain/` but never `sync`.
- `sync.rs` owns the engine that composes Adapter Pull and Apply with the
  store's sync helpers. Single entry point `sync::run_sync`; the engine
  reaches the database only through `store/sync.rs` helpers, never via raw
  SQL.
- `commands/scope.rs` owns Scope resolution (ADR-0022): the `<epic-id>`
  argument / `TK_SCOPE` precedence and Epic-only validation. `tk next` and
  `tk list` compose it; tk neither stores, infers, nor manages git worktrees.

`cli::Deps` carries explicit dependencies: stdout/stderr/stdin writers, cwd
path, subprocess runner, UTC millisecond clock, random source, and a resolved
`Styler` for colour output. Writers are borrowed for one command invocation and
must not be retained past return. `Deps` grows additively as slices need more
injectable boundaries.

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

## Scope

Scope ([ADR-0022](./docs/adr/0022-scope-is-an-explicit-epic-argument-not-persisted-state.md))
is the Epic that narrows `tk next` and `tk list`. It is supplied per
invocation as an explicit `<epic-id>` argument or the `TK_SCOPE` environment
variable — the argument wins — and is never persisted or inferred from git
state. `commands/scope.rs` owns the argument/`TK_SCOPE` precedence; the command
resolves the value Epic-only (a Ticket is a typed error) before the
store-facing selection runs, so the store receives an already-resolved Epic id.

Scope is a selection context, not an implicit item target. Commands that
inspect, update, or promote a specific item require explicit Display IDs;
agents should pass IDs selected by `tk next` or `tk list`.

tk does not create or manage git worktrees; `git worktree` is the user's or
harness's tool. An orchestrated / AFK run exports `TK_SCOPE=<epic-id>` so every
`tk` subprocess inherits the same Epic without restating it.

## Release Targets

`tk` ships prebuilt binaries for five target triples produced by a
`cargo zigbuild` step that cross-compiles from a single `ubuntu-latest` runner
with Zig 0.16.0 pinned exactly (Zig is the C cross-compiler/linker for the
bundled SQLite):

- `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` — fully static
- `x86_64-unknown-linux-gnu` — dynamic glibc, floor `2.28`
- `aarch64-apple-darwin` — dynamic `libSystem`, `MACOSX_DEPLOYMENT_TARGET=11.0`
- `x86_64-pc-windows-gnu` — static libgcc with dynamic msvcrt

Cross-compile rationale, linkage choices, and the L2 reproducibility level are
recorded in [ADR
0011](./docs/adr/0011-single-host-cross-compile-release.md). Smoke
verification runs the cross-compiled artifact on a matching native GHA runner
through a minimal `tk init / add / list` scenario; smoke failure gates artifact
upload, so a given GitHub Release may omit a triple whose smoke job failed.
