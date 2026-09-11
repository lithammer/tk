## Starting Work

Follow the user's request. Track work in tk. Active Items do not identify
who owns the work.

Choose with `tk next --plan` when the Plan has members, otherwise `tk next`.
Run `tk start <id>` before working the chosen Item.

Inspect with `tk show <id>` or `tk list`; pause with `tk stop <id>`.
Run `tk prime` after compaction or a new agent session.

## Capturing and Updating Work

Capture local work with enough context for a fresh session.

```sh
tk add -F -
tk add --bug -F -
tk add --epic -F -
tk add --parent <epic-id> -F -
tk update <id> --title "New title"
tk update <id> --body-file -
```

For `tk add`, the first paragraph is the title; the rest is the body.

## Blocking Work

```sh
tk block <blocked-id> <blocking-id>
tk unblock <blocked-id> <blocking-id>
```

Blocking affects `tk next` and `tk list --ready`.

## Working the Plan

```sh
tk plan
tk plan add <id> [<id>...]
tk plan remove <id> [<id>...]
tk plan clear
tk next --plan
```

`tk plan` shows all members, ignoring `TK_SCOPE`. Edits preserve Ticket state;
`clear` removes even unfinished members. Outside Dependencies still block
selection. No ready Ticket does not mean finished; selection never falls back
to unplanned work.

## Working in a Scope

```sh
tk next <epic-id>
tk list <epic-id>
```

An Epic argument or `TK_SCOPE=<epic-id>` limits work to the Epic and its child
Tickets. `tk next --plan` intersects that Scope. Item commands need explicit
IDs.

## Finishing Work

Check `git status --short` and `git diff --check`; preserve unrelated changes.
Verify the change or explain skipped checks. Close with `tk done <id>`;
capture follow-ups with `tk add`. Report commit/push status. Push only when
the user asks.
