-- Re-Adopt imports the Backend snapshot's Lifecycle (ADR-0047), and an
-- imported `open` has to clear a Closing Reason the `closing_reason` CHECK
-- confines to `done`. That is a `done` -> `open` write, which the
-- done-terminal trigger (ADR-0006) otherwise aborts, so the trigger gains its
-- first exception. ADR-0046 decided a second one for Sync Skip's relinquished
-- close; it is not in this schema, and the two are independent conjuncts
-- whenever it arrives.
--
-- The exception recognises the exact Re-Adopt rebind and nothing else: one
-- `items` UPDATE moving a Local Item onto a canonical Backend identity its own
-- Former Backend Identity history already reserves to it. Every other
-- `done` -> `open` write stays forbidden, including a later status change on
-- the restored Backend Item, because `old.origin = 'local'` holds only for the
-- rebind statement itself. Promotion is unaffected: `promotion::apply_receipt`
-- keeps `status` out of its SET list, so this trigger never fires there.
--
-- Recreating the trigger keeps its name, which is what the `items` object
-- inventory an ADR-0028 rebuild must reproduce is keyed on.
drop trigger items_no_escape_from_done;

create trigger items_no_escape_from_done before update of status on items
for each row when old.status = 'done' and new.status != 'done'
  and not (
      old.origin = 'local'
      and new.origin = 'backend'
      and exists (
          select 1
            from former_backend_identities f
           where f.item_id = new.id
             and f.backend_kind = new.backend_kind
             and f.backend_key = new.backend_key
      )
  )
begin
    select raise(abort, 'cannot leave done state');
end;
