# Sync failures split into three shapes chosen by engine behavior

The sync engine uses three failure shapes — bare error tags for
catastrophic environment failures (Mutation stays `pending`),
`?*Diagnostic` for Pull failures mid-sync (rendered once, no Mutation
transition), and `Outcome.failure { detail }` for per-Mutation Apply
failures (persisted to `mutations.failure_json`, row → `failed`) —
because each has a different audience and persistence lifetime.

## Considered Options

A typed discriminated-union failure (`rate_limited | validation |
sync_conflict | auth | transient`) was rejected for v1 because
subprocess CLIs like `gh` and `acli` collapse causes into "non-zero
exit + stderr text," making classification from stderr brittle.
`tk-11` will graduate `Failure { detail }` into a typed union once
the first concrete Backend Adapter makes real failure modes visible.
