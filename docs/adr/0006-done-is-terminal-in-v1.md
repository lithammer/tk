# Done is terminal in v1

Once a **Ticket** or **Epic** is `done`, v1 of Ticket refuses to
transition it back to `active` or `open`. `tk start` and `tk stop`
return exit code 1 with a CLI diagnostic; the **Repository Store**
enforces the rule with a schema trigger so any future write path
inherits the protection.

The constraint covers **Item Status** only: title, body, **Priority**,
and **Epic** membership remain editable on a `done` item.

A dedicated `tk reopen` command was rejected for v1 because the
symmetric alternative blurs the intent of `tk done`, would force
**Backend Adapters** to invent a "reopen from closed" semantic, and is
unnecessary while v1 has no recorded need for it. **Backend Pull**'s
handling of remote reopens for items already imported as `done` is
deferred to the Backend Pull slice; it may require relaxing the trigger,
adding an `origin = 'local'` guard, or shaping Backend Pull around a
delete-and-reinsert pattern.
