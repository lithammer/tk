At completion, report any Mutations remaining in `tk sync log`.

## Remote Work

Promotion and sync are explicit, human-visible operations.

```sh
tk remote
tk promote <id> [--children]
tk sync
tk sync log
```

`tk sync log` lists non-applied Mutations; append a sequence to inspect one,
including applied Mutations.
