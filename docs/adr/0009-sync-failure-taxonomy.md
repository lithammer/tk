# Sync failures split into three shapes chosen by engine behavior

The sync engine uses three failure shapes, picked by what the engine
*does* with the failure rather than by classifying the underlying cause:

- Catastrophic environment failures (`ExecutableNotFound`, `SpawnFailed`,
  `OutOfMemory`) are bare error tags rendered to stderr; the **Mutation**
  stays `pending`.
- **Pull** failures mid-sync return `PullError.PullFailed` paired with a
  `?*Diagnostic` carrying captured CLI stderr. No **Mutation** transition
  fires because **Backend Pull** is not tied to a specific row.
- **Apply** failures for a specific **Mutation** return `Outcome.failure
  { detail }`, which the engine persists into `mutations.failure_json`
  and transitions the row to `failed`.

Each shape matches its audience and lifetime: catastrophic failures are
user-fixable environment problems with no persistent home; Pull failures
are ephemeral and rendered once; Apply failures are persistent records
consumed days later by `tk sync log`. A single-shape design would
conflate ephemeral render text with persistable JSON.

A typed discriminated-union failure (`rate_limited | validation |
sync_conflict | auth | transient`) was rejected for v1 because
subprocess CLIs like `gh` and `acli` collapse network/auth/rate-limit/
validation failures into "non-zero exit + stderr text" — reverse-
engineering classification from stderr phrasing is brittle and breaks on
every CLI release. `ticket-11` will graduate `Failure { detail }` into a
typed union once the first concrete **Backend Adapter** makes real
failure modes visible.
