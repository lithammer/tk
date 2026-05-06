# Design Questions

This file tracks unresolved design work before `tk` exists. Resolved questions should be promoted into [CONTEXT.md](../CONTEXT.md), an ADR, or both.

## Open

### DQ-002: What is the initial CLI command surface?

**Status**: open
**Current recommendation**: Keep intent commands short, with `tk add` creating task Tickets by default, `--bug` for bug Tickets, `--epic` for Epics, `--parent <epic-id>` for Epic membership, and `--priority P0..P4` for local-only Priority. `tk update [id] --priority P0..P4` changes Priority; there is no top-level priority command in v1. `tk add` uses git-commit-style message input: repeatable `-m/--message`, `-F/--file`, `-F -` for stdin, or editor mode when no message/file is provided. Use lifecycle verbs `tk start [id]`, `tk stop [id]`, and `tk done [id]` instead of `tk status`. Use positional `tk block <blocked-id> <blocking-id>` and `tk unblock <blocked-id> <blocking-id>`. Use `tk sync --skip <mutation-id>` for skipped Mutations and `tk sync log` for Mutation Log inspection. Use CLI `remote` and `--remote` for backend-backed items. Use `tk worktree` rather than `tk scope` or `tk workspace` for v1 Workspace Scope inspection and control, with git worktree creation under `tk worktree start <id> [path] [--no-status]`. `tk list` defaults to a tree view with Epics as top-level rows, child Tickets nested under Epics, and unparented Tickets as top-level rows. `tk list --ready` keeps the tree view and includes non-empty Epics as containers for ready child Tickets. `tk next` selects only ready Tickets with lowest local-only Priority, then oldest creation order, within the active Workspace Scope. Any item ID argument should resolve a Display ID or Alias.
**Decision needed**: Finalize the v1 commands for reads, promotion, sync inspection, remote configuration, and worktree handling.

## Resolved

### DQ-001: What storage backend should the Repository Store use?

**Status**: resolved
**Decision**: Use SQLite.
**Recorded in**: ADR 0005.
**Rationale**: Ticket needs atomic updates across current state and the Mutation Log, easy temp-dir testing, and queryable local state without inventing a custom storage engine.

### DQ-003: What is the Backend Adapter interface?

**Status**: resolved
**Decision**: Backend Adapters expose only Backend Pull and Mutation Apply operations in v1. Backend Pull imports backend state into the Repository Store. Mutation Apply applies one pending Mutation and returns a Mutation Receipt or failure. The sync engine owns mutation ordering, Sync Cursors, retries, and failure policy. Adapters call external CLIs such as `gh` and `acli` through an injectable subprocess runner.
**Recorded in**: CONTEXT.md.
**Rationale**: A narrow adapter boundary keeps backend-specific translation separate from sync orchestration, makes adapters testable with fake subprocess runners, and avoids pushing retry/order policy into each integration.

### DQ-004: How should sync failures and conflicts be handled?

**Status**: resolved
**Decision**: Run Backend Pull before applying pending Mutations in v1. Apply pending Mutations in global Mutation Sequence order and stop on the first failed Mutation. Failed Mutations keep a structured failure and are retried by the next sync. A failed Mutation may be skipped through `tk sync --skip <mutation-id>`, and sync output warns when skipped Mutations exist. Conflicts are adapter-detected Mutation Failures only; v1 has no automatic merge, local conflict resolution model, or force-apply mode. Mutation Log inspection is handled by `tk sync log`.
**Recorded in**: CONTEXT.md.
**Rationale**: Global ordering and stop-on-failure keep sync behavior simple and safe for v1. Pull-before-apply gives adapters fresh backend state before writing. Explicit skip prevents one permanent failure from blocking sync forever while making divergence visible.

### DQ-005: How should worktree creation and Workspace Scope inference work?

**Status**: resolved
**Decision**: Store Workspace Scope in git worktree config for v1. Configured scope takes precedence over read-only branch-name inference. Ticket-created branches use `tk/<display-id>-<slug>`. `tk worktree start <id> [path] [--no-status]` begins scoped work by creating a branch and git worktree, defaulting to a sibling worktree path, and setting status active unless `--no-status` is used. Configurable worktree root/layout is deferred from v1. `tk worktree` reports the active scope and whether its source is configured, inferred, or none. `tk worktree set <id>` writes Worktree Config, and `tk worktree clear` removes configured scope without disabling inferred scope.
**Recorded in**: CONTEXT.md.
**Rationale**: Git worktree config avoids untracked per-workspace files, branch inference keeps manually created Ticket branches usable, and `tk start` provides a single intent command for beginning scoped work while still making git worktrees visible.

### DQ-006: What is the initial Mutation Type vocabulary?

**Status**: resolved
**Decision**: Use `create_ticket`, `update_ticket`, `set_ticket_status`, `create_epic`, `update_epic`, `set_epic_status`, `add_ticket_to_epic`, `remove_ticket_from_epic`, `add_dependency`, `remove_dependency`, `promote_ticket`, and `promote_epic`.
**Recorded in**: CONTEXT.md.
**Rationale**: The v1 set covers creation, title/body edits, status, epic membership, dependencies, and promotion while deferring comments, labels, and assignees. `update_ticket` and `update_epic` cover title/body only. Epic membership is limited to Tickets with no nested Epics. Dependencies may connect Tickets and Epics in any blocking or blocked combination, but cycles are rejected.

### DQ-007: What is the CLI testing strategy in Zig?

**Status**: resolved
**Decision**: Use Zig unit tests for domain and command-handler behavior, inline snapshots for small rendered outputs, fake subprocess runners for adapters, and txtar-based CLI scenario fixtures with a small script runner inspired by `rsc.io/script` and Rust's `trycmd`.
**Recorded in**: README.md.
**Rationale**: txtar keeps multi-file CLI scenarios reviewable as one text fixture, the script runner supports multi-step command behavior, and trycmd-style output comparison plus update mode gives useful CLI snapshot tests without introducing a TOML schema first.

### DQ-008: Should Ticket provide an agent briefing command?

**Status**: resolved
**Decision**: Include `tk prime` in v1. It prints static Markdown embedded from `docs/prime.md` with Zig `@embedFile`.
**Recorded in**: CONTEXT.md and docs/prime.md.
**Rationale**: A static embedded briefing is easy to review, simple to implement, and still gives agents a consistent context recovery workflow. Dynamic repository state can be added later without changing the command shape.

### DQ-000: What language should the first implementation use?

**Status**: resolved
**Decision**: Use Zig 0.16.
**Recorded in**: ADR 0004.
