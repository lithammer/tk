# Sync failures follow three persistence rules

The sync engine distinguishes three failure audiences. Each has a different
persistence lifetime, which drives whether a Mutation row transitions and
where the failure is rendered:

- **Edit environment failures** (executable not found, spawn failed, or the
  outcome could not be observed after spawn) — the Mutation keeps its prior
  `pending` or `failed` state, the sync run aborts, and the user retries when
  the environment is fixed. No state transition occurs: the failure isn't a
  Backend verdict about the Mutation.
- **Pull failures mid-sync** (a Backend Pull errored before producing
  snapshot rows) — rendered once on stderr, no Mutation row transition. The
  pull is informational; failures here don't block the outbox or stamp any
  particular Mutation row.
- **Per-Mutation Apply failures** (a Backend Adapter accepted the call but
  the backend rejected the Mutation) — persisted to `mutations.failure_json`,
  state transitions `pending` → `failed`. The engine records evidence on the
  row because `tk sync log`, `tk sync --skip <id>`, and conflict-resolution
  workflows all consume per-row failure data.

Creation has a stricter certainty boundary. Before a non-idempotent creation
call, the Promotion Mutation is durably `applying`. Creation exposes no generic
environment-error arm because its Adapter must classify every runner result by
effect certainty. `ProcError::ExecutableNotFound` and
`ProcError::SpawnFailed` occur before a child exists and therefore certify no
effect; `ProcError::OutcomeUnobserved` means a child started but its exit or
captured output could not be observed, so its effect is indeterminate. A
completed nonzero invocation does not itself certify no effect. A confirmed
identity moves the row to `applied`; certified no effect moves it to `failed`; an
ambiguous result remains `applying` with its diagnostic persisted.
`applying` is excluded from automatic replay and blocks all other
remote-changing workflows until explicit recovery resolves it.

Remote-changing commands additionally hold an exclusive repository-scoped OS
file lock from before their availability check through Backend access and
Repository Store persistence. The stable lock path is
`<git-common-dir>/tk/remote.lock`; tk never deletes or replaces it, and process
termination releases it. This closes the live-process check-then-act window;
`applying` remains necessary because a file lock cannot record an ambiguous
creation across a crash. Contention returns a retryable command failure rather
than waiting without a bound. Sync Skip participates in this serialization
because it changes the same outbox ordering state, but it commits before
Adapter open so Backend availability cannot block curation after the lock is
acquired.

If Backend creation returns a confirmed identity but the transaction that
stores it fails, the Mutation remains `applying`. The error preserves the
canonical Backend identity and warns against automatic retry; a generic Store
error would hide that the external object already exists.

The key rules are that **Pull failures stamp nothing** because they do not map
to an outbox row, while **Apply failures persist on each Mutation** rather than
in a global "last failure" record or a separate failures table.

## Considered Options

A typed discriminated-union failure (`rate_limited | validation |
sync_conflict | auth | transient`) was rejected for v1 because subprocess
CLIs like `gh` and `acli` collapse causes into "non-zero exit + stderr
text," making classification from stderr brittle. ADR-0016 amends this ADR
to specify the flat-classified-record shape the first concrete Backend
Adapter populates from real exit codes.
