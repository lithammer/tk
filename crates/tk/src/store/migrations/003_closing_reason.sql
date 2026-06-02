alter table items add column closing_reason text
    check (closing_reason is null or (length(closing_reason) > 0 and status = 'done'));
