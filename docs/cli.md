# Ticket CLI

This is the v1 command surface for `tk`.

## Global Rules

- Any item ID argument resolves a current Display ID or Alias.
- Workspace Scope constrains scope-aware commands such as `tk next`; it is not an implicit item target.
- Commands that inspect, update, or promote a specific item require an explicit Display ID. Optional `[id]` forms are filters, not Workspace Scope fallbacks.
- New Tickets and Epics are local by default.
- Commands that affect upstream state or sync repair stay explicit.

## Setup

```sh
tk init
```

Initializes the Repository Store.

## User-Observable Phrasing

User-observable diagnostic and status substrings (for example, `Initialized Repository Store at `, `not a Ticket Repository Store`, `newer Ticket version`) are canonicalised in `src/messages.zig`. Source-side call sites build their format strings by `++`-concatenating those constants with the formatting suffix they need, and tests reference the same constants via `messages.<name>`. Any new user-visible phrasing should be added there rather than hardcoded at the call site.

## Agent Briefing

```sh
tk prime
```

Prints static command-owned Markdown embedded from `src/commands/prime.md`.

## Create

```sh
tk add [--bug | --epic] [--parent <epic-id>] [--priority P0..P4] (-m <paragraph>... | -F <file | ->)
```

Creates a local task Ticket by default.

- `--bug` creates a bug Ticket.
- `--epic` creates an Epic.
- `--bug` and `--epic` are mutually exclusive.
- `--parent <epic-id>` places a new Ticket under an Epic in v1.
- `--parent` and `--epic` are mutually exclusive. Epics cannot contain other Epics.
- `--priority` sets local-only Priority. Default is `P2`.
- `-m/--message` is repeatable and follows git-commit-style paragraph joining.
- `-F/--file` reads the message from a file, or from stdin with `-F -`.
- With no message or file, `tk add` opens editor mode.

The first paragraph becomes the title. Later paragraphs become the body.

## Read

```sh
tk list [--ready | --blocked | --active] [--local | --remote]
tk next
tk show <id>
```

`tk list` renders a tree:

- Epics are top-level rows.
- Child Tickets are nested under their Epic.
- Unparented Tickets are top-level rows.
- Rows use decorative tree glyphs for child items, such as `├──` and `└──`.
- Rows are not column-aligned; each field is separated by one space.
- Rows do not render Origin as a separate field. Local or Backend origin is
  normally inferred from the Display ID shape.
- Top-level rows are ordered by creation order. Each Epic's child Tickets are
  ordered by creation order. Priority is displayed but does not sort `tk list`.
- `tk list` defaults to open and active items. Done-item browsing is deferred
  until there is a concrete workflow for limiting or windowing old completed
  work.
- Whether `tk list` defaults to Workspace Scope is deferred from v1.
- `--ready` shows only open Tickets with no unresolved Dependencies or External Blockers, keeping the tree shape and including non-empty Epics as containers. Active Tickets are not ready. Parent Epic status does not hide otherwise ready child Tickets.
- `--blocked` shows open or active Tickets with unresolved Dependencies or External Blockers, keeping the tree shape and including non-empty Epics as containers. Epics are not selected as blocked work in v1.
- `--active` shows active Tickets and Epics, keeping the tree shape and including Epics as containers for active child Tickets even when the Epic itself is not active.
- `--ready`, `--blocked`, and `--active` are mutually exclusive.
- `--local` shows only Local Tickets and Local Epics.
- `--remote` shows only items that have been promoted.
- `--local` and `--remote` are mutually exclusive and may be combined with one of the readiness filters.

These filters use the stored Origin. `--remote` may be empty before Promotion
or Backend Pull has introduced backend-origin items.
When a readiness filter and an Origin filter are combined, the filters compose
as an AND. Container Epics must pass the Origin filter to render. If an Origin
filter hides an Epic while one of its child Tickets still matches, the matching
child Ticket renders as a top-level row.

The plain output row shape is:

```text
[tree-prefix] <status-marker> <display-id> [<blocked-marker>] <priority-marker> <priority> [<kind-marker>] <title>
[tree-prefix] <status-marker> <display-id> [<blocked-marker>] [epic] <title>
```

Status markers are `○` for `open`, `◐` for `active`, and `✓` for `done`.
Ticket rows render the priority marker as `●` in plain output. Epic rows do
not render Priority because Priority belongs to Tickets.
Rows with unresolved Dependencies or External Blockers render blocked marker
`⊘` after the Display ID. The blocked marker is an overlay, not an Item Status.

`[epic]` is shown for Epics, `[bug]` is shown for bug Tickets, and task
Tickets omit a kind marker.
Titles render as-is on one line; `tk list` does not truncate or wrap them.
This slice does not introduce a new escaping policy for stored titles.

`tk list` ends with a separator, a rendered item count by Item Status, and a
status legend. Filtered views count rendered rows, including Epics retained as
containers for matching child Tickets:

```text
--------------------------------------------------------------------------------
Total: 3 items (2 open, 1 active)

Status: ○ open  ◐ active  ✓ done
Blocked: ⊘ blocked
```

An empty `tk list` result is exit code `0`. The default view prints
`No open or active items.` Filtered views print the matching empty message:
`No ready items.`, `No blocked items.`, `No active items.`, `No local items.`,
or `No remote items.`

`tk next` selects only ready Tickets, never Epics. It picks the ready Ticket
with lowest local-only Priority, then lowest Repository Store `created_seq`,
within the active Workspace Scope. Backend Tickets use local import order for
this tie break, not backend-native creation time. Ticket Kind does not affect
ordering. Selection is deterministic and does not randomize among candidates.
Assignees are not readiness or ordering inputs; Assignee support is deferred
and may be omitted entirely.

