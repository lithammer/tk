# Backend Pull reads back Containment between bound Items

Containment sync shipped push-only (ADR-0055): Apply sends
`set_ticket_container` and `clear_ticket_container`, but Backend Pull never
reads a parent back, so adopting the issues of a GitHub tree yields flat
Tickets. ADR-0056 brought Dependencies back on Pull for every edge whose two
ends are bound, and left Containment out because its rule does not settle
two things a Dependency lacks: a Ticket has at most one Container, and that
Container may be Local. Backend Pull now reads Containment back on the same
terms, and this ADR settles those two.

## Decision

### Which Containment round-trips

Containment round-trips when its Ticket is in this Pull and its Container
holds a Backend Binding to the same Backend, whatever the Container's
Lifecycle. GitHub is authoritative for it. Pull ignores every other parent
GitHub lists, as ADR-0056 ignores every other blocker: an issue nobody
Adopted, a Local Item, an Item awaiting its Promotion receipt, a Detached
Item, and an issue this viewer cannot see.

A Local Container is outside the round-trip, since GitHub cannot hold it.
ADR-0035 already keeps mixed-Origin Containment local, and `tk update
--parent` pushes a move to a Local Container as `clear_ticket_container`.
So for a refreshed Ticket:

| GitHub's parent | tk's Container | Pull |
| --- | --- | --- |
| none, or not bound | none | nothing |
| none, or not bound | Local | keeps it |
| none, or not bound | bound | clears it |
| bound | none, or another bound | sets GitHub's |
| bound | Local | replaces it with GitHub's |

A bound parent replaces a Local Container without a warning. A Ticket has
one Container, GitHub has just named one tk can hold, and after the Pull tk
and GitHub agree. Keeping the Local Container would leave them disagreeing
with nothing to say which is right, the drift ADR-0056 refuses.

An Epic in the Pull reads no Containment, since an Epic has no Container.
Pull ignores its GitHub parent without a warning: an Epic under an
initiative issue is ordinary GitHub use that tk cannot model, and a warning
would fire on every sync with nothing the user could fix in tk.

### What Pull reads

The GitHub Adapter reads `parent { url }` for each refreshed Item in the
batched `issueOrPullRequest` query. GitHub's schema types `Issue.parent` as a
nullable `Issue`, and a read of lithammer/tk-gh-playground on 2026-10-29
returned canonical issue URLs. A null parent means GitHub lists none; a
response without the `parent` key is an Adapter read error, since reading it
as none would clear every bound Container in the Pull.

### How the merge follows the Backend

`merge_backend_refreshes` brings Containment in line with GitHub in the same
transaction that follows Dependencies, in three steps:

1. For each refreshed Item, remove the Dependencies GitHub lacks, and clear
   a bound Container GitHub no longer lists or any Container a bound parent
   replaces.
2. Add the Dependencies GitHub lists.
3. Set each Container GitHub lists.

Clears land before any set, so a Ticket whose held Tickets all moved away in
the same Pull can be held by a Ticket, and a Ticket Container the Ticket is
leaving cannot make a Dependency in step 2 look like a cycle.

Dependencies land before Containment, so when both arrive in one Pull and
together would close a cycle, Pull drops the Containment: half-represented
Containment costs only grouping (ADR-0035), while a missing Dependency would
let an Item start before its blocker. A Containment tk already holds stays,
so a Dependency that arrives later and closes a cycle with it is the one
skipped.

A `pending` or `failed` `set_ticket_container` or `clear_ticket_container`
shields the Ticket's Containment whole, as an unresolved
`set_ticket_priority` shields Priority (ADR-0054). A Ticket has one
Container, so a pair shield like ADR-0056's would not stop Pull from moving
the Ticket elsewhere while its own Mutation waits, and the next Apply would
move it back.

### Skip and warn

Pull skips a Container tk would refuse and commits the rest: one the depth
cap refuses, and a Ticket Container the Ticket already waits on (ADR-0055).
`tk update --parent` refuses both through the same check, and `tk block`
walks the same wait path, held Tickets included; each refusal names the
wait path.

The skipped Ticket ends as if GitHub listed no parent: it loses a bound
Container, and Pull puts back a Local one it cleared in step 1. The Local
Container goes back through the same check, so if it would now close a
cycle with a Dependency this Pull added, the Dependency wins and the Ticket
keeps no Container.

Sync and Promotion print one stderr warning per skipped Containment on
every run that returns a report, as for a skipped Dependency, until someone
changes one side. The warning gives the depth cap's own reason, or names
the Items on the wait path:

```
tk sync: warning: did not import Containment gh-2 held by gh-1: gh-2 holds Tickets, so only an Epic can hold it, not Ticket gh-1
tk sync: warning: did not import Containment gh-12 held by gh-9: gh-12 already waits on gh-9 through gh-4
```

GitHub allows eight levels of sub-issues. When its tree is deeper than tk's
cap, Pull sets Containers in the order it refreshed their Tickets, so which
level tk keeps is repeatable but follows no rule of the tree.

Adopt reads no parent; the next Pull brings it in. The `N pulled, M applied`
summary does not count Containment.

## Considered Options

**Keep a Local Container when GitHub names a bound parent.** Rejected: tk
and GitHub would disagree on the Ticket's one Container on every sync, with
no Mutation pending to settle it.

**Warn when a bound parent replaces a Local Container.** Rejected: the
result agrees with GitHub and needs no action, and the warning would fire
once with nothing to fix.

**Warn about an Epic's GitHub parent.** Rejected: the same reason ADR-0056
gives for not reporting un-Adopted blockers. The user cannot change it in tk,
so the warning would fire on every sync.

**Clear a Local Container when GitHub lists no parent.** Rejected: GitHub
cannot hold a Local Container, so its absence there says nothing about it.

**Shield only the Container a Mutation names.** Rejected: Pull could move
the Ticket elsewhere while its own Mutation waits.

**Set Containment before Dependencies.** Rejected: on a conflict Pull would
drop the Dependency, which holds work back, rather than the Containment,
which only groups it.

## Consequences

- A `set_ticket_container` that fails and is skipped is undone by the next
  Pull, as ADR-0056 does for a skipped `add_dependency`.
- `tk update --parent` and `tk block` now refuse a cycle through a held
  Ticket, as ADR-0055 required; both had checked Dependencies alone.
- `tk update --parent` and Pull check and write a Container through the
  same Store helpers, so both refuse the same Containment and both take
  `container_class` from the Container's row.
- Amends ADR-0021, ADR-0049 and ADR-0055: Containment is no longer
  push-only.
- Amends ADR-0056, which said gh-151 would inherit its pair shield; a
  Containment shield covers the whole Ticket.
