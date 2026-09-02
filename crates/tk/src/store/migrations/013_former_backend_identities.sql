create table former_backend_identities (
    backend_kind text not null check(backend_kind in ('github','jira')),
    backend_key text not null,
    item_id text not null references items(id) on delete restrict,
    backend_display_value text not null collate nocase,
    detached_seq integer not null unique check(detached_seq > 0),
    detached_at text not null,
    primary key (backend_kind, backend_key),
    check (length(backend_key) > 0),
    check (
        length(backend_display_value) > 0
        and not (backend_display_value glob '*[^A-Za-z0-9._/:#-]*')
    )
) strict, without rowid;

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

create trigger former_backend_identity_ownership_is_immutable
before update of backend_kind, backend_key, item_id on former_backend_identities
when new.backend_kind <> old.backend_kind
  or new.backend_key <> old.backend_key
  or new.item_id <> old.item_id
begin
    select raise(abort, 'former backend identity ownership is immutable');
end;

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
