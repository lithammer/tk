alter table items add column binding_local_display_value text
    check (
        binding_local_display_value is null
        or (
            length(binding_local_display_value) > 0
            and not (binding_local_display_value glob '*[^A-Za-z0-9._/:#-]*')
        )
    );

alter table items add column binding_display_provenance text not null default 'none'
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
    );

update items
   set binding_local_display_value = (
           select value
             from item_ids
            where item_id = items.id and source = 'alias'
       ),
       binding_display_provenance = 'known'
 where origin = 'backend'
   and (select count(*) from item_ids where item_id = items.id and source = 'alias') = 1;

update items
   set binding_display_provenance = 'ambiguous'
 where origin = 'backend'
   and (select count(*) from item_ids where item_id = items.id and source = 'alias') > 1;
