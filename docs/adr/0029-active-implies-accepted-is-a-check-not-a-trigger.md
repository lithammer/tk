# Enforce `active ⟹ accepted` with a row-shape CHECK, not a trigger

A Ticket that is `active` must have Selection State `accepted` — you cannot
actively work `triage` or `parked` work (tk-72, tk-76). We enforce this in the
Repository Store schema as a conjunct on the existing combined Ticket invariant
CHECK (`status <> 'active' or selection_state = 'accepted'`), added via a
foreign-keys-off table rebuild (ADR-0028), rather than as a trigger. The `tk
start` and `tk park` transition helpers still own the user-facing diagnostics;
the CHECK is the defence-in-depth backstop. This is a *row-shape* invariant —
it constrains only the new row's columns — so a declarative CHECK is the right
tool: it covers INSERT as well as UPDATE and keeps the entire "what is a valid
Ticket row" invariant in one place, alongside the priority × Selection State
clause from ADR-0027/ADR-0028.

## Consequences

- Backend Pull is a fourth writer of Item Status, alongside the `tk start` /
  `tk park` transition guards and the CHECK backstop. Its merge clamps an
  incoming `active` on a non-`accepted` Ticket down to `open` (tk-77), the same
  heal this migration applies on rebuild, so a backend signal cannot flip
  locally held (`parked`) work into progress. The clamp covers the `active`
  case only; a Pull onto a locally-`done` row is governed by the
  `items_no_escape_from_done` trigger and is a separate sync-conflict concern.

## Considered Options

- **A `BEFORE UPDATE` trigger**, mirroring the `items_no_escape_from_done`
  done-terminal guard. Rejected: that precedent does not transfer. Done-terminal
  is a *transition* rule (`when old.status = 'done' and new.status != 'done'`) —
  it references `old`, which a CHECK cannot see, so a trigger is mandatory
  there. `active ⟹ accepted` references only the new row, so a trigger would be
  an imperative band-aid for a constraint the engine can enforce declaratively,
  and a `BEFORE UPDATE` trigger would miss a malformed INSERT.
