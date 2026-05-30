# Mutation Failure is a flat classified record, populated by the first adapter

The persisted **Mutation Failure** (`mutations.failure_json`) graduates from
the `{"detail": "..."}` placeholder into a flat JSON record:

```json
{
  "detail": "<human-readable failure>",
  "class": "<FailureClass>",
  "retry_after_s": <int|null>
}
```

`FailureClass` is one of `rate_limited`, `validation`, `sync_conflict`,
`auth`, `transient`, or `unknown`. The variant set is the *named decision*
this ADR records. Values other than `unknown` are assigned by the first
concrete **Backend Adapter** (GitHub via `gh`) from real exit codes and
structured CLI output — not designed from imagination here. The **Backend
Adapter** that produces an Adapter Failure owns the classification; the
sync engine and recovery workflows own retry and recoverability *policy*.
The design is recorded here so the first adapter implements against a
fixed contract and the recovery work unblocks with the vocabulary it needs.

## Decision details

- **Flat enum, not a tagged union.** With per-variant payloads deferred,
  rendering is single-branch (a class label plus the `detail` string), so
  the project precedent for a payload-free closed classification applies:
  a typed enum with a stable `text()` SQL spelling, matching `Priority`,
  `ItemStatus`, and `TicketKind`. **Migration trigger:** when a concrete
  adapter reveals per-variant payloads that *differ* by cause (e.g.
  `validation { field, reason }`, `rate_limited { endpoint }`),
  `FailureClass` migrates to a typed enum-with-variants under that ticket's
  evidence — the forcing constraint AGENTS.md requires before reshaping a
  closed classification.
- **No schema migration.** The `failure_json` CHECK enforces only
  `json_valid` and the state↔presence coupling, not internal shape, so the
  implementing ticket adds no migration. Forward compatibility lives in
  the decoder: `class` must default to `unknown` and `retry_after_s` to
  `null` (so legacy `{"detail":"..."}` rows parse without a missing-field
  error), and the decode site must accept unknown fields so an older
  binary reading a newer row's extra columns does not refuse the row. The
  encoder must produce a stable, generic JSON shape (no custom
  stringification), and the record must serialize `detail`-first so the
  common `unknown` row has a predictable byte order.
- **In-memory vs persisted type stays open.** Whether the adapter's return
  type and the persisted record collapse into one type is *not* decided
  here. Both plausible divergence vectors — adapter-owned per-variant
  payloads and engine-owned bookkeeping (`attempt_count`,
  `first_failed_at`) — are speculative until an adapter or recovery work
  introduces a real field. Defer the split-vs-collapse decision to
  whichever ticket first does.

## Considered Options

- **Ship the typed code now.** Rejected: with no adapter, every row is
  `unknown` — the taxonomy classifies nothing and the rendered class is
  noise. It also forces the structural and type-split bets above without
  evidence, for no live signal.
- **Rich tagged union now** (`rate_limited { retry_after_s, endpoint } |
  validation_failed { field, reason } | ...`). Rejected for the same
  reason ADR-0009 rejected it: subprocess CLIs collapse causes into
  "non-zero exit + stderr," and the per-variant fields are imagined until
  an adapter makes them observable.
- **Append-only `mutation_failures` history table.** Rejected: CONTEXT.md
  defines a Mutation Failure as the *latest* structured failure; a
  history table needs a migration and FK, and recovery workflows consume
  only the latest classification.

## Consequences

- Recovery / conflict-resolution work is unblocked: it has the
  `FailureClass` vocabulary and the decision that recoverability policy
  is its to define, not this ADR's.
- The first concrete Backend Adapter implements against the contract
  above, populating real classes from `gh` failure modes; the decode
  invariants, encoder choice, declaration order, and migration trigger
  are settled.
- Amends ADR-0009: the typed-failure graduation it deferred is now
  specified as a flat classified record rather than a union.

## Amendment (tk-34): the graduation lands in the first adapter

tk-11 shipped this ADR and the CONTEXT.md Adapter Failure entry but
deferred the code (a design ticket). tk-34, the GitHub Backend Adapter,
is the "whichever ticket first does" this ADR named, and it settles the
two questions left open here:

- **In-memory and persisted types collapse into one.** The adapter's
  returned `Failure` and the persisted record are structurally identical
  (`detail`, `class`, `retry_after_s`) and every field is adapter-owned,
  so `FailureClass` and `Failure` carry `serde` directly and persist
  without a separate wire type — the `FailureJsonWrapper` placeholder is
  removed. This follows the `MutationPayload` precedent (the domain type
  owns its JSON). The split stays deferred until an engine-owned field
  (`attempt_count`, `first_failed_at`) actually appears, per the
  divergence trigger above.
- **Conservative, evidence-grounded classifier.** The GitHub adapter
  classifies from a `gh` failure-mode spike, as a pure `(exit_code,
  stderr) -> FailureClass` function: high-signal stable patterns map to
  `auth` / `rate_limited` / `validation` / `sync_conflict` / `transient`,
  and everything else defaults to `unknown`. `retry_after_s` stays `null`
  in v1 — `gh issue` does not reliably surface a reset time, and tk-23
  (recovery policy) is its consumer. No schema migration, per this ADR.
