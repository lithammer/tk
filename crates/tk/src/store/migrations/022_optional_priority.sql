-- Priority is optional in every Selection State. Preserve all stored values,
-- including P2: the Store cannot tell a default from an explicit choice.
-- Rebuild with foreign keys off; restore the triggers that name items.
drop trigger former_backend_identity_not_owned_by_another_active_item;
drop trigger items_no_escape_from_done;

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
    effort text
        check (effort is null or (effort in ('low','medium','high') and item_class = 'ticket')),
    -- Selection State is total on Tickets; NULL would otherwise pass CHECK.
    check (
        (item_class = 'ticket' and ticket_kind is not null and selection_state is not null
            and (work_state <> 'active' or selection_state = 'accepted'))
        or
        (item_class = 'epic' and ticket_kind is null and priority is null and selection_state is null)
    ),
    -- Containment (ADR-0021, ADR-0055): only a Ticket has a Container, and the
    -- Container is an Epic or a Ticket. The composite foreign key below pins
    -- `container_class` to the Container's real class. The depth cap — a Ticket
    -- held by a Ticket holds nothing — spans rows; the Store's containment
    -- writer checks it inside the write transaction.
    check (
        (item_class = 'epic' and container_id is null and container_class is null)
        or
        (item_class = 'ticket')
    ),
    check (
        (container_id is null and container_class is null)
        or
        (container_id is not null and container_class in ('epic','ticket'))
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
    selection_state, binding_local_display_value, binding_display_provenance,
    effort
)
select
    id, display_value, item_class, ticket_kind, priority, title, body,
    container_id, container_class, origin, backend_kind, backend_key, status,
    work_state, created_seq, created_at, updated_at, closing_reason,
    selection_state, binding_local_display_value, binding_display_provenance,
    effort
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
