-- Rebuild `mutations` to add the terminal `cancelled` state that Promotion
-- Cancellation writes. A cancelled Mutation was withdrawn before any Backend
-- attempt, so it may carry the failure evidence of an earlier certified
-- rejection or none at all.
--
-- The `skipped` clause also gains a Mutation Type restriction: Sync Skip is
-- defined as bypassing one failed Mutation, and a Promotion leaves the queue
-- only through cancellation.
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
    state text not null check(state in (
        'pending','failed','applying','skipped','cancelled','applied'
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
