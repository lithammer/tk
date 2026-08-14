# Promotion cancellation withdraws an operation without a Backend call

A Promotion the Backend will never accept has no exit. `tk sync --skip` refuses
a Promotion, `tk remote clear` refuses while one is pending and recommends the
`--skip` that just refused, and `tk promote` recommends a `tk sync` that can
never succeed. ADR-0035 deferred cancellation semantics because later title,
body, status, membership, and Dependency Mutations may all rely on the missing
backend identity. ADR-0037 gave an *indeterminate* creation reconcile and retry;
this decision gives a *certified* one its exit.

## Decision

`tk promote cancel <id>` withdraws the Promotion Operation that the named
Item's Promotion belongs to. The unit is the invocation, not the row: a
regretted `tk promote <epic> --children` is undone whole rather than leaving
twelve children to be created as bare Backend Tickets after their Epic is gone.

A `pending` or `failed` Promotion may be cancelled. Both are certified
no-effect — the engine durably writes `applying` before the creation call, and
certified `Rejected` is the only path to `failed`. An `applying` Promotion
anywhere in the operation refuses the whole cancellation and names reconcile and
retry, because a partial withdrawal would leave an operation whose Epic may
exist upstream and whose children are gone: exactly the half-state operation
scope exists to prevent. An already `applied` Promotion does not block
cancellation of the operation's remaining Promotions; it is reported, since tk
never compensates by deleting Backend objects.

### The withdrawn set

Cancellation withdraws every nonterminal Mutation that cannot resolve without a
cancelled Item's Backend address: the Promotions themselves, any Mutation
targeting a cancelled Item, `add_dependency` or `remove_dependency` naming one
as Blocking Item, and `add_ticket_to_epic` naming a cancelled Epic.
`remove_ticket_from_epic` is excluded — clearing a 0..1 slot resolves without a
counterpart address, so it still applies.

The set is one hop and never transitive. Only the cancelled Items lose their
prospective identity, so no third Item's Mutations become unresolvable; a later
Promotion Operation for another Item survives with only its references to a
cancelled Item withdrawn. Ordering makes the set entirely later and entirely
unapplied: a Mutation for a Local Item exists only once that Item's Promotion is
in the Mutation Log, and global Mutation Sequence order holds everything behind
a nonterminal Promotion.

An exhaustive match over Mutation Type derives which counterpart roles each
Mutation addresses, so a Mutation Type added later cannot reach the Mutation Log
without a decision about whether it joins a withdrawn set.

### A withdrawn Dependency refuses rather than degrades

Cancelling a Blocking Item while its Blocked Item stays backend-bound would
leave a backend-bound Blocked Item waiting on a Local Blocking Item — the state
`tk block` and Promotion preflight both refuse to create. Cancellation refuses
it too, naming each offending edge and `tk unblock` as the remedy. The rule is
not restated: cancellation is the third caller of ADR-0035's Dependency
classification, judging every affected edge against the Backend Binding the
withdrawal will produce, exactly as preflight judges against the Binding a
Promotion will produce. When both endpoints are cancelled together the edge
simply stays local, so an operation-wide withdrawal is self-consistent.

Epic Membership degrades instead, as ADR-0035 already decided: losing grouping
is not the same failure as a backend-backed Item exposing an incomplete blocking
constraint.

### Cancelled is a distinct terminal Mutation state

A Skipped Mutation is one a human bypassed after sync failed on it. A withdrawn
Mutation was never attempted, and most of a withdrawn set is collateral the
operator never inspected. `cancelled` keeps the two provenances separable in the
Sync Log rather than leaving `failure_json` presence as the only clue, and it
moves the invariant that Sync Skip never touches a Promotion into the schema: a
Promotion row can reach `cancelled`, never `skipped`.

`cancelled` joins the Sync Log's default filter, so a withdrawal is visible
without a flag, and `tk sync log --cancelled` lists them alone. Pending
Promotion already resolves from the nonterminal states alone, so a cancelled
Promotion returns its Item to Local Backend Binding with no further rule. The
report of whether a Promotion Operation resolved asks the nonterminal set
instead of "not applied", so human-curated terminal omission counts as
resolved — which corrects the pre-existing reading of a Skipped Mutation too.

