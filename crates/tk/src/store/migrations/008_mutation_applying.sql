-- Rebuild `mutations` to add the durable `applying` state used before a
-- non-idempotent Backend creation call. `applying` may carry no failure while
-- the call is in flight, or failure evidence when its effect is indeterminate.
create table mutations_new (
    sequence integer primary key,
    mutation_type text not null check(mutation_type in (
        'update_ticket','update_epic',
        'set_item_status',
        'add_ticket_to_epic','remove_ticket_from_epic',
        'add_dependency','remove_dependency',
        'add_external_blocker','resolve_external_blocker',
        'promote_ticket','promote_epic'
    )),
    item_id text not null,
    item_class text not null check(item_class in ('ticket','epic')),
    payload_json text not null check(json_valid(payload_json)),
    state text not null check(state in ('pending','failed','applying','skipped','applied')),
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
        (state = 'skipped')
        or
        (
            state = 'applying'
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
