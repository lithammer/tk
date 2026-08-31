-- Split Work State out of `items.status` (ADR-0043). `status` narrows to the
-- two-valued Lifecycle a Backend Adapter shares with its Backend; the new
-- `work_state` column carries the purely local idle/active axis. Item Status —
-- the three-valued view every command renders — stops being stored and becomes
-- derived from the pair.
--
-- A table-level CHECK cannot be altered in place, so this is a foreign-keys-off
-- `items` rebuild in the shape ADR-0028 records, the same one migrations 005 and
-- 006 used. Each `status = 'active'` row copies across as `open` + `active`;
-- every other row takes `idle`. One column could never hold both, so no
-- pre-existing row lands in the `(done, active)` shape both `done` writers exist
-- to avoid.
--
-- ADR-0029's `active ⟹ accepted` conjunct is relocated, not retired: it now
-- reads `work_state`, and it stays inside the `item_class = 'ticket'` branch
-- because Selection State — Ticket-only (ADR-0027) — is the half of it that has
-- not moved. An Epic carries a NULL Selection State and is active freely.
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
    -- Combined Priority x Selection State x Work State invariant (ADR-0027,
    -- ADR-0028, ADR-0029, ADR-0043): a Ticket carries a Ticket Kind and a
    -- Selection State; `triage` holds no Priority, `accepted`/`parked` require
    -- one; a Ticket is `active` only when `accepted` — the conjunct that used
    -- to read `status` now reads `work_state`; an Epic stays outside all three.
    -- The explicit `selection_state is not null` is load-bearing — it keeps a
    -- NULL on a Ticket a definite CHECK failure rather than a NULL result,
    -- which SQLite would treat as passing.
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
    work_state, created_seq, created_at, updated_at, closing_reason, selection_state
)
select
    id, display_value, item_class, ticket_kind, priority, title, body,
    container_id, container_class, origin, backend_kind, backend_key,
    -- The fused value splits here: `active` was always an open Lifecycle
    -- someone had picked up, so it lands `open` + `active`; every other row
    -- keeps its Lifecycle and starts idle.
    case when status = 'active' then 'open' else status end,
    case when status = 'active' then 'active' else 'idle' end,
    created_seq, created_at, updated_at, closing_reason, selection_state
from items;

drop table items;
alter table items_new rename to items;

create unique index items_backend_unique on items(backend_kind, backend_key) where backend_kind is not null;
create index items_container_idx on items(container_id) where container_id is not null;
-- The partial index tracks the `tk next` candidate filter, which now spans both
-- axes: work already under way is no longer ready, so `work_state = 'idle'`
-- joins `status = 'open'` and `selection_state = 'accepted'` in the predicate.
create index items_next_idx on items(priority, created_seq) where status = 'open' and work_state = 'idle' and item_class = 'ticket' and selection_state = 'accepted';
create unique index items_id_class_unique on items(id, item_class);

create trigger items_no_escape_from_done before update of status on items
for each row when old.status = 'done' and new.status != 'done'
begin
    select raise(abort, 'cannot leave done state');
end;

-- Withdraw the `set_item_status` Mutations the fused column manufactured: before
-- this split, `tk start` and `tk stop` on a Backend-bound Item each queued a push
-- of a Work State no Backend Adapter has a verb for (ADR-0043). This takes the
-- `pending, failed -> cancelled` edge `domain/mutation_state.rs`'s transition
-- table names, which is what lets that table stay a complete index of the edge's
-- callers even though `store::mutations::transition` is not the writer here.
-- `promotion_operation_id is null` spares a row belonging to a Promotion
-- Operation: ADR-0038 leaves those for a human to withdraw. Their exit is for
-- the Adapter to refuse a non-closing target, which ADR-0043 records as an
-- amendment to ADR-0021. That amendment is not implemented yet, so until it
-- is, such a row still applies. Widening this statement to cover them is the wrong fix:
-- it would withdraw Mutations inside a Promotion Operation, which is the line
-- ADR-0038 reserves for a human.
--
-- `state_changed_at` is left UNCHANGED rather than restamped — a migration has
-- no `Clock` seam to sample a fresh timestamp from. Unchanged is not the same
-- as the append time: a row cancelled out of `failed` still carries the moment
-- it failed. `tk sync log` therefore dates these withdrawals by whatever
-- transition last touched them, not by this upgrade.
update mutations set state = 'cancelled'
  where mutation_type = 'set_item_status' and state in ('pending','failed')
    and json_extract(payload_json, '$.status') <> 'done'
    and promotion_operation_id is null;
