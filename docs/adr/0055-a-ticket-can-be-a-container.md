# A Ticket can be a Container

A Ticket may hold Tickets, as an Epic does. The two cases share one
relationship, **Containment**, and the item that holds a Ticket is its
**Container**: an Epic or a Ticket. **Epic Membership** is retired. A Ticket
that holds Tickets waits on them in `tk next`, and nesting stops at Epic,
Ticket, Ticket.

GitHub allows sub-issues on any issue type, and the trees agents build there
hang from Tasks. Matt Pocock's `to-tickets` skill files each slice as a
sub-issue of the spec issue it came from, and `wayfinder` keeps its map as a
labelled issue whose tickets are sub-issues. Until now only an Epic could hold
anything, so such a tree had no local shape. ADR-0054 made the `Epic` issue
type required, which is what lets GitHub tell an Epic from a Task with
sub-tasks.

## Decision

### One relationship, one Container per Ticket

A Ticket has zero or one Container. An Epic has none. Containment stays a
property of the Ticket, as ADR-0021 made Epic Membership: changing it is a
Ticket Mutation, and clearing it names no Container. A Ticket cannot hold both
an Epic and a parent Ticket, since GitHub gives an issue one parent
(`Issue.parent` is a single `Issue` in the GraphQL schema). A held Ticket's Epic
is therefore its Container's Container. Ticket Kind plays no part: a `bug` may
hold Tickets, and a `task` may hold `bug` Tickets.

### Nesting stops at Epic, Ticket, Ticket

A Ticket held by a Ticket holds nothing. A Ticket that holds Tickets has no
Container except an Epic. GitHub allows eight levels of sub-issues. Jira allows
one level below a standard work item: "a subtask can't have any child work
items", and a work item that has subtasks cannot become one. Lifting the cap
later breaks no stored data; adding one later would mean migrating every tree
that already breaks it.

A CHECK constraint cannot span rows, and the deferred foreign key fails only
at commit, so the write that sets the Container enforces the cap, inside its own
transaction. Checking where the Parent Argument resolves would leave a window
between the check and the write for a concurrent `tk update` to open a third
level.

### A Ticket Container waits on what it holds

A Ticket is ready only when every Ticket it holds is `done`, whatever that
Ticket's Selection State. Each held Ticket acts as a Blocking Item of its
Container. Pocock's `to-spec` docs name the reason: an agent that picks the
parent spec tries to build all of it in one run instead of taking the slices,
"the most-reported problem with the skill". Once the Tickets it holds are
done, the Container is ready, and its work is to check the whole and close it.

The same edge counts against cycles. `tk block` refuses a Dependency, and the
Parent Argument refuses a Container, when the result would make a Ticket wait
on itself: `tk block A1 A` where A holds A1, or A1 waiting on B while B waits
on A. A check that walks Dependencies alone misses both, and readiness has no
guard of its own, so every Ticket in the loop would wait forever. An Epic adds
no edge, since it never waits.

Effective Priority follows the same edge. A Ticket Container counts as a
Blocked Item of each Ticket it holds, so a P0 Container lifts the P3 Tickets
it holds to Effective P0. An Epic has no Priority and keeps its ADR-0015 rule: a Ticket
that blocks an Epic takes the lowest Effective Priority of the Epic's
unfinished Tickets, and a Ticket held by an Epic takes nothing from it.

Nothing flows the other way. A Container's Item Status, Selection State,
Dependencies and External Blockers never change the readiness of what it
holds, which widens the existing Epic rule to every Container. Closing a
Container with open Tickets neither refuses nor warns, and closes nothing
else; GitHub does not cascade a close to sub-issues either.

### Scope is any Container

`tk next`, `tk list` and `TK_SCOPE` accept an Epic or a Ticket that holds
Tickets. A Ticket that holds none is still a typed error, since ADR-0022's
reason stands: scoping to it selects one item or nothing. A Scope covers its
Container and every Ticket it holds, directly or through a Ticket it holds, so
an Epic Scope reaches the Tickets that now carry its work. Scoping to a Ticket
Container answers `wayfinder`'s frontier query.

