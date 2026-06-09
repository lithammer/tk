# Schema CHECK changes use a foreign-keys-off table rebuild

tk-74 introduces triage Tickets, which carry no Priority. The migration-001
`items` table CHECK hard-codes `ticket ⟹ priority is not null`, so admitting
a null-Priority Ticket requires changing that table-level CHECK. SQLite
cannot alter a CHECK in place: the only way is the 12-step table rebuild —
create `items_new` with the new constraints, copy rows, drop `items`, rename
`items_new` to `items`, and recreate every index, trigger, and generated
column.

The rebuild collides with the migration runner. `DROP TABLE items` fires an
implicit `DELETE` of every row, which violates the `on delete restrict`
foreign keys that `dependencies`, `external_blockers`, `mutations`, and
`item_ids` hold against `items` — so the drop fails unless
`PRAGMA foreign_keys = OFF`. That pragma is a no-op inside a transaction and
must be set before `BEGIN`, but `apply_one` runs every migration inside
`BEGIN IMMEDIATE` with foreign keys enabled.

## Decision

- **Migration 005 rebuilds `items`** with the complete final
  Priority × Selection State invariant for all three Selection States, so no
  further rebuild is needed when parking lands (tk-75):

  ```text
  ticket ⟹ ticket_kind is not null
         ∧ ( (selection_state = 'triage'                  ∧ priority is null)
           ∨ (selection_state in ('accepted','parked')    ∧ priority is not null) )
  epic   ⟹ ticket_kind is null ∧ priority is null ∧ selection_state is null
  ```

  This also promotes tk-73's writer-guaranteed `ticket ⟹ non-null
  selection_state` (ADR-0027) to a hard schema guarantee.

- **The runner gains an explicit per-migration foreign-keys mode.** A
  `Migration` declares whether it needs foreign keys disabled; the default is
  enabled and every existing migration keeps it. For a disabled migration,
  `apply_one` sets `PRAGMA foreign_keys = OFF` before `BEGIN IMMEDIATE`, runs
  the rebuild, asserts `PRAGMA foreign_key_check` returns no rows *inside* the
  transaction so a corrupt copy rolls back atomically, commits, then restores
  `PRAGMA foreign_keys = ON`. Foreign keys are restored on every exit path,
  including error, so a failed rebuild cannot leave the connection with
  enforcement off.

## Considered Options

- **Run every migration foreign-keys-off with a trailing
  `foreign_key_check`.** Rejected: it silently drops per-statement foreign-key
  enforcement for all future migrations. The rebuild is the rare exception, so
  it opts out explicitly rather than weakening the default.
- **Avoid the rebuild — enforce Priority nullability with a trigger and leave
  the CHECK.** Rejected: the offending CHECK is baked into the migration-001
  table definition and cannot be dropped without a rebuild, so this does not
  remove the constraint that rejects triage.
- **Split parked into a second rebuild in tk-75.** Rejected: two rebuilds for
  one logical invariant. The full three-state invariant is cheaper to bake in
  once, and tk-75 then changes behaviour only, not schema.

## Consequences

- Migration 005 is the first foreign-keys-off migration; the runner's
  restore-on-all-paths contract is load-bearing and is covered by a test that
  a failed rebuild leaves `PRAGMA foreign_keys` re-enabled.
- `foreign_key_check` runs inside the transaction, so a bad data copy rolls
  back with the rest of the migration rather than committing a store whose
  child rows dangle.
- Auto-migrate-on-open (ADR-0024) heals an older store through this same
  path. Foreign-keys-off is connection-local, and `BEGIN IMMEDIATE` still
  serializes writers, so a concurrent reader is unaffected.
- The tk-73 tripwire test (a NULL `selection_state` on a Ticket is *not*
  rejected by the ADD COLUMN CHECK) flips after 005: the rebuild's CHECK
  rejects it, and the test is rewritten to assert rejection.
- The rebuild must recreate the `items_no_escape_from_done` trigger
  (ADR-0006 / migration 002), the `display_source` generated column, the
  `items_next_idx` / `items_backend_unique` / `items_container_idx` /
  `items_id_class_unique` indexes, and the composite foreign key to
  `item_ids`, or those contracts silently vanish.
