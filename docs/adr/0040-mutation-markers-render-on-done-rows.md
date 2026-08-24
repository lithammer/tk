# Mutation markers render on `done` rows

tk-47 adds two list-row markers — `~` for a pending Mutation, `⚑` for a
failed one — so `tk list` and `tk search` can say a row carries work the
Backend has not received. No `tk list` view predicate matches a `done`
Ticket, but its epic-parent-inclusion branch can surface a `done` Epic as a
container, and `tk search` matches every Item Status including `done`.
Either path can
feed a `done` row to the shared renderer carrying a pending or failed
Mutation, so the marker needs an explicit answer for that row the way the
blocked treatment (`⊘`) already has one.

## Considered Options

- **Suppress the markers on a `done` row, consistent with `⊘`.** A marker on
  a closed Item reads as noise the same way a blocked dim would. **Rejected**:
  a `done` Backend Item with a queued `set_item_status` is exactly the case
  where the Backend does not yet know the Item is done. Suppressing the
  marker there hides the only thing that row has left to say — closing an
  Item locally does not make the Backend agree until the Mutation applies.
- **Render the markers on a `done` row.** **Chosen.** The marker's meaning —
  "the Backend has not received this Item's work" — holds regardless of the
  Item's own lifecycle state; a `done` Item is not exempt from carrying
  unsent work, it is often the row where the unsent work matters most.

## Consequences

- `render_row` gates the blocked treatment on `row.status != ItemStatus::Done`
  but applies no such gate to the Mutation markers; a `done` row can show
  `⚑` and/or `~` beside its title exactly like any other row.
- This does not reverse ADR-0025. That ADR's decision — a `done` row never
  renders the blocked treatment — is scoped to `⊘` and stands unchanged; its
  own `## Amendment (tk-163)` section already corrects the premise that
  `tk list` never renders `done` in the first place. This ADR answers a
  different question (Mutation markers, not the blocked treatment) for the
  same class of row.
- The first case to exercise it is a `done` Ticket with a queued
  `set_item_status` Mutation, reached through `tk search`: it must still
  show `~`.
