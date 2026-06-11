# Scenarios exercise real subprocesses; fakes live in unit tests

tk's tests split into two levels, and the boundary between them is the
subprocess seam:

- **Scenario tests** (`crates/tk/tests/scenarios.rs`) drive the built `tk`
  binary as a real subprocess through real git repos, asserting rendered
  stdout/stderr/exit via `insta` + `assert_cmd`. Every executable the binary
  spawns during a scenario is real.
- **Unit tests** inside command and store modules run in-process and
  substitute fakes through `Deps` — `FakeRunner` (`proc.rs`) for
  subprocesses, `FakeClock` (`clock.rs`) for time — scripting exact argv
  expectations and outputs.

**Decision: the scenario layer never injects a fake subprocess runner.**
Commands whose behaviour depends on a non-git subprocess — `sync`, whose
Backend Adapters drive `gh` (tk-34) and `acli` (tk-35), and `self-update`
(`curl`) — are covered by `FakeRunner` unit tests and have no scenarios. This ratifies the existing practice as policy:
scenarios assert the user-visible byte contract through the real OS
boundary; logic branches that depend on subprocess outcomes (spawn failure,
non-zero exit, stderr capture, output parsing) belong to unit tests, where
the trait seam scripts them precisely.

If a non-git-subprocess scenario is ever genuinely needed, the sanctioned
mechanism is a **PATH-shim** — a scripted fake executable placed ahead of
the real one on the scenario's `PATH`. It is hermetic, process-level, and
requires no seam in the production binary. Building that fixture machinery
is deferred until a concrete scenario justifies it, and doing so amends
this ADR.

## Why not inject fakes into scenarios

- **The binary deliberately has no injection channel.** tk-105 removed every
  process-level determinism seam (`TK_NOW`, `SOURCE_DATE_EPOCH`,
  `TK_RAND_SEED`) from the production binary as ambient-env footguns
  (ADR-0018). A `TK_FAKE_RUNNER`-style env mode would reintroduce exactly
  that class of channel, in its most dangerous form: one that swaps out
  subprocess execution.
- **Real gh/acli/curl in scenarios is not an option either.** Those
  executables are non-hermetic (network, auth state, remote data) and
  version-dependent — the opposite of a frozen byte contract. So "scenario
  coverage of sync" has no honest cheap form; pretending otherwise produces
  flaky or vacuous tests.
- **The split keeps each layer honest.** A scenario failure means the
  user-visible contract moved; a `FakeRunner` test failure means a
  subprocess-handling branch broke. Blending the layers blurs what a failure
  means.

## Considered Options

- **Real-everything in scenarios; `FakeRunner` unit tests as the only
  fake-subprocess coverage.** Chosen, as elaborated above.
- **Extend the scenario harness to inject a scripted runner.** Rejected: it
  needs a channel into the spawned binary, which tk-105 deliberately closed.
- **PATH-shim fixtures now.** Rejected as premature: no current scenario
  needs one, and speculative fixture machinery is exactly what the
  evidence-deferral practice (ADR-0016 precedent) avoids. The mechanism is
  designated; the build waits for a consumer.

## Consequences

- `sync` has no scenarios by design; the GitHub Backend Adapter (tk-34)
  ships with `FakeRunner` unit tests only — its formerly deferred
  "end-to-end scripted-gh scenario tests" resolve to *not built* under this
  policy.
- `self-update`'s `curl` path likewise stays unit-only.
- The scenario harness keeps its current determinism story unchanged:
  subprocess isolation, env scrubbing, and `insta` redaction filters
  (ADR-0018) — no new knobs.
- Resolves tk-6, which asked for this stance to be recorded before a
  command invoking a non-git subprocess gained a scenario.
