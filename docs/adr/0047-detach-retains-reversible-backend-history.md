# Detach retains reversible Backend history

Adopt and Promotion deliberately move an Item into tk's opt-in Backend working
set, but a Backend Binding previously had no reverse. A deleted or inaccessible
Backend object could therefore stop all-or-nothing Pull indefinitely, and a
user could not resume local-only work without deleting or rewriting the Item.

## Decision

### Detach removes the Binding, not the Item

`tk detach <id>` turns a concrete Backend Ticket or Backend Epic into the same
Item with Local Origin. It accepts any Lifecycle and Work State. Detach rejects
a Local Item. For a Pending Promotion, it points to Promotion Cancellation
because no Backend identity exists to detach.

Detach opens no Backend Adapter and leaves the Backend object unchanged. It
holds the Remote workflow lock and commits the Origin, identity, Display ID,
history, and Mutation changes in one Repository Store transaction. It works
without a configured Remote. An unrelated `applying` Promotion does not block
this Store-only operation, but Detach refuses when any affected Mutation
belongs to an unresolved Promotion Operation. The operator must first
reconcile, retry, or cancel that operation rather than partially withdrawing
it.

The Item keeps its stable internal ID, Item Class, fields, Lifecycle, Work
State, Closing Reason, Selection State, and relationships. Only its Origin,
Backend Binding, user-facing identity, and `updated_at` change.

Detach withdraws every pending or failed Mutation that needs the removed
identity. This includes Mutations targeting the Item and relationship Mutations
that address it as a counterpart. Each becomes `cancelled`; an existing
Mutation Failure remains as evidence. Terminal Mutation history is unchanged.
The command names every withdrawal instead of hiding lost intent in a count.

### Relationships survive unless they would violate the resulting graph

Dependencies, Epic Membership, and External Blockers remain local current
state. Detach refuses when the Item is a Blocking Item whose Blocked Item would
remain Backend-bound. The user must remove that Dependency first. A detached
Blocked Item may retain its Dependencies, and mixed-Origin Epic Membership
stays local. These are ADR-0035's existing resulting-graph rules applied in the
reverse direction.

### Former Backend Identity is history, not a dormant Binding

Detach removes the Backend Display ID from the resolver and retains the
canonical Backend identity as a **Former Backend Identity**. The former
identity does not participate in Pull, Mutation creation, the retained Backend
cohort, or Remote-clear checks. It therefore imposes no constraint on the
configured Remote.

Former identities remain globally reserved to their stable Item for that
Item's lifetime. An Item may retain several across Detach and Promotion cycles,
but has at most one active Binding. `tk show` lists the identities most recently
detached first, once per canonical identity. When Re-Adopt restores an identity,
it remains in history but is omitted while it is the current Binding; another
Detach moves it to the top again. List, next, and search views omit former
identities. Forget may eventually remove them with the Item, but no separate
pruning operation ships here.

Every Binding records the local Display ID it displaced. Detach restores that
ID, including after repeated Detach and Re-Adopt cycles. An Item created by
ordinary Adopt has no prior local Display ID, so its first Detach allocates one
from the normal Repository Store sequence. The Backend Display ID is not kept
as an Alias.

The migration backfills existing Backend Items only from unambiguous evidence:
one Alias is the displaced local Display ID, while no Alias means ordinary
Adopt. It preserves a schema-valid Item with several Aliases but marks its
provenance unresolved; Detach refuses that Item rather than guessing.

### Exact Re-Adopt restores the same Item

`tk adopt <backend-key>` still creates only Backend Tickets for unknown Backend
objects. After the Adapter canonicalizes and verifies the key, an exact Former
Backend Identity instead restores its original Item, including a
Backend Epic. It requires a configured Remote of the matching Backend kind and
refuses while that Item is already bound elsewhere. It never creates a second
Item for a canonical identity tk already knows.

Re-Adopt makes the Backend Display ID current and demotes the local Display ID
to an Alias. The Backend snapshot replaces title, body, Lifecycle, and Ticket
Kind for a Ticket. Item Class, Local Fields, and relationships remain local. A
`done` Lifecycle clears Work State; `open` preserves Work State and clears an
incompatible Closing Reason.

Re-Adopt preflights the resulting relationship graph as Promotion does. It
appends fresh Mutations for each relationship that becomes representable
Backend intent, rejects invalid Dependencies, and leaves mixed-Origin Epic
Membership local. Withdrawn Mutations remain terminal. The rebind, imported
fields, Display ID changes, and new Mutations commit atomically. Re-Adopt does
not run sync; it reports each queued relationship Mutation so the user can
apply the ordered outbox with `tk sync`.

Detach reports the direction and the Backend object it did not change:

```text
Detached: Backend Ticket gh-53 → Local Ticket tk-17
Backend object left unchanged: https://github.com/o/r/issues/53
Withdrew update_ticket for tk-17 (Mutation 4)
```

Re-Adopt likewise reports its Display ID mapping, imported shared fields, and
each queued relationship Mutation.

## Considered Options

**Forget the local representation.** Rejected for this slice. Forget is
destructive graph and history deletion, not the inverse state transition
needed to recover local work. It requires a separate decision.

**Keep the Backend Display ID as an Alias.** Rejected because short Backend
Display IDs are not canonical across repositories. A detached `gh-53` Alias
could also collide with a later active Backend Item while falsely appearing to
identify the old object.

**Mint a new local Display ID for every Detach.** Rejected because Promotion
and Re-Adopt already retain an exact local identity. Replacing it on every
cycle would make one stable Item look new.

**Restore withdrawn Mutations during Re-Adopt.** Rejected because Detach made
their cancellation terminal. Replaying them would contradict the fresh
Backend snapshot and resurrect intent the user explicitly withdrew.

**Run sync from Re-Adopt.** Rejected because ordinary Adopt does not drain the
outbox, and doing so could apply unrelated queued work. Atomic append plus an
explicit report preserves normal Mutation ordering without hidden effects.

**Let Former Backend Identity constrain Remote configuration.** Rejected
because history would then behave as a dormant Binding and make Detach
incomplete.

## Consequences

- A missing Backend object can leave the Pull working set without deleting the
  local Item or inferring deletion from an Adapter failure.
- Detach is reversible when the same canonical Backend object remains
  accessible, while promotion to a different Backend object remains possible.
- Canonical identities need one uniqueness invariant spanning active and former
  ownership, plus explicit displaced-Display-ID provenance.
- Detach extends `cancelled` beyond Promotion Cancellation and migrations while
  retaining Mutation Failure evidence.
- Re-Adopt extends Adopt's Store path to existing Tickets and Epics but does not
  add general Backend Epic intake.
- Forget remains a separately tracked destructive operation.
