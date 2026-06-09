-- Rebuild `items` to enforce `active ⟹ accepted` (ADR-0029): a Ticket may hold
-- Item Status `active` only while its Selection State is `accepted`. This is a
-- row-shape invariant — it constrains only the row's own columns — so it is a
-- declarative CHECK, not a trigger: the `items_no_escape_from_done` trigger
-- exists because done-terminal is a *transition* rule (it reads `old.status`,
-- which a CHECK cannot see), whereas this rule reads only the new row, covers
-- INSERT as well as UPDATE, and folds into the combined Ticket invariant below.
-- A table-level CHECK cannot be altered in place, so this recreates the table
-- and copies the rows, the same foreign-keys-off rebuild migration 005 used.
-- The `tk start` / `tk park` transition helpers own the user-facing diagnostics;
-- this CHECK is the defence-in-depth backstop for any writer that skips them.
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
    -- Combined Priority × Selection State × Item Status invariant
    -- (ADR-0027, ADR-0028, ADR-0029): a Ticket carries a Ticket Kind and a
    -- Selection State; `triage` holds no Priority, `accepted`/`parked` require
    -- one; a Ticket is `active` only when `accepted`; an Epic stays outside all
    -- three. The explicit `selection_state is not null` is load-bearing — it
    -- keeps a NULL on a Ticket a definite CHECK failure rather than a NULL
    -- result, which SQLite would treat as passing.
    check (
        (item_class = 'ticket' and ticket_kind is not null and selection_state is not null
            and (
                (selection_state = 'triage' and priority is null)
                or
                (selection_state in ('accepted','parked') and priority is not null)
            )
            and (status <> 'active' or selection_state = 'accepted'))
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
    container_id, container_class, origin, backend_kind, backend_key,
    -- Heal tk-75-era loophole rows so the copy satisfies the new CHECK rather
    -- than aborting the upgrade: before this slice's start-guard, a store could
    -- reach `active` + `triage`/`parked` (park was status-agnostic; start was
    -- unguarded). Demote such a Ticket to `open` — the equivalent of a stop —
    -- preserving its Selection State. Tickets only; an Epic carries null
    -- selection_state and keeps its status. `updated_at` is left untouched: a
    -- migration repair is not a user edit (the tk-73 backfill precedent).
    case
        when item_class = 'ticket' and status = 'active' and selection_state <> 'accepted'
        then 'open'
        else status
    end,
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
