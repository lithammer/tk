# Mutation Failure is a flat classified record, populated by the first adapter

The persisted **Mutation Failure** (`mutations.failure_json`) graduates from
the `{"detail": "..."}` placeholder into a flat record carried by
`Outcome.failure` and serialized in place:

```
MutationFailure { detail: []const u8, class: FailureClass, retry_after_s: ?u32 }
FailureClass = enum { rate_limited, validation, sync_conflict, auth, transient, unknown }
```

The variant set is the *named decision* this ADR records. Values other than
`.unknown` are assigned by the first concrete **Backend Adapter** (tk-34,
GitHub via `gh`) from real exit codes and structured CLI output — not designed
from imagination here. The **Backend Adapter** that produces an Adapter
Failure owns the classification; the sync engine and recovery workflows
(tk-23) own retry and recoverability *policy*. tk-11 ships no `.zig` code: it
settles the shape, the home (`src/domain/mutation_failure.zig`), and the
contract below so tk-34 implements against a fixed target and tk-23 unblocks
with the vocabulary it needs.

## Decision details

- **Flat enum, not a tagged union.** `AGENTS.md` reserves the typed `Outcome`
  tagged union for callers that "dispatch on failure kind across multiple
  stable rendering branches." With per-variant payloads deferred, rendering is
  single-branch (a class label plus the `detail` string), so the project
  precedent for a payload-free closed classification applies instead: a bare
  enum with `text()` / `format`, as on `Priority`, `ItemStatus`, and
  `TicketKind`. **Migration trigger:** when a concrete adapter reveals
  per-variant payloads that *differ* by cause (e.g. `validation { field,
  reason }`, `rate_limited { endpoint }`), `FailureClass` migrates to a union
  under that ticket's evidence — the forcing constraint `AGENTS.md` requires.
  This deliberately deviates from tk-11's "Likely shape" block, which proposed
  the union up front.
- **No schema migration.** The `failure_json` CHECK enforces only
  `json_valid` and the state↔presence coupling, not internal shape, so the
  implementing ticket adds no migration. Forward compatibility must live in the
  decoder: `class` must default to `.unknown` and `retry_after_s` to `null`
  (so legacy `{"detail":"..."}` rows parse without `error.MissingField`), and
  the decode site must pass `.{ .ignore_unknown_fields = true }` so an older
  binary reading a newer row's `class` does not hit `error.UnknownField`. The
  encoder must stay generic `std.json` (no bespoke `jsonStringify`), and the
  record must be declared `detail`-first so the common `.unknown` row
  serializes predictably.
- **In-memory vs persisted type stays as-is.** Whether `Outcome.Failure` (the
  adapter return) and the persisted record collapse into one type is *not*
  decided here. Both plausible divergence vectors — adapter-owned per-variant
  payloads and engine-owned bookkeeping (`attempt_count`, `first_failed_at`) —
  are speculative until an adapter or tk-23 introduces a real field. Defer the
  split-vs-collapse decision to whichever ticket first does.

## Considered Options

- **Ship the typed code now (the `MutationFailure` struct, enum, decode, and
  `tk sync log` rendering in tk-11).** Rejected: with no adapter, every row is
  `.unknown` — the taxonomy classifies nothing and the rendered class is noise.
  It also forces the structural and type-split bets above without evidence, for
  no live signal.
- **Rich tagged union now** (tk-11's "Likely shape":
  `rate_limited { retry_after_s, endpoint } | validation_failed { field, reason }
  | ...`). Rejected for the same reason `docs/adr/0009` rejected it: subprocess
  CLIs collapse causes into "non-zero exit + stderr," and the per-variant fields
  are imagined until an adapter makes them observable.
- **Append-only `mutation_failures` history table.** Rejected: `CONTEXT.md`
  defines a Mutation Failure as the *latest* structured failure; a history table
  needs a migration and FK, and tk-23 consumes only the latest classification.
- **Re-sequence tk-34 before tk-11.** Viable and sanctioned by the tk-40 epic,
  but leaves tk-23 blocked until after the adapter lands. Recording the design
  here instead lets tk-23 proceed now and tk-34 build against a settled
  contract.

## Consequences

- tk-23 (force sync / conflict resolution) is unblocked: it has the
  `FailureClass` vocabulary and the decision that recoverability policy is its
  to define, not tk-11's.
- tk-34 creates `src/domain/mutation_failure.zig` against this contract,
  populating real classes from `gh` failure modes; the home, decode invariants,
  encoder choice, declaration order, and migration trigger are settled.
- Amends `docs/adr/0009-sync-failure-taxonomy.md`: the typed-failure graduation
  it deferred is now specified as a flat classified record rather than a union.
