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
                           types: MutationPayload, BackendEdit, BackendCreate,
                           directional Backend read and outcome contracts, and the
                           Promotion contract types: BackendBinding,
                           PromotionCapabilities, PromotionGraph,
                           PromotionPlan)
  git/                     Git subprocess discovery façade
  proc.rs                  subprocess runner trait and fakes
  promotion/               Promotion engine: the `tk promote` preflight planner
                           over a Repository Store snapshot (sibling of
                           sync.rs)
  remote/                  Backend Adapter trait, factory, and FakeAdapter;
                           GitHub owns typed GraphQL operations and a private,
                           replaceable wire transport under remote/github/
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
  and `sync`. Its relationship planner classifies the resulting graph and
  builds ordered Mutation drafts for both Promotion and Re-Adopt.
- `proc.rs` captures subprocess output for callers to classify. Commands should
  not stream Git or Backend Adapter subprocess output directly to user writers.
- `git/` classifies Git discovery outcomes and keeps shared Git diagnostic
  phrasing out of command modules.
- `store/` owns Repository Store opening, migrations, current-state reads and
  writes, Display ID / Alias resolution, sequence allocation, and Mutation Log
  persistence. A Mutation Log read projected onto an item read — the
  `tk list` / `tk search` row markers, and the per-Item Mutation list
  `tk show` renders — belongs beside that item read in `store/repository/`;
  the log-oriented views belong to `store/sync.rs`, which owns Remote
  configuration, retained Backend cohort validation, canonical Adopt
  insertion, Re-Adopt, Pull refresh, and Mutation Log replay and inspection
  helpers. The Store also owns atomic Detach, Former Backend Identity
  provenance, and the uniqueness invariant spanning active and former Backend
  ownership ([ADR 0047](./docs/adr/0047-detach-retains-reversible-backend-history.md)).
  `store/promotion.rs` exposes the SQL half of `tk promote` — the preflight
  graph read, the one-transaction outbox commit, receipt application, and the
  post-sync Mutation Log reads. Promotion recovery also lives here:
  unique-target lookup, queue-wide mapping capture, and atomic reconcile/retry
  transitions. `commands/promote.rs` owns Backend inspection, snapshot
  comparison, recovery guidance, and the nested sync report.
- `remote/` owns the type-erased Backend Adapter trait (mirroring
  `ProcRunner`), the factory that dispatches by configured backend kind, and
  the FakeAdapter used by engine tests. It imports `store/`, `proc`, and
  `domain/` but never `sync`.
- `sync.rs` owns the engine that composes Adapter Pull and Apply with the
  store's sync helpers. Single entry point `sync::run_sync`; the engine
  reaches the database only through `store/sync.rs` helpers, never via raw
  SQL.
- `promotion/` owns Promotion-specific analysis of the `tk promote` preflight
  graph and composes the shared domain relationship planner: first it derives
  the required Backend capability facets, then
  `promotion::plan::plan_promotion` consumes the resolved capabilities. Neither
  step reaches a database, subprocess, or Backend Adapter. The command always
  asks the Adapter to resolve the typed requirements between those steps; the
  Adapter may read the Backend only for a requested dynamic facet.
  `store/promotion.rs` produces the `PromotionGraph` the analysis consumes and
  commits the `PromotionPlan` it returns; the dependency runs one way —
  `store/` does not import `promotion/`.
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
  `schema_migrations` is authoritative and `PRAGMA user_version` mirrors it;
  both are written in the migration's own transaction.
- A Store Backup is written to `<git-common-dir>/tk/backups/` before any
  pending migration runs against a store that already has a schema, and the
  newest ten are kept ([ADR-0048](./docs/adr/0048-migrations-back-up-the-repository-store-first.md)).
  The directory is derived from the connection's path, so an in-memory store
  writes none. A backup that cannot be written refuses the migration.
- `PRAGMA application_id = 0x544B4442` identifies tk stores.
- Connections enable foreign keys and a busy timeout; `tk init` enables WAL.
- Every write transaction begins `IMMEDIATE` (`store::write_transaction`),
  so parallel writers queue on the busy timeout. A deferred read-then-write
  transaction would instead fail with `SQLITE_BUSY` the moment another
  writer commits first — the busy timeout never covers a snapshot upgrade.
  Guarded by `tests/concurrency.rs`; there is no internal retry/backoff
  layer, and the "Repository Store is busy; retry the command" stderr line
  is the backstop for a writer that holds the lock past the timeout.
- `items` stores current Ticket/Epic state. Current state is the read model;
  the Mutation Log is an outbox, not an event-sourced source of truth.
