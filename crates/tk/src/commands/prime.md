## Finding Work

- `tk next` - Show the next ready Ticket.
- `tk show <id>` - Show Item details.
- `tk list` - List open and active Items.
- `tk list --ready` - List ready Tickets.
- `tk prime` - Refresh this briefing.

## Creating and Updating

- `tk add -F -` - Create a local Ticket from stdin: first paragraph is the
  title, the rest is the body.
- `tk add --bug -F -` - Create a local Bug Ticket.
- `tk add --epic -F -` - Create a local Epic.
- `tk add --parent <epic-id> -F -` - Create a Ticket under an Epic.
- `tk update <id> --title "New title"` - Change the title.
- `tk update <id> --body-file -` - Replace the body from stdin.
- `tk start <id>` - Required when starting work; marks the Item active.
- `tk stop <id>` - Return an Item to idle.
- `tk done <id>` - Mark an Item complete.

## Dependencies

- `tk block <blocked-id> <blocking-id>` - Add a Dependency.
- `tk unblock <blocked-id> <blocking-id>` - Remove a Dependency.

## Plan

- `tk plan` - Show the whole Plan, including done Tickets.
- `tk plan add <id> [<id>...]` - Add Tickets to the Plan.
- `tk plan remove <id> [<id>...]` - Remove Tickets from the Plan.
- `tk plan clear` - Remove all Plan membership without closing Tickets.
- `tk next --plan` - Show the next ready Ticket in the Plan and current Scope.

## Scope

- `tk next <epic-id>` - Show the next ready Ticket under an Epic.
- `tk list <epic-id>` - List an Epic and its child Tickets.
