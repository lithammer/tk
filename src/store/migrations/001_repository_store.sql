create table schema_migrations (
    version integer primary key,
    applied_at text not null
) strict;

create table sequences (
    name text primary key check(name in ('item_created_seq','display_seq','mutation_seq')),
    value integer not null check(value >= 0)
) strict, without rowid;
insert into sequences(name, value) values ('item_created_seq', 0);
insert into sequences(name, value) values ('display_seq', 0);
insert into sequences(name, value) values ('mutation_seq', 0);

create table store_config (
    key text primary key check(key in ('display_prefix')),
    value text not null
) strict, without rowid;

create table items (
    id text primary key,
    display_value text not null collate nocase,
    item_class text not null check(item_class in ('ticket','epic')),
    ticket_kind text check(ticket_kind in ('task','bug')),
    priority text check(priority in ('P0','P1','P2','P3','P4')),
    title text not null check(length(title) > 0),
    body text not null default '',
    container_id text,
    container_class text,
    origin text not null check(origin in ('local','backend')),
    backend_kind text,
    backend_key text,
    status text not null check(status in ('open','active','done')),
    created_seq integer not null unique,
    created_at text not null,
    updated_at text not null,
    display_source text not null generated always as ('display') stored,
    check (
        (item_class = 'ticket' and ticket_kind is not null and priority is not null)
        or
        (item_class = 'epic' and ticket_kind is null and priority is null)
    ),
    check (
        (item_class = 'epic' and container_id is null and container_class is null)
        or
        (item_class = 'ticket')
    ),
    check (
        (container_id is null and container_class is null)
        or
        (container_id is not null and container_class = 'epic')
    ),
    check (
        (origin = 'local' and backend_kind is null and backend_key is null)
        or
        (origin = 'backend' and backend_kind is not null and backend_key is not null)
    ),
    foreign key (container_id, container_class) references items(id, item_class) deferrable initially deferred,
    foreign key (display_value, id, display_source) references item_ids(value, item_id, source) deferrable initially deferred
) strict;
create unique index items_backend_unique on items(backend_kind, backend_key) where backend_kind is not null;
create index items_container_idx on items(container_id) where container_id is not null;
create index items_next_idx on items(priority, created_seq) where status = 'open' and item_class = 'ticket';
create unique index items_id_class_unique on items(id, item_class);

create table item_ids (
    value text primary key collate nocase,
    source text not null check(source in ('display','alias')),
    item_id text not null references items(id) on delete restrict deferrable initially deferred,
    created_at text not null,
    check (length(value) > 0 and not (value glob '*[^A-Za-z0-9._/:#-]*'))
) strict, without rowid;
create unique index item_ids_value_id_source on item_ids(value, item_id, source);
create unique index item_ids_one_display_per_item on item_ids(item_id) where source = 'display';

create table dependencies (
    blocking_id text not null references items(id) on delete restrict,
    blocked_id text not null references items(id) on delete restrict,
    created_at text not null,
    primary key (blocking_id, blocked_id),
    check (blocking_id <> blocked_id)
) strict, without rowid;
create index dependencies_blocked_idx on dependencies(blocked_id);
create index dependencies_blocking_idx on dependencies(blocking_id);

create table external_blockers (
    id text primary key,
    item_id text not null references items(id) on delete restrict,
    reason text not null check(length(reason) > 0),
    created_at text not null,
    resolved_at text
) strict, without rowid;
create index external_blockers_unresolved_idx on external_blockers(item_id) where resolved_at is null;

create table mutations (
    sequence integer primary key,
    mutation_type text not null check(mutation_type in (
        'create_ticket','create_epic',
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
    state text not null check(state in ('pending','failed','skipped','applied')),
    failure_json text check(failure_json is null or json_valid(failure_json)),
    created_at text not null,
    state_changed_at text not null,
    foreign key (item_id, item_class) references items(id, item_class),
    check (
        (state in ('pending','applied') and failure_json is null)
        or
        (state = 'failed' and failure_json is not null)
        or
        (state = 'skipped')
    )
) strict;
create index mutations_state_idx on mutations(state, sequence);

create table remotes (
    name text primary key check(name = 'primary'),
    backend_kind text not null check(backend_kind in ('github','jira')),
    config_json text not null check(json_valid(config_json)),
    created_at text not null,
    updated_at text not null
) strict, without rowid;

create table sync_cursors (
    remote_name text primary key references remotes(name) on delete restrict,
    backend_kind text not null,
    last_applied_sequence integer not null default 0,
    updated_at text not null
) strict, without rowid;

create trigger dependencies_no_cycle before insert on dependencies
for each row when exists (
    with recursive reachable(id) as (
        select new.blocking_id
        union
        select dependencies.blocking_id
          from dependencies, reachable
         where dependencies.blocked_id = reachable.id
    )
    select 1 from reachable where id = new.blocked_id
) begin
    select raise(abort, 'dependency cycle');
end;
