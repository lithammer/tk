create trigger items_no_escape_from_done before update of status on items
for each row when old.status = 'done' and new.status != 'done'
begin
    select raise(abort, 'cannot leave done state');
end;
