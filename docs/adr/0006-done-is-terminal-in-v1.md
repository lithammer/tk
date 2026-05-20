# Done is terminal in v1

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

Backend Pull's handling of remote reopens for items already imported as
`done` is deferred to the Backend Pull slice; it may require relaxing
the trigger, adding an `origin = 'local'` guard, or shaping Backend
Pull around a delete-and-reinsert pattern.
