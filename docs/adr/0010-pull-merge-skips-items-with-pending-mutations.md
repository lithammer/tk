# Backend Pull merge skips rows with pending or failed Mutations

When a Pull snapshot matches an existing local row, the sync engine
skips the entire row's overwrite if any `pending` or `failed` Mutation
targets it; otherwise it overwrites `title`, `body`, `status`, and
`updated_at` in one transaction. The whole-row skip is deliberately
coarse — local edits stay visible until Apply confirms them, matching
user intent and self-healing on the next sync.

## Considered Options

Field-level merge (per-MutationType rules for which snapshot fields to
keep) was rejected: the rule set grows with every new MutationType and
v1 has no forcing example. Always overwrite was rejected because it
flips local titles to stale backend values before Apply flips them
back, producing visible flicker. Per-field timestamps was rejected
because clock skew between local and backend makes the comparison
unreliable.

## Consequences

Absence from a snapshot is treated as no-op, not a delete signal: v1
cannot distinguish "backend deleted" from "Pull was filtered" or "auth
hid it," so stranded local Backend items are safer than data loss on
a misread Pull.
