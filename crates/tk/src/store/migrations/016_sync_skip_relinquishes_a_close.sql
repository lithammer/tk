-- ADR-0046 decided Sync Skip's second exception to the done-terminal trigger
-- (ADR-0006): skipping a failed closing Mutation relinquishes the close intent
-- and restores `open`. It is the second of two independent conjuncts,
-- alongside the ADR-0047 Re-Adopt exception migration 015 added. The failed
-- closing Mutation is what authorizes the write; recreating the trigger keeps
-- its name, which is what the `items` object inventory an ADR-0028 rebuild
-- must reproduce is keyed on.
--
-- This conjunct is not consumable the way the Re-Adopt one is: it recognises
-- a state rather than a statement, and stays open for as long as the Item
-- carries a `failed` closing Mutation. ADR-0046's third amendment and
-- ADR-0006's header record what that widens; do not restate the argument
-- here.
--
-- The `json_extract(m.payload_json, '$.status') = 'done'` conjunct is load-
-- bearing, not incidental: migration 011 deliberately spared a `failed`
-- `set_item_status` row belonging to a Promotion Operation even when its
-- target was not `done` (011_split_work_state.sql). Without this
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
