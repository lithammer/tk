# Sync Skip relinquishes a failed close

> **Amended below.** The Store migration repair this Decision describes is not
> implemented: the divergence it would repair has no instances and cannot
> acquire any. And the trigger exception is wider than "narrow, consumable"
> suggests — it admits any `done` -> `open` write on an Item for as long as
> that Item carries the failed closing Mutation.

`tk done` changes local Lifecycle before its closing Mutation reaches the
Backend. If that Mutation fails and the operator skips it, retaining local
`done` leaves the Backend open forever because done Items are outside Backend
Pull. Sync Skip instead relinquishes the close intent and restores the shared
Lifecycle to `open`.

## Decision

Skipping a failed `set_item_status` Mutation targeting `done` changes the Item
to `open` and the Mutation from `failed` to `skipped` in the same Repository
Store transaction. The Item keeps its local content, Priority,
Selection State, and relationships. It stays idle, loses its Closing Reason,
and receives the transaction timestamp as `updated_at`. The rule is identical
for Tickets and Epics.

The `items_no_escape_from_done` trigger remains the terminal-Lifecycle
backstop. It permits this exception only while the same Item has the exact
failed closing Mutation. The Store reopens the Item before transitioning that
Mutation to `skipped`; a failure in either write rolls back both. Once the
transaction commits, the durable authorization is gone and every other
`done`-to-`open` write remains forbidden.

The Item immediately rejoins the ordinary open-only Pull working set. Sync
Skip still commits before opening the Backend Adapter, then continues the same
sync. If the Backend was independently closed, Pull imports `done` again. The
command reports the committed local outcome before Backend work begins:

```text
Skipped Mutation 4; restored gh-53 to open.
```

Other skipped Mutation Types report `Skipped Mutation 4.` through the same
boundary.

This is not general remote-reopen support. A done Item without that explicit
failed-close Skip remains terminal and outside Pull. There is no `tk reopen`
command in v1.

The Store migration repairs existing instances of the same divergence. A
`done` Item with a skipped `set_item_status` Mutation targeting `done` becomes
open and idle, and its Closing Reason is cleared. The migration preserves
`updated_at` because migrations have no `Clock`; the skipped Mutation remains
historical evidence of the abandoned intent.

Detach must ship first. Restoring `open` puts the Item back into exact-key
Pull, where a deleted or inaccessible Backend object can block the whole sync.
Sync Skip never infers deletion from an Adapter error and never removes a
Backend Binding. Detach is the explicit, non-destructive way to retain a Local
Item while removing that Binding.

## Considered Options

**Preserve local `done` after Skip.** Rejected because it makes a known
Lifecycle divergence permanent after the operator has relinquished the close.

**Refuse to skip closing Mutations.** Rejected because one Backend rejection
could block every later Mutation indefinitely.

**Pull every done Backend Item.** Rejected because the bounded working set
would grow into the complete retained history, and one deleted object would
break every later sync. General remote reopen remains deferred.

**Wait for a targeted Backend read before reopening.** Rejected because Skip
must commit without an available Adapter. Waiting would require durable
reconciliation state and a second Pull cohort where the explicit operator
decision already determines the local result.

**Remove the terminal-Lifecycle trigger.** Rejected because typed writers do
not protect future SQL write paths. The failed closing Mutation provides a
narrow, consumable authorization for the one accepted exception.

**Automatically detach on a missing-looking Pull error.** Rejected because the
GitHub Adapter cannot certify deletion separately from invisibility,
permissions, or another item failure. Removing a Backend Binding remains an
explicit operation.

## Consequences

- An accepted, idle Ticket becomes eligible for `tk next` again. Triage and
  parked Tickets keep their Selection State and remain excluded.
- Dependencies the Item resolved as their Blocking Item become unresolved
  again.
- Repairing an existing skipped close may clear an old Closing Reason and may
  expose a missing Backend key on the next sync. Detach is the recovery for the
  latter case.
- Pull remains all-or-nothing and open-only. The stale-refresh guard continues
  to skip an Item closed while a Backend request is in flight, so one such row
  does not abort the batch.

## Amendment: the Store migration repair is dropped

The Decision above says "The Store migration repairs existing instances of the
same divergence." No repair ships. tk is single-user with one Repository
Store: it holds zero `skipped` Mutations, so there is no pre-existing
divergence to repair. And once the schema trigger exception and the Sync Skip
writer ship together, every new skip restores `open` inline, so no
pre-existing divergence can accumulate for a later migration to find either. A
repair for a population that is empty now and cannot grow is dead code.

Consequences' third bullet — "Repairing an existing skipped close may clear an
old Closing Reason and may expose a missing Backend key on the next sync" —
describes only that migration, and is retired with it.

The literal predicate this Decision described — a `done` Item with a
`skipped` `set_item_status` Mutation targeting `done` — was not safe to ship
as written even setting the empty population aside. A `done` Item carrying a
Skipped Mutation of that shape is also produced by:

- `tk detach`. A Skipped Mutation survives Detach: `withdrawal_candidates`
  only selects `pending`/`failed` rows, and Detach never writes `status` or
  `closing_reason`.
- A later close that succeeded, leaving the earlier skip's row as history
  alongside a Mutation that actually closed the Item.
- A Re-Adopt that imported `done` (ADR-0047), which can rebind onto an Item
  still carrying an old skipped close from before it was detached.

Reopening any of those would be wrong: none of them is the divergence this
Decision meant to repair.

## Amendment: the authorization window is wider than the Decision states

"Once the transaction commits, the durable authorization is gone" is true,
but it describes only the window *after* Sync Skip transitions the closing
Mutation to `skipped`. The trigger exception it authorizes is open for the
whole period *before* that commit too: for as long as the Item carries a
`failed` `set_item_status` Mutation targeting `done`, any `done` -> `open`
write on that Item is admitted, not only the one Sync Skip's own transaction
performs.

This is a real widening of the terminal-Lifecycle backstop ADR-0006 keeps for
every other write path. It is accepted because the exception still requires a
specific `failed` closing Mutation to exist on the Item — nothing admits the
write once that Mutation is absent, applied, cancelled, or skipped. Considered
Options' "narrow, consumable authorization" describes the same exception and
is superseded for the same reason: the authorization is real for as long as
the failed Mutation exists, not spent by a single write against it.
