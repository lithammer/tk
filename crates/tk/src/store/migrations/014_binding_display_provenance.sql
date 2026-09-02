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

with aliases(item_id, alias_count, alias_value) as materialized (
    select item_id, count(*), min(value)
      from item_ids
     where source = 'alias'
     group by item_id
)
update items
   set binding_local_display_value = case
           when aliases.alias_count = 1 then aliases.alias_value
           else null
       end,
       binding_display_provenance = case
           when aliases.alias_count = 1 then 'known'
           else 'ambiguous'
       end
  from aliases
 where items.id = aliases.item_id
   and items.origin = 'backend';
