# Design Questions

This file tracks unresolved design work before `tk` exists. Resolved questions should be promoted into [CONTEXT.md](../CONTEXT.md), an ADR, or both.

## Open

### DQ-002: What is the initial CLI command surface?

**Status**: open
**Current recommendation**: Keep intent commands short, with `tk add` creating task Tickets by default, `--bug` for bug Tickets, and `--epic` for Epics.
**Decision needed**: Finalize the v1 commands for create, update, dependency management, promotion, sync, workspace scope, and worktree handling.

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
**Decision**: Run Backend Pull before applying pending Mutations in v1. Apply pending Mutations in global Mutation Sequence order and stop on the first failed Mutation. Failed Mutations keep a structured failure and are retried by the next sync. A failed Mutation may be skipped only through an explicit command with a reason, and sync output warns when skipped Mutations exist. Conflicts are adapter-detected Mutation Failures only; v1 has no automatic merge or local conflict resolution model.
**Recorded in**: CONTEXT.md.
**Rationale**: Global ordering and stop-on-failure keep sync behavior simple and safe for v1. Pull-before-apply gives adapters fresh backend state before writing. Explicit skip prevents one permanent failure from blocking sync forever while making divergence visible.

### DQ-005: How should worktree creation and Workspace Scope inference work?

**Status**: resolved
**Decision**: Store Workspace Scope in git worktree config for v1. Configured scope takes precedence over read-only branch-name inference. Ticket-created branches use `tk/<display-id>-<slug>`. `tk start <id> [path]` begins scoped work by creating a branch and git worktree, defaulting to a sibling worktree path, and setting status active unless `--no-status` is used. Configurable worktree root/layout is deferred from v1. `tk scope` reports the active scope and whether its source is configured, inferred, or none. `tk scope set <id>` writes Worktree Config, and `tk scope clear` removes configured scope without disabling inferred scope.
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

### DQ-000: What language should the first implementation use?

**Status**: resolved
**Decision**: Use Zig 0.16.
**Recorded in**: ADR 0004.
