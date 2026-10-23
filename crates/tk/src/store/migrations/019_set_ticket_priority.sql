-- Add the `set_ticket_priority` Mutation Type (ADR-0054) and queue the
-- Priority backfill that must precede Pull's first read of it.
--
-- The `mutation_type` CHECK cannot be altered in place, so this is a
-- foreign-keys-off `mutations` rebuild in the shape ADR-0028 records and
-- migration 010 used last. `items_no_escape_from_done` names `mutations`, and
-- SQLite re-parses every trigger during `alter table ... rename`, which fails
-- while `mutations` is absent, so the trigger is dropped before the rebuild
-- and recreated after it, byte for byte as migration 018 left it.
--
-- Backfill: every Ticket promoted before this change has no Priority on
-- GitHub, so a Pull that read the field would map each one to `P2` and
-- overwrite the local value. One `pending` Mutation per Backend Ticket in the
-- Pull working set pushes the current Priority. The Mutation is also the Pull shield
-- (ADR-0044): while it is `pending` or `failed`, Pull leaves Priority alone,
-- so no Pull can run between this upgrade and the push. A triage Ticket has
-- no Priority to push. A `done` Ticket leaves the Pull working set (ADR-0034),
-- except one whose close has not reached the Backend: Sync Skip relinquishes
-- a failed close and reopens it (ADR-0046), so it is backfilled too.
--
-- The sequences continue the `mutation_seq` counter that `store::sequences`
-- allocates from, in `created_seq` order. The timestamps come from SQLite's
-- clock because a migration has no `Clock` seam.
drop trigger items_no_escape_from_done;

create table mutations_new (
    sequence integer primary key,
    mutation_type text not null check(mutation_type in (
        'update_ticket','update_epic',
        'set_item_status',
        'set_ticket_priority',
        'add_ticket_to_epic','remove_ticket_from_epic',
        'add_dependency','remove_dependency',
        'add_external_blocker','resolve_external_blocker',
        'promote_ticket','promote_epic'
    )),
    item_id text not null,
    item_class text not null check(item_class in ('ticket','epic')),
    payload_json text not null check(json_valid(payload_json)),
    state text not null check(state in (
        'pending','failed','applying','skipped','cancelled','abandoned','applied'
    )),
    failure_json text check(failure_json is null or json_valid(failure_json)),
    created_at text not null,
    state_changed_at text not null,
    promotion_operation_id text,
    foreign key (item_id, item_class) references items(id, item_class),
    check (
        (state in ('pending','applied') and failure_json is null)
        or
        (state = 'failed' and failure_json is not null)
        or
        (
            state = 'skipped'
            and mutation_type not in ('promote_ticket','promote_epic')
        )
        or
        (state = 'cancelled')
        or
        (
            state in ('applying','abandoned')
            and (
                (mutation_type = 'promote_ticket' and item_class = 'ticket')
                or
                (mutation_type = 'promote_epic' and item_class = 'epic')
            )
        )
    )
) strict;

insert into mutations_new (
    sequence, mutation_type, item_id, item_class, payload_json, state,
    failure_json, created_at, state_changed_at, promotion_operation_id
)
select
    sequence, mutation_type, item_id, item_class, payload_json, state,
    failure_json, created_at, state_changed_at, promotion_operation_id
from mutations;

drop table mutations;
alter table mutations_new rename to mutations;

create index mutations_state_idx on mutations(state, sequence);
create index mutations_promotion_operation_idx on mutations(promotion_operation_id)
where promotion_operation_id is not null;

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

insert into mutations (
    sequence, mutation_type, item_id, item_class, payload_json, state,
    failure_json, created_at, state_changed_at, promotion_operation_id
)
select
    max(
        (select value from sequences where name = 'mutation_seq'),
        (select coalesce(max(sequence), 0) from mutations)
    ) + row_number() over (order by created_seq),
    'set_ticket_priority', id, 'ticket', json_object('priority', priority),
    'pending', null,
    strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
    null
from items
where item_class = 'ticket'
  and backend_key is not null
  and priority is not null
  and (
      status = 'open'
      or exists (
          select 1
            from mutations m
           where m.item_id = items.id
             and m.mutation_type = 'set_item_status'
             and m.state in ('pending', 'failed')
      )
  );

update sequences
   set value = max(value, (select coalesce(max(sequence), 0) from mutations))
 where name = 'mutation_seq';
