# Backend Pull reconciles Dependencies between bound Items

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
in `blockedBy`. On the live Store the one edge Pull can see, gh-77 blocked by
gh-68, is of this kind.

Pull ignores every other blocker GitHub lists: a Local Item, an Item awaiting
its Promotion receipt, a Detached Item, an issue nobody Adopted, an issue in
another repository, and an issue this viewer cannot see. tk does not report
them. Adopting the blocker brings the edge in on the next Pull.

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

### How the merge reconciles

`merge_backend_refreshes` reconciles the round-tripping set in its one
transaction. It removes an edge tk has and GitHub lacks, then adds an edge
GitHub has and tk lacks. Removals go first, so a removal that breaks a cycle
does not block an addition.

A `pending` or `failed` `add_dependency` or `remove_dependency` shields its
own (Blocked Item, Blocking Item) pair, in the shape ADR-0044 gives title and
body. Every other edge on the Blocked Item still reconciles. Pull reconciles
the edges of an Item it lands as `done` in the same run, since that data is as
fresh as the close. An added edge takes the Pull time as its `created_at`.

An edge that would close a cycle in the local graph is skipped, and the rest
of the Pull commits. The check counts each Ticket a Ticket Container holds as
one of its Blocking Items, as the glossary's cycle rule does, which the
`dependencies_no_cycle` trigger does not. GitHub cannot see Local Items or
Containment, so it can hold an edge that is legal there and a cycle here. Sync
prints one stderr warning per skipped edge, every run until someone removes
one side, naming both Display IDs and the local path:

```
warning: skipped GitHub dependency gh-12 blocked by gh-9: gh-9 already waits on gh-12, which it holds
```

Adopt stays field-only, and the `N pulled, M applied` summary is unchanged.

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
  of edges that ADR-0021's placeholder Apply no-op-Acknowledged. The live
  Store has none outside GitHub.
- Containment stays push-only. It is 0..1 and admits a Local Container, so
  this rule does not settle it. gh-151 owns its read-back and inherits the
  pair-style shield and the skip-and-warn rule.
- Amends ADR-0021: Dependency sync is no longer push-only.
- Amends ADR-0049: Dependencies between bound Items now pass its test.
  Containment and Item Class still sit outside it.
- Amends ADR-0055: gh-151, not gh-66, owns Containment read-back.
