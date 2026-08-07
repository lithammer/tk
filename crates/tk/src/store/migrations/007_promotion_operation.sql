alter table mutations add column promotion_operation_id text;
create index mutations_promotion_operation_idx on mutations(promotion_operation_id)
    where promotion_operation_id is not null;
