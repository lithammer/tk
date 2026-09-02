# Done is terminal in v1

> **Amended by ADR-0046.** Sync Skip of an Item's failed closing Mutation is
> the only v1 exception: it relinquishes the close and restores Lifecycle to
> `open`. General local and remote reopen remain deferred.

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
