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
    jira.zig
  worktree/
    git.zig
  testing/
    snapshot.zig
    txtar.zig
    script.zig
```

Only add files when the slice needs them. The layout is a direction, not a scaffolding checklist.

## Boundaries

`main.zig` owns process-level concerns: argv, stdin/stdout/stderr, exit codes, allocator setup, and command dispatch.

`cli.zig` parses arguments into command structs. It should not perform filesystem, SQLite, git, or subprocess work.

Command handlers execute parsed commands against explicit dependencies:

- Repository Store
- Worktree service
- Sync engine
- Remote adapter registry
- Subprocess runner
- Clock or ID generator when needed

Domain logic should not depend on SQLite, filesystem paths, git, or subprocess execution.

## Storage

The Repository Store uses SQLite.

Current Ticket and Epic state is stored directly. The Mutation Log is an outbox for replayable backend intent, not the primary read model.

Any command that changes syncable state must update current state and append the corresponding Mutation in one SQLite transaction.

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
- txtar-based CLI scenario tests with a small script runner inspired by `rsc.io/script` and Rust's `trycmd`.

Avoid testing everything through subprocess CLI scenarios. Keep most behavior fast and local to domain or command-handler tests.

## First Slices

Implement in small vertical slices:

1. `tk prime`
   - Set up Zig build and command dispatch.
   - Print `docs/prime.md` through Zig `@embedFile`.
   - Add the first CLI scenario test.

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

6. Lifecycle and blocking
   - Implement `tk start`, `tk stop`, `tk done`, `tk block`, and `tk unblock`.
   - Enforce dependency cycle rejection.

7. Worktree scope
   - Implement `tk worktree`, `set`, `clear`, and `start`.
   - Use git worktree config.

8. Remote and sync skeleton
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