### Cancellation opens no Adapter

Cancellation performs no Backend access: no Adapter, no nested sync, and none of
ADR-0037's requirement that recovery act only on the earliest nonterminal
Mutation, which exists to keep *Backend* effects ordered. It holds the
repository-scoped Remote workflow lock, because it rewrites Mutation Log state a
concurrent sync could be draining, and it commits in one transaction.

Its report names every withdrawn Promotion, every already-applied Promotion it
cannot undo, and every withdrawn Mutation whose target is *not* itself a
cancelled Item — that last group is intent lost for an object that really exists
upstream, such as a Backend Ticket's membership change in a cancelled Epic. The
remainder is a count. Enumerate what surprises; count what does not.

## Considered Options

**Teach `tk sync --skip` to accept a Promotion.** Rejected. Skip is
sequence-addressed, `failed`-only, and defined as bypassing one Mutation during
sync. A cancellation is item-addressed, spans many rows, cancels from `pending`
as readily as `failed`, and touches no Backend.

**Cancel only the named Item's Promotion.** Rejected. Cancelling a `--children`
Epic would still create every child upstream as a bare Ticket, which is not what
anyone abandoning that invocation asked for.

**Cancel the resolvable Promotions of an operation that also holds an
`applying` one.** Rejected. It trades a refusal that points at a remedy for a
partially real operation nothing can describe.

**Reuse `skipped` for withdrawn Mutations.** Rejected. It costs no migration,
but it makes Sync Skip's own definition untrue, leaves cancellation and skip
distinguishable only by whether a failure record happens to be attached, and
demotes "Sync Skip never touches a Promotion" from a schema fact back to a
runtime guard.

**Skip a withdrawn Dependency Mutation and leave the local edge.** Rejected.
ADR-0035 treats a half-represented Dependency as the one thing worth rejecting a
whole Promotion over; a withdrawal reaches the same state from the other
direction and deserves the same refusal.

**Delete the offending Dependency edges as part of cancelling.** Rejected.
Cancellation withdraws backend intent; silently editing the Repository Store's
Dependency graph is a different and unrequested change.

**Require `--force` when the withdrawn set is non-empty.** Rejected. A
re-promotion re-snapshots title, body, Item Status, and same-Backend Epic
Membership, so cancellation is reversible, and friction on the exit of last
resort only makes a stuck queue stickier. The report is the dry run.

**A shared declaration of addressed counterpart roles, consulted by both
delivery resolution and the withdrawn-set query.** Rejected for now. An
exhaustive match already forces a decision when a Mutation Type is added, and a
test pinning `add_ticket_to_epic` in and `remove_ticket_from_epic` out costs far
less than refactoring the delivery seam that cancellation deliberately avoids.

## Consequences

- Every Promotion state now has an exit: certified failure and untried intent
  cancel, indeterminate creation reconciles or retries, and nothing requires
  deleting a Backend object.
- `tk remote clear` becomes reachable after a permanently rejected Promotion,
  and its guidance recommends cancellation rather than the `--skip` that
  refuses.
- A cancelled Item keeps its current state, its Display ID, and its Aliases. It
  never held a Backend identity, so there is nothing to replace or retain.
- Re-promoting a cancelled Item converges: the new operation snapshots current
  title, body, Item Status, and the membership of the same-Backend Tickets the
  Epic already contains. The withdrawn operation stays in the Mutation Log as
  cancelled rows.
- The `mutations.state` CHECK gains `cancelled` through an ADR-0028
  foreign-keys-off table rebuild.
- Cancellation renders no Promotion mappings and classifies no sync error, so it
  does not trigger the recovery-rendering consolidation tracked separately.
- `prime.md` names cancellation among the human curation commands under the
  existing rule: an agent surfaces the exit rather than running it, because
  withdrawing intent is a human decision in the same way reconcile and retry
  are.
