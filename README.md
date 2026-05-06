# Ticket

Ticket is an agent-first command-line tool for managing work items through a simple local interface and pluggable issue-tracker backends. The binary is **`tk`**.

The goal is similar in spirit to Beads: make work visible to humans and agents from the command line. Ticket deliberately aims for a simpler architecture, local-first capture, and backend adapters for systems like GitHub Issues and Jira.

## Current Design

- Ticket will start as a Zig 0.16 implementation.
- **Tickets** are backend-agnostic work items.
- **Epics** group related Tickets and require explicit closure.
- New Tickets and Epics are **local by default**, even when a Primary Backend exists.
- **Promotion** explicitly converts a Local Ticket or Local Epic into a backend-backed object.
- Promotion replaces the visible local ID with the backend ID and keeps the old local ID as an alias.
- A repo can have zero or one **Primary Backend**.
- **Backend Adapters** map Ticket domain concepts to backend systems.
- Backend Adapters expose pull and apply-mutation operations; the sync engine owns ordering, cursors, retries, and failure policy.
- The **Repository Store** is shared across workspaces for one repository and is untracked local state by default.
- The Repository Store uses SQLite.
- **Workspace Scope** is local-only and lets `tk` default reads to the current Ticket or Epic, usually from a git worktree context.
- Workspace Scope is stored in git worktree config for v1, with read-only branch-name inference as a fallback.
- `tk start [id]` marks work active; `tk stop [id]` moves active work back to open.
- `tk worktree start <id> [path]` creates a Ticket branch and scoped git worktree, defaulting to a sibling worktree path.
- `tk worktree` reports or changes the current Workspace Scope stored in git worktree config.
- `tk prime` prints static Markdown embedded from [docs/prime.md](./docs/prime.md) for new or compacted sessions.
- **Mutations** record durable local intent as named domain operations so they can be synced by Backend Adapters.
- Sync pulls backend state before applying pending Mutations, applies Mutations in global sequence order, and stops on the first failure.
- Failed Mutations retry on the next sync; explicit `tk sync --skip <mutation-id>` remains visible.
- `tk sync log` inspects pending, failed, skipped, and applied Mutations.

## Resolved ADRs

- [0001: Keep the repository store untracked by default](./docs/adr/0001-untracked-repository-store.md)
- [0002: Create tickets locally by default](./docs/adr/0002-local-by-default-ticket-creation.md)
- [0003: Use a current-state store with a mutation outbox](./docs/adr/0003-use-current-state-store-with-mutation-outbox.md)
- [0004: Use Zig 0.16 for the first implementation](./docs/adr/0004-use-zig-0-16-for-the-first-implementation.md)
- [0005: Use SQLite for the repository store](./docs/adr/0005-use-sqlite-for-the-repository-store.md)

## Domain Language

The project glossary lives in [CONTEXT.md](./CONTEXT.md). Keep design documents and code aligned with that language.

Important distinctions:

- **Epic membership** groups work. **Dependency** describes blocking order.
- **Priority** is local-only in v1 and sorts `P0` before `P4`.
- **Ticket Status** is `open`, `active`, `blocked`, or `done`.
- **Epic Status** is `open`, `active`, or `done`.
- `active` means current work. **Assignee** is tracked separately.
- **Workspace Scope** is local-only and is not synced to backends.

## Open Design Areas

Open design work is tracked in [docs/design-questions.md](./docs/design-questions.md).

## Testing Direction

Ticket should use layered tests from the start:

- Zig unit tests for domain behavior and command handlers.
- Inline snapshots for small rendered outputs.
- Fake subprocess runners for Backend Adapter tests.
- txtar-based CLI scenario fixtures with a small script runner inspired by `rsc.io/script` and Rust's `trycmd`.

The CLI scenario runner should support multi-step command tests, expected stdout/stderr/exit checks, simple filesystem assertions, elision for unstable output, and an explicit snapshot update mode.

## CLI Direction

Creation should keep the common path short:

```sh
tk add -m "Update README"               # creates a task Ticket
tk add --bug -F bug-report.md           # creates a bug Ticket
tk add --epic -m "Jira backend"         # creates an Epic
tk add --bug -F - < rich-bug-report.md  # reads message from stdin
tk add --priority P1 -F -               # creates a higher-priority local Ticket
```

`tk add` uses git-commit-style message input: repeatable `-m/--message`, `-F/--file`, `-F -` for stdin, or editor mode when no message/file is provided. The first paragraph becomes the title and later paragraphs become the body. `--bug` and `--epic` are mutually exclusive. `Epic` is not a Ticket Kind.

`tk next` selects the ready Ticket with the lowest local-only Priority, then oldest creation order, within the active Workspace Scope. It does not select Epics. The default Priority is `P2`.

Priority is set with `tk add --priority P0..P4` or `tk update [id] --priority P0..P4`; v1 does not have a top-level priority command.

`tk list` defaults to a tree view: Epics are top-level rows, child Tickets are nested under their Epic, and unparented Tickets are top-level rows. `tk list --ready` keeps the tree shape and includes non-empty Epics as containers for ready child Tickets. Rows use compact status, priority, and kind markers.

Ticket's default command paths should let agents manage local work safely. Commands that affect upstream state or sync repair, such as `promote` and `sync`, stay explicit and visible.

The CLI uses `remote` for backend configuration and filters:

```sh
tk remote
tk remote set <kind>
tk remote clear
tk list --remote
```

V1 supports zero or one configured remote. Backend authentication is delegated to external CLIs such as `gh` or `acli`.
