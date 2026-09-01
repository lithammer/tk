# Sync Skip relinquishes a failed close

`tk done` changes local Lifecycle before its closing Mutation reaches the
Backend. If that Mutation fails and the operator skips it, retaining local
`done` leaves the Backend open forever because done Items are outside Backend
Pull. Sync Skip instead relinquishes the close intent and restores the shared
Lifecycle to `open`.

## Decision

Skipping a failed `SetItemStatus(done)` Mutation changes the Item from `done`
to `open` in the same Repository Store transaction that changes the Mutation
from `failed` to `skipped`. The Item keeps its local content, Priority,
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
`done` Item with a skipped `SetItemStatus(done)` Mutation becomes open and
idle, and its Closing Reason is cleared. The migration preserves `updated_at`
because migrations have no `Clock`; the skipped Mutation remains historical
evidence of the abandoned intent.

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
