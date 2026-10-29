# Backend Pull reads back Dependencies between bound Items

> **Amended by ADR-0057.** Containment read-back shields a Ticket's whole
> Containment, not a pair, and runs after Dependency additions.

Dependency sync shipped push-only (ADR-0021): Apply sends `add_dependency` and
`remove_dependency`, but Backend Pull never reads an edge back. ADR-0049
refuses that half-sync for a field and left edges outside its rule only
because an edge to an un-Adopted issue has no local Item to point at. That
reason covers some edges, not all of them. Backend Pull now reads Dependencies
back for every edge where both ends exist locally, and GitHub is
authoritative for those edges.

## Decision

### Which edges round-trip

An edge round-trips when its Blocked Item is in this Pull and its Blocking
Item holds a Backend Binding to the same Backend, whatever the Blocking Item's
Lifecycle. A `done` Blocking Item still counts: GitHub keeps a closed blocker
in `blockedBy`. No GitHub document says so; reads of lithammer/tk#77 and
JavaZeroo/termforge#47 on 2026-10-26 listed closed blockers.

Pull ignores every other blocker GitHub lists: a Local Item, an Item awaiting
its Promotion receipt, a Detached Item, an issue nobody Adopted, and an issue
this viewer cannot see. tk does not report them. Adopting the blocker brings
the edge in on the next Pull, even from another repository, since Adopt takes
any issue `gh` resolves.

### What Pull reads

The GitHub Adapter reads `blockedBy` for each refreshed Item in the batched
`issueOrPullRequest` query. It does not read `blocking`. The Mutation already
sits on the Blocked Item, so reading `blockedBy` keeps that ownership and sees
each edge once. An edge whose Blocked Item is `done` is outside Pull and stays
as it is; a `done` Blocked Item affects neither readiness nor Effective
Priority.

The read asks for `first: 100` with `pageInfo { hasNextPage }`. GitHub's
changelog caps each relationship type at 50 per issue. `hasNextPage` is an
Adapter read error, the same refusal the issue-field read makes for a
truncated list, because a truncated list cannot prove an edge is absent.

### How the merge follows the Backend

`merge_backend_refreshes` brings the round-tripping set in line with GitHub in
its one transaction. It removes an edge tk has and GitHub lacks, then adds an
edge GitHub has and tk lacks. Removals go first, so a removal that breaks a
cycle does not block an addition.

A `pending` or `failed` `add_dependency` or `remove_dependency` shields its
own (Blocked Item, Blocking Item) pair, in the shape ADR-0044 gives title and
body. Every other edge on the Blocked Item still follows GitHub. Pull also
updates the edges of an Item it lands as `done` in the same run, since that
data is as fresh as the close. An added edge takes the Pull time as its
`created_at`.

Pull skips an edge that would close a cycle in the local graph and commits
the rest. The check follows the glossary's cycle rule and counts each Ticket a
Ticket Container holds as one of its Blocking Items; the
`dependencies_no_cycle` trigger walks Dependencies alone. GitHub cannot see
Local Items or Containment, so it can hold an edge that is legal there and a
cycle here.

Sync and Promotion print one stderr warning per skipped edge on every run that
returns a report, a run stopped at a rejected Mutation included, until someone
removes one side. A run whose Apply errors prints only that error. The warning
names both Display IDs and any Items on the path between them. Here gh-9 holds
a Local Ticket that waits on gh-12:

```
tk sync: warning: did not import Dependency gh-12 blocked by gh-9: gh-9 already waits on gh-12 through repo-40
```

Adopt reads no edges; the next Pull brings them in. The `N pulled, M applied`
summary does not count edges.

## Considered Options

**Ignore absence and only add edges.** Rejected: a blocker removed on GitHub
would keep holding the local Item back forever, and a skipped
`add_dependency` would leave tk and GitHub disagreeing with nothing to say
which is right. ADR-0010 treats a missing *item* as no signal because Pull
cannot tell deletion from filtering. A missing edge on an item Pull did read,
from an untruncated list, is a complete answer.

**Read `blocking` too.** Rejected: it sees each edge twice, and the extra
edges it finds have a `done` Blocked Item that nothing downstream reads.

**Fail the Pull on a cycle.** Rejected: Pull runs before Apply, so one
Backend edge would stop every Mutation from applying. ADR-0049 rule 3 keeps an
ambiguous label set from failing Pull for the same reason.

**Report un-Adopted blockers.** Rejected: under ADR-0034 an un-Adopted issue
is not tk's to track. A warning would fire every sync for work the user chose
not to take on.

**Shield every edge on an Item with any pending Dependency Mutation.**
Rejected: one queued `tk block` would freeze unrelated edges on the same Item
until Apply.

**Paginate `blockedBy`.** Rejected for now: GitHub's changelog caps the list
at 50 and the read asks for 100, so paging would be code no real issue reaches.

## Consequences

- An `add_dependency` that fails and is skipped is undone by the next Pull,
  just as ADR-0046 restores `open` after a skipped close. There is no backfill
  of edges that ADR-0021's placeholder Apply no-op-Acknowledged.
- Containment stays push-only. It is 0..1 and admits a Local Container, so
  this rule does not settle it. gh-151 owns its read-back and inherits the
  pair-style shield and the skip-and-warn rule.
- Amends ADR-0021: Dependency sync is no longer push-only.
- Amends ADR-0049: Dependencies between bound Items now pass its test.
  Containment and Item Class still sit outside it.
- Amends ADR-0055: gh-151, not gh-66, owns Containment read-back.
