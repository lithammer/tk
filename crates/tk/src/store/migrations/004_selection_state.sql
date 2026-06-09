alter table items add column selection_state text
    check (
        (item_class = 'ticket' and selection_state in ('triage','accepted','parked'))
        or
        (item_class = 'epic' and selection_state is null)
    );
update items set selection_state = 'accepted' where item_class = 'ticket';
