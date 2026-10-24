-- Add Ticket Effort (ADR-0054) and the `set_ticket_effort` Mutation Type that
-- pushes it.
--
-- `items.effort` is optional, so `alter table ... add column` suffices, as for
-- migration 003's `closing_reason`; SQLite checks the CHECK against existing
-- rows, which all hold NULL. Only a Ticket carries one.
--
-- The `mutation_type` CHECK cannot be altered in place, so `mutations` is
-- rebuilt in migration 019's shape, dropping and recreating
-- `items_no_escape_from_done` around it for the same reason.
--
-- No backfill: every existing Ticket has no Effort, and an issue with no
-- Effort reads back as none, so Pull agrees with the store from the start.
alter table items add column effort text
    check (effort is null or (effort in ('low','medium','high') and item_class = 'ticket'));

drop trigger items_no_escape_from_done;

create table mutations_new (
    sequence integer primary key,
    mutation_type text not null check(mutation_type in (
        'update_ticket','update_epic',
        'set_item_status',
        'set_ticket_priority','set_ticket_effort',
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
