# Backend Pull merge skips rows with pending or failed Mutations

When **Backend Pull** returns a `BackendItemSnapshot` whose
`(backend_kind, backend_key)` matches an existing local row, the sync
engine skips the entire row's overwrite if any `pending` or `failed`
**Mutation** targets it. Otherwise it updates `title`, `body`, `status`,
and `updated_at` from the snapshot in one transaction. `skipped` and
`applied` Mutations do not block overwrite — the user explicitly
abandoned the first, the second is history.

The whole-row skip is deliberately coarse: a backend-side body update is
shadowed by a local title edit until **Apply** completes. The net
outcome ("your local edits stay visible until Apply confirms them")
matches user intent and self-heals on the next sync.

Three alternatives were rejected. A **field-level merge** rule per
`MutationType` would grow with every new MutationType and v1 has no
forcing example. **Always overwrite** momentarily flips local titles to
the stale backend value before **Apply** flips them back, producing
visible flicker. **Per-field timestamps** is unreliable because of
clock skew between local and backend, and the engine has no per-field
timestamps anyway.

Pull and Apply run in distinct transactions so a Pull rollback never
partially commits and a per-Mutation failure cannot poison the Pull
merge. Absence from a snapshot is treated as no-op, not a delete signal
— v1 cannot distinguish "backend deleted" from "Pull was filtered" or
"auth hid it," so stranded local Backend items are safer than data
loss on a misread Pull.
