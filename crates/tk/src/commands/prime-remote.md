At completion, check `tk sync log` for unresolved or withdrawn Mutations and
report any that remain.

## Remote Work

Promotion and sync are explicit, human-visible operations.

```sh
tk remote
tk promote <id> [--children]
tk sync
tk sync log
```

`tk promote` creates Backend objects for local Items. `tk sync` pulls Backend
changes and applies queued Mutations. `tk sync log` lists every non-applied
Mutation; `tk sync log <sequence>` inspects any Mutation, applied included.
