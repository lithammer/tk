# Sync failures split into three audiences by persistence lifetime

The sync engine distinguishes three failure audiences. Each has a different
persistence lifetime, which drives whether a Mutation row transitions and
where the failure is rendered:

- **Environment failures** (executable not found, spawn failed, OOM) — the
  Mutation stays `pending`, the sync run aborts, and the user retries when
  the environment is fixed. No `failed` transition: the failure isn't about
  the Mutation, the Mutation just didn't get a fair attempt.
- **Pull failures mid-sync** (a Backend Pull errored before producing
  snapshot rows) — rendered once on stderr, no Mutation row transition. The
  pull is informational; failures here don't block the outbox or stamp any
  particular Mutation row.
- **Per-Mutation Apply failures** (a Backend Adapter accepted the call but
  the backend rejected the Mutation) — persisted to `mutations.failure_json`,
  state transitions `pending` → `failed`. The engine records evidence on the
  row because `tk sync log`, `tk sync --skip <id>`, and conflict-resolution
  workflows all consume per-row failure data.

The two non-obvious claims are that **Pull failures stamp nothing** (they're
not associated with any outbox row) and that **Apply failures persist per
Mutation** rather than into a global "last failure" or a separate failures
table.

## Considered Options

A typed discriminated-union failure (`rate_limited | validation |
sync_conflict | auth | transient`) was rejected for v1 because subprocess
CLIs like `gh` and `acli` collapse causes into "non-zero exit + stderr
text," making classification from stderr brittle. ADR-0016 amends this ADR
to specify the flat-classified-record shape the first concrete Backend
Adapter populates from real exit codes.
