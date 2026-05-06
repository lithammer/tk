# Implementation Plan

This document bridges the design docs to the first Zig implementation. It should stay practical and change when implementation pressure proves a better shape.

## Inputs

- Domain language: [../CONTEXT.md](../CONTEXT.md)
- CLI surface: [cli.md](./cli.md)
- Design decisions: [adr/](./adr/)
- Resolved questions: [design-questions.md](./design-questions.md)
- Prime template: [prime.md](./prime.md)

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
  store/
    sqlite.zig
    migrations.zig
  sync/
    engine.zig
  remote/
    runner.zig
    github.zig
  worktree/
    git.zig
  testing/
    snapshot.zig
    txtar.zig
    script.zig
```

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
- Repository Store
- Worktree service
- Sync engine
- Remote adapter registry
- Subprocess runner
- Clock or ID generator when needed
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

Current Ticket and Epic state is stored directly. The Mutation Log is an outbox for replayable backend intent, not the primary read model.

Any command that changes syncable state must update current state and append the corresponding Mutation in one SQLite transaction.

A single command may append more than one Mutation, of different Mutation Types, in the same transaction. For example, `tk update --parent` edits Epic membership through `add_ticket_to_epic` (and `remove_ticket_from_epic` when moving between Epics), while editing title/body through `update_ticket` in the same call. Command-to-Mutation is many-to-one, not one-to-one.

Workspace Scope is not stored in SQLite in v1. It is stored in git worktree config.

## IDs

Store items by an internal stable ID. Display IDs and Aliases are lookup keys.

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

Avoid testing everything through subprocess CLI scenarios. Keep most behavior fast and local to domain or command-handler tests.

Fixture wiring uses per-test `@embedFile` so a missing fixture is a build error. If the scenario set outgrows manual test functions (roughly 30–50 fixtures), introduce a `build.zig` step that walks `tests/scenarios/` and generates a Zig manifest the runner iterates over.

The `-- script --` tokenizer follows [`rogpeppe/go-internal/testscript`](https://github.com/rogpeppe/go-internal/blob/master/testscript/testscript.go) (the maintained successor to `rsc.io/script`): whitespace splits args, `'…'` quotes a literal chunk, `''` inside a quoted chunk is a literal `'`, `#` starts a comment, and `$NAME` / `${NAME}` expand against the runner's env map. No double quotes, no backslash escapes. Output comparison is byte-exact — no whitespace normalisation. Setting `TK_UPDATE=1` rewrites the `expected/*` sections of each fixture in place, preserving section order and any non-expected sections (like `-- input/foo.md --`).

Each scenario gets a fresh per-script work directory exposed as `$WORK` in the env map (matching testscript). From slice #2 onward, the runner also passes a `std.fs.Dir` handle for `$WORK` via `Deps.cwd`. The work directory is removed unconditionally after the scenario; setting `TK_TESTWORK=1` opts in to preserving it for debugging. Failure output substitutes the literal string `$WORK` for the actual path so messages stay readable; re-run with `TK_TESTWORK=1` to keep artefacts.

## First Slices

Implement in small vertical slices:

1. `tk prime`
   - `build.zig` declares the `tk` exe, fetches zig-clap, and registers `docs/prime.md` via `b.addAnonymousImport("prime_md", .{ .root_source_file = b.path("docs/prime.md") })` so `@embedFile("prime_md")` reads its bytes. Single `zig build test` target.
   - `main.zig` implements `pub fn main(init: std.process.Init) !void`, builds `Deps { stdout, stderr, gpa }` from `init.io` and `init.gpa`, iterates argv via `init.minimal.args.iterateAllocator(init.gpa)` (skipping argv[0]), calls `runArgv`, catches any propagated error and translates it to exit 3, then translates the returned `u8` into the process exit code.
   - `cli.zig` exports `runArgv(deps, args_iter: anytype) !u8` with one `SubCommand` arm, plus top-level `--help` (stdout, exit 0) and `--version` (stdout, exit 0; `const VERSION = "v0.0.1";` lives here). Unknown subcommand and zig-clap parse failure return exit 2 with the clap diagnostic on stderr. Inline test verifies routing + exit 2.
   - `commands/prime.zig` reads `@embedFile("prime_md")`, trims trailing whitespace via `std.mem.trimEnd`, writes the bytes plus exactly one `\n` to `deps.stdout`, and returns 0. Never writes to `deps.stderr`. Works outside a `tk init`'d repo. Inline test captures output via `std.Io.Writer.Allocating`.
   - `src/testing/script.zig` ships a minimal txtar runner: parse sections, tokenize each `-- script --` line testscript-style, build a `Deps` with `std.Io.Writer.Allocating`-backed writers, dispatch through `runArgv`, byte-exact compare against `expected/stdout`/`expected/stderr`/`expected/exit`. `TK_UPDATE=1` rewrites the `expected/*` sections in place, preserving section order.
   - One fixture (`scenarios/prime/basic.txtar`) covers `tk prime` end-to-end. A second fixture (`scenarios/_harness/preserve_sections.txtar`) exercises `TK_UPDATE`'s order-preservation property by including a non-`expected/*` section that must survive a rewrite. No subprocess smoke test in slice #1 — deferred to slice #2.

2. `tk init`
   - Create the Repository Store.
   - Run SQLite migrations.
   - Add temp-dir integration tests.

3. `tk add -F -`
   - Parse git-commit-style message input.
   - Create local task Tickets with Priority `P2`.
   - Append `create_ticket` Mutation in the same transaction.

4. `tk list`
   - Render the List Tree for local Tickets and Epics.
   - Add fixture tests for Epics, child Tickets, unparented Tickets, statuses, kinds, and priorities.

5. `tk next`
   - Select ready Tickets by Priority and creation order within Workspace Scope.
   - Exclude Epics.
   - Add dependency and scope tests.

6. `tk show` and `tk update`
   - Render a single Ticket or Epic with its current fields.
   - Edit title/body, local-only Priority, and Epic membership.
   - `--parent` and `--no-parent` append `add_ticket_to_epic` or `remove_ticket_from_epic` Mutations; title/body edits append `update_ticket` or `update_epic`. A single invocation may emit more than one Mutation in one transaction.

7. Lifecycle and blocking
   - Implement `tk start`, `tk stop`, `tk done`, `tk block`, and `tk unblock`.
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
- Comments, labels, and assignees.
- Force sync or conflict resolution.
- Multiple remotes.
- Non-git Workspace Scope storage.
