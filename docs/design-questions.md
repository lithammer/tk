# Design Questions

This file tracks unresolved design work before `tk` exists. Resolved questions should be promoted into [CONTEXT.md](../CONTEXT.md), an ADR, or both.

## Open

### DQ-002: What is the initial CLI command surface?

**Status**: open
**Current recommendation**: Keep intent commands short, with `tk add` creating task Tickets by default, `--bug` for bug Tickets, and `--epic` for Epics.
**Decision needed**: Finalize the v1 commands for create, update, dependency management, promotion, sync, workspace scope, and worktree handling.

### DQ-003: What is the Backend Adapter interface?

**Status**: open
**Current recommendation**: Backend Adapters should call external CLIs such as `gh` and `acli` through an injectable subprocess runner.
**Decision needed**: Define the adapter contract, supported operations, receipt format, failure model, and contract test strategy.

### DQ-004: How should sync failures and conflicts be handled?

**Status**: open
**Current recommendation**: Apply Mutations in sequence and stop on the first failed Mutation for v1.
**Decision needed**: Define retry behavior, conflict reporting, manual resolution commands, and whether sync is global or per item.

### DQ-005: How should worktree creation and Workspace Scope inference work?

**Status**: open
**Current recommendation**: First-party worktree creation should write Workspace Scope automatically, with branch-name inference as a fallback.
**Decision needed**: Define worktree command behavior, scope file location, branch naming, and how `tk scope` explains inferred scope.

### DQ-006: What is the initial Mutation Type vocabulary?

**Status**: open
**Current recommendation**: Start with domain operations for creating, updating, commenting, dependency changes, epic membership, promotion, and closure.
**Decision needed**: Define the exact Mutation Types for Tickets, Epics, Dependencies, and Promotion.

### DQ-007: What is the CLI testing strategy in Zig?

**Status**: open
**Current recommendation**: Combine fast domain tests, command-handler tests with fake storage and subprocess runners, golden CLI tests, and filesystem integration tests.
**Decision needed**: Decide test harness structure, fixture format, snapshot update flow, and how to run subprocess-style tests.

## Resolved

### DQ-001: What storage backend should the Repository Store use?

**Status**: resolved
**Decision**: Use SQLite.
**Recorded in**: ADR 0005.
**Rationale**: Ticket needs atomic updates across current state and the Mutation Log, easy temp-dir testing, and queryable local state without inventing a custom storage engine.

### DQ-000: What language should the first implementation use?

**Status**: resolved
**Decision**: Use Zig 0.16.
**Recorded in**: ADR 0004.