`tk next` does not explain skipped candidates or ranking reasons. Use `tk list
--ready`, `tk list --blocked`, or `tk show <id>` for inspection.

`tk next` does not filter by Origin. Local Tickets and Backend Tickets compete
in the same ready-work ordering.

`tk next` does not use Mutation Log, Mutation Failure, or Sync Cursor state as
readiness inputs, and it does not emit sync-health warnings. Sync health is
inspected through `tk sync log`.

`tk next` is read-only. It does not change Item Status; `tk worktree start`
starts work and marks a Ticket active by default.

If there is no active Workspace Scope, `tk next` searches all ready Tickets.
If Workspace Scope is a Ticket, `tk next` selects that Ticket only when it is
ready. If Workspace Scope is an Epic, `tk next` searches direct child Tickets
within that Epic. Workspace Scope resolves through the same Display ID and
Alias resolver as item ID arguments, so Promotion does not break old scope
references.

`tk next` does not accept a positional scope argument. Scoped selection comes
only from active Workspace Scope.

Parent Epic status does not hide otherwise ready child Tickets. A ready child
Ticket under a done Epic may still be selected. When scoped to an Epic, `tk
next` applies readiness to each direct child Ticket; Dependencies and External
Blockers on the Epic itself do not block those children.

The plain output shape is one Display ID, so scripts can use
`id=$(tk next)` and fetch details through `tk show "$id"`:

```text
<display-id>
```

`tk next` has no JSON or structured-output mode in v1. The Display ID line is
the machine interface.

If no ready Ticket matches repository-wide selection, `tk next` exits `1` and
writes `tk next: no ready Tickets` to stderr. If Workspace Scope was applied,
it instead writes `tk next: no ready Tickets in Workspace Scope`.

`tk next` is flagless in v1. Global ready-work inspection uses `tk list
--ready`.

`tk show <id>` shows one Ticket or Epic by Display ID or Alias.

## Update

```sh
tk update <id> [--priority P0..P4] [--parent <epic-id> | --no-parent] (-m <paragraph>... | -F <file | ->)
```

Updates title/body, local-only Priority, or Epic membership.

- `-m/--message` and `-F/--file` use the same message parsing as `tk add`.
- `--priority` changes local-only Priority.
- `--parent <epic-id>` moves a Ticket under an Epic in v1.
- `--no-parent` removes Epic membership.
- `--parent` and `--no-parent` are mutually exclusive.
- `--parent` and `--no-parent` are errors when the target is an Epic. Epics cannot contain other Epics.

## Lifecycle

```sh
tk start <id>
tk stop <id>
tk done <id>
```

- `tk start` marks a Ticket or Epic active.
- `tk stop` moves active work back to open.
- `tk done` marks a Ticket or Epic done.

## Blocking

```sh
tk block <blocked-id> <blocking-id>
tk unblock <blocked-id> <blocking-id>
```

Blocking affects `tk next`, `tk list --ready`, and `tk list --blocked`.

Dependencies may connect Tickets and Epics in any blocking or blocked combination, but cycles are rejected.

External Blocker CLI is deferred from the initial blocking command surface, but the Repository Store models External Blockers separately from Dependencies.

Successful Dependency changes print one line:

```text
Added Dependency: <blocked-id> blocked by <blocking-id>
Removed Dependency: <blocked-id> no longer blocked by <blocking-id>
```

## Promotion

```sh
tk promote <id> [--children]
```

Promotes a Local Ticket or Local Epic through the configured Remote.

- Without `--children`, only the target is promoted.
- `--children` is valid for Epics and includes directly contained Local Tickets.
- `--children` on a Ticket is an error.
- `--children` does not follow Dependencies and is not recursive in v1.

## Sync

```sh
tk sync [--skip <mutation-id>]
tk sync log [--pending | --failed | --skipped] [id]
```

`tk sync` pulls remote state before applying pending Mutations. It applies Mutations in global sequence order and stops on the first failure.

- Failed Mutations retry on the next sync.
- `--skip <mutation-id>` marks one failed Mutation skipped and continues sync.
- v1 has no force-apply mode and no automatic conflict resolution.
- `tk sync log` inspects the Mutation Log. Default view shows pending, failed,
  and skipped Mutations; applied Mutations are recorded but not rendered by
  default (browsing applied is deferred). The filter flags
  `--pending | --failed | --skipped` narrow to one state each.
- `tk sync log [id]` accepts a Mutation Sequence (the same numeric ID used by
  `tk sync --skip`) and prints one Mutation in detail, including the typed
  payload and any recorded failure.

## Worktrees

```sh
tk worktree
tk worktree start <id> [path] [--no-status]
tk worktree set <id>
tk worktree clear
```

`tk worktree` reports the current Workspace Scope and whether its source is configured, inferred, or none.

`tk worktree start` creates a Ticket Branch, creates a git worktree, stores Workspace Scope in git worktree config, and marks the scoped item active by default.

- Ticket Branches use `tk/<display-id>-<slug>`.
- Without an explicit path, the worktree is created as a sibling path.
- `--no-status` skips marking the item active.
- `tk worktree set <id>` writes Workspace Scope to git worktree config.
- `tk worktree clear` removes configured Workspace Scope without disabling branch-name inference.

## Remote

```sh
tk remote
tk remote set github --repo <owner/name>
tk remote set jira --site <url> --project <key>
tk remote clear
```

V1 supports zero or one configured Remote.

- `tk remote` shows the configured Remote.
- `tk remote set <kind>` configures or replaces it.
- `tk remote clear` removes it when no pending remote Mutations would be orphaned.
- Authentication is delegated to backend-specific CLIs such as `gh` and `acli`.