Under `tk next --plan`, a held Ticket outside the Plan blocks its Container the
way an outside Dependency blocks its Blocked Item, and `tk plan` names it.

### Views nest every Container

The List Tree nests held Tickets under any Container, as deep as Containment
goes, and shows a Container as grouping context when a Ticket it holds matches
but it does not. `tk show` renders an item's Container and the Tickets it
holds; a Ticket may now have both.

### Promotion keeps one level

`tk promote <id> --children` promotes the local Tickets that a Container holds
directly, for an Epic or a Ticket. `tk promote <epic> --children` therefore
leaves the Tickets each of its Tickets holds local, and their Containment stays local,
as mixed-Origin Containment does under ADR-0035, until each Ticket Container
is promoted with `--children`.

### Mutations set and clear a Container

`add_ticket_to_epic` and `remove_ticket_from_epic` become
`set_ticket_container` and `clear_ticket_container`, matching
`set_ticket_priority` and `set_ticket_effort`. A move is one
`set_ticket_container`, not a remove-then-add pair: `gh issue edit
--parent` in gh 2.100.0 sends `addSubIssue` with `replaceParent: true`, so one
call moves the issue. `--remove-parent` sends `removeSubIssue` against the
current parent, and sends nothing when there is none, so a replayed clear is
harmless.

### Containment stays push-only

Backend Pull reads no parent, as ADR-0021 and ADR-0049 decided for every
relationship. gh-66 owns read-back for edges whose two ends are both Adopted,
and Containment joins Dependencies in its scope.

## Considered Options

**A Sub-ticket relationship beside Epic Membership.** Rejected. A Ticket could
then name both an Epic and a parent Ticket, which GitHub cannot hold, and its
Epic would no longer follow from its parent.

**Keep a Ticket Container selectable.** Rejected; it is the failure `to-spec`
reports most.

**Nest as deep as GitHub allows.** Rejected: Jira cannot hold it. A
`wayfinder` map that is a Task, holding a task that
`to-tickets` then splits, is Ticket, Ticket, Ticket; model that map as a local
Epic and promote it.

**Let a parked or blocked Container hold back what it holds.** Rejected. That
adds a hidden rule to readiness, when parking the held Tickets says the same
thing in plain sight.

**Make `--children` recursive.** Rejected. The one-level rule stands, and
promoting each Ticket Container with `--children` covers the level below.

**Read Containment back on Pull now.** Deferred to gh-66. Reading a parent
needs the rules gh-66 has not settled: what to do when the parent is not
Adopted, and which pending Mutations shield the edge.

## Consequences

- Migration 021 widens `container_class` to `epic` or `ticket`, renames the
  two Mutation Types in the `mutations` CHECK and on existing rows, and
  renames the payload's `epic_id` to `container_id`. Both CHECK changes need
  the table rebuild ADR-0028 sets out, and ADR-0048 backs the Store up first.
  A queued remove-then-add pair stays two rows, which still converges.
- Adopting the issues of a GitHub tree yields flat Tickets until gh-66. Link
  them with `tk update --parent`; each link queues a push that re-sends the
  parent GitHub already has.
- The Jira Adapter must settle three things before it can represent
  a Ticket Container:
  - A Ticket held by a Ticket must carry a subtask-level type, so moving a
    Ticket between an Epic and a Ticket Container changes its Jira type.
  - Team-managed spaces cannot define a subtask-level Bug type.
  - Atlassian's docs disagree on whether a subtask can move straight to a new
    parent; probe it.

  Until then, Promotion rejects a Containment the Adapter cannot represent,
  as it does any relationship.
- Amends ADR-0015, ADR-0021, ADR-0022, ADR-0025, ADR-0050 and ADR-0054. ADRs
  that only use the term Epic Membership stay as written; GLOSSARY.md lists it
  under Containment's _Avoid_.
