create table plan_members (
    item_id text primary key,
    item_class text not null default 'ticket' check (item_class = 'ticket'),
    foreign key (item_id, item_class) references items(id, item_class) on delete cascade
) strict, without rowid;
