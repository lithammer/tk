-- Rebuild `items` to admit triage Tickets, which carry no Priority (ADR-0028).
-- A table-level CHECK cannot be altered in place, so this recreates the table
-- with the combined Priority x Selection State invariant and copies the rows.
-- Runs with foreign_keys disabled (the migration runner's FK-off mode): the
-- DROP fires an implicit DELETE that would otherwise trip the on-delete-restrict
-- foreign keys from dependencies / external_blockers / mutations / item_ids.
create table items_new (
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
    closing_reason text
        check (closing_reason is null or (length(closing_reason) > 0 and status = 'done')),
    selection_state text
        check (selection_state is null or selection_state in ('triage','accepted','parked')),
    -- Combined Priority x Selection State invariant (ADR-0027, ADR-0028): a
    -- Ticket carries a Ticket Kind and a Selection State; `triage` holds no
    -- Priority, `accepted`/`parked` require one; an Epic stays outside all
    -- three. The explicit `selection_state is not null` is load-bearing — it
    -- keeps a NULL on a Ticket a definite CHECK failure rather than a NULL
    -- result, which SQLite would treat as passing.
    check (
        (item_class = 'ticket' and ticket_kind is not null and selection_state is not null
            and (
                (selection_state = 'triage' and priority is null)
                or
                (selection_state in ('accepted','parked') and priority is not null)
            ))
        or
        (item_class = 'epic' and ticket_kind is null and priority is null and selection_state is null)
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

-- `display_source` is generated, so it is omitted from the copy column list.
insert into items_new (
    id, display_value, item_class, ticket_kind, priority, title, body,
    container_id, container_class, origin, backend_kind, backend_key, status,
    created_seq, created_at, updated_at, closing_reason, selection_state
)
select
    id, display_value, item_class, ticket_kind, priority, title, body,
    container_id, container_class, origin, backend_kind, backend_key, status,
    created_seq, created_at, updated_at, closing_reason, selection_state
from items;

drop table items;
alter table items_new rename to items;

create unique index items_backend_unique on items(backend_kind, backend_key) where backend_kind is not null;
create index items_container_idx on items(container_id) where container_id is not null;
-- `selection_state = 'accepted'` matches the `tk next` candidate filter: triage
-- (NULL Priority) and parked Tickets are never selected, so they stay out of
-- the selection index.
create index items_next_idx on items(priority, created_seq) where status = 'open' and item_class = 'ticket' and selection_state = 'accepted';
create unique index items_id_class_unique on items(id, item_class);

create trigger items_no_escape_from_done before update of status on items
for each row when old.status = 'done' and new.status != 'done'
begin
    select raise(abort, 'cannot leave done state');
end;
