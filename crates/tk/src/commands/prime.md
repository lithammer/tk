# tk Workflow Context

Run `tk prime` after compaction, clear, or a new agent session.

## Core Rules

- Use tk for repository-local work tracking.
- New Tickets and Epics are local by default.
- Use `tk add` for self-contained local Tickets with enough context for a fresh
  agent session.
- Promotion and sync are explicit, human-visible operations.
- Use `tk next` to choose agent work.
- Scope `tk next` / `tk list` to an Epic with `tk next <epic-id>` / `tk list <epic-id>` or the `TK_SCOPE` environment variable; absent a Scope they cover the whole store.
- Scope is not an implicit item target; pass explicit Display IDs to item commands.
- Use `tk sync log` to inspect pending, failed, skipped, and applied Mutations.
- Do not run `git push` unless the user explicitly asks for it.

## Session Start

```sh
tk prime
tk next
```

## Definition of Done

Before saying work is complete:

```sh
git status --short
git diff --check
tk sync log
```

- Inspect repo state and keep unrelated user changes separate.
- Run the narrow verification for the change; if skipped, say why.
- Use `tk done <id>` for completed scoped work.
- Use `tk add` for follow-ups, deferred decisions, or context that should
  survive a fresh session.
- Surface pending, failed, or skipped Mutations from `tk sync log`.
- State whether code is uncommitted, committed, or waiting for an explicit push.

## Essential Commands

### Find Work

```sh
tk next
tk list
tk list --ready
tk show <id>
```

### Create Work

```sh
tk add -F -
tk add --bug -F -
tk add --epic -F -
tk add --parent <epic-id> -F -
```

`tk add` uses git-commit-style message input. The first paragraph becomes the
title. Later paragraphs become the body.

### Update Work

```sh
tk update <id> -F -
tk start <id>
tk stop <id>
tk done <id>
```

### Blocking

```sh
tk block <blocked-id> <blocking-id>
tk unblock <blocked-id> <blocking-id>
```

Blocking affects `tk next` and `tk list --ready`.

### Scope

```sh
tk next <epic-id>
tk list <epic-id>
```

Pass an Epic to narrow `tk next` / `tk list` to that Epic and its child
Tickets, or export `TK_SCOPE=<epic-id>` to scope a whole session. tk does not
create or manage git worktrees; use `git worktree` directly.

### Human Curation

```sh
tk promote <id>
tk sync
tk sync --skip <mutation-id>
tk remote
```

Agents should surface promotion, sync failures, and skipped Mutations rather
than quietly repairing upstream state.
