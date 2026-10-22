-- Cut Priority to `P0`..`P3` (ADR-0054). A stored `P4` becomes `P3`, done
-- Tickets included, and the `priority` CHECK stops naming `P4`.
--
-- A column CHECK cannot be altered in place, so this is a foreign-keys-off
-- `items` rebuild in the shape ADR-0028 records. It reproduces everything the
-- table carries: the columns migration 014 added, the three explicit
-- indexes, and the three triggers. `items_next_idx` stays absent
-- (migration 012, ADR-0045).
--
-- `former_backend_identity_not_owned_by_another_active_item` lives on
-- `former_backend_identities` but names `items`. SQLite re-parses every trigger
-- during `alter table ... rename`, which fails while `items` is absent, so the
-- trigger is dropped before the rebuild and recreated after it.
--
-- Mutation payloads hold no Priority, so no queued Mutation needs rewriting.
drop trigger former_backend_identity_not_owned_by_another_active_item;

create table items_new (
    id text primary key,
    display_value text not null collate nocase,
    item_class text not null check(item_class in ('ticket','epic')),
    ticket_kind text check(ticket_kind in ('task','bug')),
    priority text check(priority in ('P0','P1','P2','P3')),
    title text not null check(length(title) > 0),
    body text not null default '',
    container_id text,
    container_class text,
    origin text not null check(origin in ('local','backend')),
    backend_kind text,
    backend_key text,
    status text not null check(status in ('open','done')),
    work_state text not null default 'idle' check(work_state in ('idle','active')),
    created_seq integer not null unique,
    created_at text not null,
    updated_at text not null,
    display_source text not null generated always as ('display') stored,
    closing_reason text
        check (closing_reason is null or (length(closing_reason) > 0 and status = 'done')),
    selection_state text
        check (selection_state is null or selection_state in ('triage','accepted','parked')),
    binding_local_display_value text
        check (
            binding_local_display_value is null
            or (
                length(binding_local_display_value) > 0
                and not (binding_local_display_value glob '*[^A-Za-z0-9._/:#-]*')
            )
        ),
    binding_display_provenance text not null default 'none'
        check (
            (origin = 'local'
             and binding_display_provenance = 'none'
             and binding_local_display_value is null)
            or
            (origin = 'backend' and (
                (binding_display_provenance = 'none'
                 and binding_local_display_value is null)
                or
                (binding_display_provenance = 'known'
                 and binding_local_display_value is not null)
                or
                (binding_display_provenance = 'ambiguous'
                 and binding_local_display_value is null)
            ))
        ),
    -- Combined Priority x Selection State x Work State invariant (ADR-0027,
    -- ADR-0028, ADR-0029, ADR-0043): a Ticket carries a Ticket Kind and a
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
            and (work_state <> 'active' or selection_state = 'accepted'))
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
    work_state, created_seq, created_at, updated_at, closing_reason,
    selection_state, binding_local_display_value, binding_display_provenance
)
select
    id, display_value, item_class, ticket_kind,
    case when priority = 'P4' then 'P3' else priority end,
    title, body, container_id, container_class, origin, backend_kind,
    backend_key, status, work_state, created_seq, created_at, updated_at,
    closing_reason, selection_state, binding_local_display_value,
    binding_display_provenance
from items;

drop table items;
alter table items_new rename to items;

create unique index items_backend_unique on items(backend_kind, backend_key) where backend_kind is not null;
create index items_container_idx on items(container_id) where container_id is not null;
create unique index items_id_class_unique on items(id, item_class);

create trigger active_backend_identity_not_owned_by_another_former_item_insert
before insert on items
when new.backend_kind is not null
 and exists (
    select 1
      from former_backend_identities
     where backend_kind = new.backend_kind
       and backend_key = new.backend_key
       and item_id <> new.id
 )
begin
    select raise(abort, 'backend identity is owned by another Item');
end;

create trigger active_backend_identity_not_owned_by_another_former_item_update
before update of backend_kind, backend_key on items
when new.backend_kind is not null
 and exists (
    select 1
      from former_backend_identities
     where backend_kind = new.backend_kind
       and backend_key = new.backend_key
       and item_id <> new.id
 )
begin
    select raise(abort, 'backend identity is owned by another Item');
end;

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

create trigger former_backend_identity_not_owned_by_another_active_item
before insert on former_backend_identities
when exists (
    select 1
      from items
     where backend_kind = new.backend_kind
       and backend_key = new.backend_key
       and id <> new.item_id
)
begin
    select raise(abort, 'backend identity is owned by another Item');
end;
