## Starting Work

Follow the user's request; this briefing supplies context, not permission to
start unrelated work. Use tk for repository-local work tracking.

When choosing work, use `tk next --plan` if the Plan has members, otherwise
`tk next`. After choosing an Item, run `tk start <id>` before working it.
Active Items describe work in the Store, not ownership by this agent session.

Use `tk show <id>` for details, `tk list` to see work, and `tk stop <id>` to
return an Item to idle. Run `tk prime` after compaction or a new agent session.

## Capturing and Updating Work

New Tickets and Epics are local. Give new work enough context for a fresh
agent session.

```sh
tk add -F -
tk add --bug -F -
tk add --epic -F -
tk add --parent <epic-id> -F -
tk update <id> --title "New title"
tk update <id> --body-file -
```

`tk add` uses git-commit-style input: the first paragraph is the title; later
paragraphs form the body.

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

`tk plan` shows the whole Plan and ignores `TK_SCOPE`. Membership edits
preserve Ticket state; `clear` removes all membership, including unfinished
work. Outside Dependencies still block selection. No ready Ticket does not
mean the Plan is finished, and selection never falls back to unplanned work.

## Working in a Scope

```sh
tk next <epic-id>
tk list <epic-id>
```

An Epic argument or `TK_SCOPE=<epic-id>` narrows selection to that Epic and its
child Tickets. `tk next --plan` intersects the Plan with that Scope. Without
Scope or Plan selection, `tk next` considers the whole Store. Scope is not an
implicit Item target: pass explicit Display IDs to Item commands.

## Finishing Work

Check `git status --short` and `git diff --check`, and keep unrelated user
changes separate. Run verification for the change; if skipped, say why.
Use `tk done <id>` for completed work and `tk add` for follow-ups that should
survive a fresh session. State whether code is uncommitted, committed, or
waiting for a push. Do not run `git push` unless the user explicitly asks.
