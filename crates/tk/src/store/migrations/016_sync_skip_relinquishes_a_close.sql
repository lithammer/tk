-- ADR-0046 decided Sync Skip's second exception to the done-terminal trigger
-- (ADR-0006): skipping a failed closing Mutation relinquishes the close intent
-- and restores `open`. It is the second of two independent conjuncts,
-- alongside the ADR-0047 Re-Adopt exception migration 015 added. The failed
-- closing Mutation is what authorizes the write; recreating the trigger keeps
-- its name, which is what the `items` object inventory an ADR-0028 rebuild
-- must reproduce is keyed on.
--
-- Unlike the Re-Adopt conjunct, this one is time-shaped, not statement-shaped.
-- `old.origin = 'local' and new.origin = 'backend'` holds only for the single
-- rebind statement Re-Adopt issues. This conjunct instead holds for the
-- entire period a `failed` closing Mutation exists on the Item: the `exists`
-- clause names no particular failed closing Mutation, so any `done` -> `open`
-- write on the Item is admitted for as long as one such row exists — not only
-- the reopen Sync Skip performs in the same transaction as its transition of
-- that Mutation to `skipped`. ADR-0046's "once the transaction commits, the
-- durable authorization is gone" describes only the window *after* that
-- transition; it is not a description of how narrow the exception is
-- beforehand, and this exception must not be read as "consumable" the way the
-- Re-Adopt one is.
--
-- The `json_extract(m.payload_json, '$.status') = 'done'` conjunct is load-
-- bearing, not incidental: migration 011 deliberately spared a `failed`
-- `set_item_status` row belonging to a Promotion Operation even when its
-- target was not `done` (011_split_work_state.sql:132-135). Without this
-- conjunct, such a spared non-closing failure would authorize an unrelated
-- `done` -> `open` write on its Item.
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
  and not (
      exists (
          select 1
            from mutations m
           where m.item_id = new.id
             and m.item_class = new.item_class
             and m.mutation_type = 'set_item_status'
             and m.state = 'failed'
             and json_extract(m.payload_json, '$.status') = 'done'
      )
  )
begin
    select raise(abort, 'cannot leave done state');
end;
