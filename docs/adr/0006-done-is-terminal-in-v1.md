# Done is terminal in v1

> **Amended by ADR-0046 and ADR-0047.** Two v1 exceptions restore Lifecycle
> to `open`: Sync Skip of an Item's failed closing Mutation relinquishes that
> close, and exact Re-Adopt imports the Backend snapshot's Lifecycle onto the
> Item its own Former Backend Identity reserves. General local and remote
> reopen remain deferred.
>
> The two are not equally narrow, and "any future write path inherits the
> protection" below is qualified by the first. Re-Adopt's exception recognises
> a single rebind statement. Sync Skip's recognises a *state*: while an Item
> carries a `failed` `set_item_status` Mutation targeting `done`, the trigger
> admits any `done` -> `open` write on that Item, not only the one Sync Skip
> performs. Every Item without such a Mutation keeps the full protection.

Once a Ticket or Epic is `done`, v1 refuses to transition it back to
`active` or `open`; the Repository Store enforces this with a schema
trigger so any future write path inherits the protection. The
constraint covers Item Status only — title, body, Priority, and Epic
membership remain editable on a `done` item.

## Considered Options

A dedicated `tk reopen` command was rejected because the symmetric
alternative blurs the intent of `tk done`, forces Backend Adapters to
invent a "reopen from closed" semantic, and is unnecessary while v1
has no recorded need for it.

## Consequences

Backend Pull does not observe general remote reopens for Items already imported
as `done`. ADR-0046's Sync Skip exception is narrower and does not add done
Items to the Pull working set.