- `items.work_state` is a Local Field. `tk start` and `tk stop` write it
  directly; `tk done` and Backend Pull may clear it only when closing an Item.
  Work State itself is never recorded as a Mutation (ADR-0043).
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
  failure JSON is a typed record carrying detail, classification, and an
  optional retry hint ([ADR 0009](./docs/adr/0009-sync-failure-taxonomy.md)).
  `domain::mutation_state::MutationState` carries the transition table, and
  `store::mutations::transition` is the only runtime writer of the `state`
  column: it refuses an edge the table omits and owns the `failure_json` and
  `state_changed_at` bookkeeping each edge implies, so a workflow contributes
  only the domain preconditions it names its own diagnostics for. A migration
  runs raw SQL through `apply_one_txn` and has no `transition` to call, so it
  may write `state` directly only along an edge the transition table names and
  must cite that row in its SQL (ADR-0043).
  A Mutation optionally carries the Promotion
  Operation grouping every Mutation one `tk promote` invocation appended, so
  the command can ask whether its whole operation resolved ([ADR
  0036](./docs/adr/0036-promotion-intent-precedes-backend-capability.md)).
  `applying` is durably written before non-idempotent Backend
  creation. An `applying` Mutation is excluded from automatic replay and is a
  global barrier for Pull, Apply, Adopt, and Remote clear; Repository Store
  local edits remain available, and later Promotion intent may be committed
  behind it without applying. Explicit reconcile or retry resolves only the
  earliest global nonterminal Mutation. Reconciliation
  applies identity, optional forced convergence, Mutation state, and monotonic
  cursor movement in one transaction ([ADR
  0037](./docs/adr/0037-promotion-recovery-is-explicit-and-ordered.md)).
  `cancelled` records intent Promotion Cancellation withdrew: an
  operation-wide exit that opens no Adapter, so it is exempt from that ordering
  rule. The `skipped` clause is Mutation-Type-restricted in the same schema, so
  a Promotion never reaches it; whether a Promotion Operation has
  resolved asks the nonterminal set rather than "not applied" ([ADR
  0038](./docs/adr/0038-promotion-cancellation-withdraws-an-operation.md)).
  Withdrawing an `applying` Promotion records `abandoned` rather than
  `cancelled`, restricted by its own CHECK clause to Promotion Mutation Types,
  because tk recorded no Backend identity for that creation and may have left an
  object it cannot address ([ADR
  0039](./docs/adr/0039-cancellation-withdraws-an-unobserved-promotion.md)).
- `Store::lock_remote_workflow` owns an exclusive OS lock on the stable
  `<git-common-dir>/tk/remote.lock` file. Sync, Adopt, Promotion, and Remote
  configuration hold one guard across Backend access and Store persistence;
  nested Promotion sync reuses its caller's guard. The lock closes the live
  process check-then-act race, while the durable `applying` state remains the
  crash-recovery barrier. Lock contention fails immediately with retry
  guidance instead of blocking indefinitely. Sync Skip, Promotion
  Cancellation, and Detach share the guard but reach no Adapter — they rewrite
  Backend identity or Mutation Log state a concurrent sync could be draining,
  and each commits in one transaction. Sync Log and other local commands stay
  unlocked.
- `remotes` and `sync_cursors` hold the v1 singleton Remote model.
- `store_config.display_prefix` controls newly generated local Display IDs.
  Custom prefix configuration is tracked by `tk-22`.

Write commands use `BEGIN IMMEDIATE` and commit current-state changes together
with any required Mutation appends. Backend Binding gates Mutations
([ADR 0036](./docs/adr/0036-promotion-intent-precedes-backend-capability.md)):
an item appends backend-applicable Mutations in the same transaction as the
visible state change once it is backend-bound — already a Backend item, or a
Local item whose Promotion is durable in the Mutation Log, whose later
Mutations are therefore ordered behind that Promotion. A Local item with no
Promotion intent updates current state only. Priority remains a Local Field and
does not emit Mutations.

`done` is terminal in v1 per
[ADR 0006](./docs/adr/0006-done-is-terminal-in-v1.md). `items.status` changes
through `tk done` and Backend Pull; a Backend-bound `tk done` appends
`set_item_status`. `items.work_state` changes through `tk start`, `tk stop`,
and the clear-on-close in `tk done` or Backend Pull. The schema trigger
backstops the terminal Lifecycle rule for future writers, and carries two
exceptions today: exact Re-Adopt imports the Backend's Lifecycle in the single
`items` update that rebinds a Local Item onto its own Former Backend Identity
([ADR 0047](./docs/adr/0047-detach-retains-reversible-backend-history.md)).
The second admits Sync Skip's relinquished close, and is the wider of the
two: while an Item carries a `failed` `set_item_status` targeting `done`, any
`done` -> `open` write on that Item is admitted, not only the reopen Sync Skip
performs alongside marking that Mutation skipped
([ADR 0046](./docs/adr/0046-sync-skip-relinquishes-a-failed-close.md)).

## IDs

Items have random opaque internal stable IDs. Display IDs and Aliases are the
user-facing lookup keys and are globally unique across Tickets and Epics.
Former Backend Identities are canonical historical keys, not resolver entries;
they remain globally reserved to their stable Item. Each Backend Binding also
records the local Display ID it displaced so Detach can restore it exactly.

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
