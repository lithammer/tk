# A Rust rewrite preserves observable contracts and ports the implementation idiomatically

This ADR records the *method* the Rust rewrite of tk followed. The go/no-go
was a separate decision: ADR-0019 triggered the rewrite on 2026-05-28
(fulfilling the second implementation ADR-0004 anticipated), and the
porting-slice tickets under tk-80 inherited the fixed method recorded here
instead of relitigating it. The canonical implementation now lives under
`crates/tk/`.

**Decision: preserve the observable contracts exactly; write the implementation
idiomatically.** Conservative about behavior, liberal about structure. This is
not a midpoint between a mechanical port and a from-scratch rewrite — it is the
idiomatic rewrite, constrained by the existing specification and a differential
oracle so it cannot drift.

## The specification is not the Zig source

tk's behavior was already externalized in three places; the port targeted
these, not the `.zig` files (one reference implementation of them):

- **The ADRs** — the durable invariants: untracked Repository Store (0001),
  current-state store + Mutation outbox (0003), SQLite store (0005),
  done-is-terminal (0006), sync failure taxonomy (0009), pull-merge skips items
  with pending Mutations (0010), `tk next` orders by effective priority (0015),
  Mutation Failure record (0016).
- **The test corpus** — the CLI byte-output and exit-code contract, encoded
  black-box (txtar scenarios + command-handler + migration tests).
- **`CONTEXT.md`** — the domain vocabulary. Prose, so it fails no test; drift
  here yields idiomatic-but-vocabulary-frayed Rust. The names stay (Repository
  Store, Display ID, Mutation, Backend Adapter, …).

## Frozen contracts vs. idiomatic internals

- **Frozen:** CLI flags / stdout-stderr bytes / exit codes; the SQLite schema
  and migrations (the durable artifact); the domain invariants above.
- **Idiomatic — Zig shape deliberately *not* preserved:** error handling
  collapses to `Result<T, E>` + enums (the three AGENTS.md shapes — bare tag,
  `?*Diagnostic` out-param, typed `Outcome` — were workarounds for payload-less
  Zig errors); manual `deinit` / `gpa` threading → ownership + `Drop`;
  `*anyopaque` vtable seams (`proc.Runner`, `http.Http`) → traits; out-param
  init (`buildItemDetail`) → returns/builders.

## Differential oracle and order

- **Oracle:** the txtar scenarios in the Zig tree's `src/testing/scenarios/`
  were the byte-exact CLI contract. The Zig `script.zig` runner dispatched
  `tk` lines **in-process** with injected fakes for clock / HTTP / RNG, so
  "swap the spawned binary" was not possible; the Rust side instead replays
  the contract against the **built binary as a subprocess** through an
  `insta` + `assert_cmd` harness (`crates/tk/tests/scenarios.rs`).
- **Determinism comes from tests, not the binary.** In-process tests inject
  dependencies (`Deps` with `FakeClock` / a seeded `StdRng`); subprocess
  tests redact the few nondeterministic spans (timestamps, git's
  environment-variable stderr) via `insta` filters. The production binary
  reads **no determinism env knobs** — a runtime `SOURCE_DATE_EPOCH` read is
  a determinism seam in disguise, not a reproducible-builds feature (that
  convention is build-time), and any such ambient-env channel is a footgun.
  Because all networking is subprocess (git / `gh` / `acli` / `curl`, per
  tk-80), network test coverage is subprocess test coverage; the standing
  policy for faking non-git subprocesses is ADR-0031 (scenarios exercise
  real subprocesses; fakes live in unit tests).
- **Freeze the Zig tree during the port.** A living oracle requires a still
  oracle. Justified by single-author / ~3-week-old / no externally visible
  users; feature work resumed on the Rust tree (ADR-0019), and the Zig tree
  was removed once the port completed.
- **Order: one vertical slice end-to-end first** (`tk init` — touches store,
  render, resolver) to pin the Rust idioms (error plumbing, `Deps` shape,
  trait-vs-concrete seams) under concrete pressure before they entrenched
  across ~25k lines. **Then** fan out bottom-up: domain values →
  store/migrations → proc/git/http seams → remaining commands → remote/sync
  last.

## Dependency baseline

Confirmed empirically by the first vertical slice; recorded so slices inherit
it:

- **CLI:** `clap` (derive) — replaces hand-rolled dispatch and per-command
  `writeHelp`; help/usage/version generated.
- **Store:** `rusqlite` with `bundled` (single static binary); inline SQL as raw
  string literals (`r#"…"#`); no compile-time SQL macro (that is `sqlx`, ruled
  out by the no-async constraint) — queries stay runtime-checked by
  migration/store tests.
- **Cross-compile:** `cargo-zigbuild` uses Zig as the C cross-compiler/linker
  for bundled SQLite, preserving the single-Linux-host six-triple release
  (0011).
- **Styling:** `anstyle` + `anstream`. ADR-0014's *contract* is preserved (named
  semantic styles, policy resolved once and carried on `Deps`, legacy console →
  plain output); only its comptime-builder *mechanism* is replaced. These crates
  resolve the same `NO_COLOR` / `CLICOLOR_FORCE` / TTY inputs. clap styles only
  its own help/errors (itself via `anstyle`); `tk show` / `tk list` output
  styling is tk's own `render/` concern, but sharing `anstyle` gives one style
  vocabulary across both. Reject `owo-colors`-style per-call-site chaining — it
  reintroduces the palette drift ADR-0014 rejected.
- **Errors:** `thiserror` across the typed domain/store/sync layers (zero
  runtime cost, no API footprint, serves the typed taxonomies). The
  dynamic-reporter family — **`anyhow` / `eyre` / `color-eyre` — is declined.**
  They solve a different job (report dynamic errors at the boundary), but tk
  fills that slot with its own curated stderr + exit-code contract (ADR-0017);
  type erasure fights it. `color-eyre`'s colorized backtrace/span-trace
  reports are an *anti-goal* for tk's stable, verbatim, oracle-asserted error
  lines. A reporter is acceptable only in test/dev glue, not on the
  user-facing path.
- **Testing:** `insta` (snapshots; `INSTA_UPDATE` / `cargo insta`), `assert_cmd`
  + `predicates`, `tempfile`.
- **Lints:** `clippy::pedantic` enabled as **warnings** via
  `[workspace.lints.clippy]` in `Cargo.toml` from day one — cheap on greenfield,
  expensive to retrofit. The allow-list is a slice-0 deliverable (near-certain
  entries: `module_name_repetitions` for tk's domain-qualified type names;
  `must_use_candidate` if noisy). CI flips to `-D warnings` after slice-0
  ratifies the allow-list. `clippy::nursery` and `clippy::restriction` excluded
  as groups (WIP / mutually contradictory); individual lints from them only
  when specifically motivated.
- **Manpage:** `include_str!` the hand-authored `man/tk.1` (mirrors the Zig
  `@embedFile`); keep the no-CR guard via `.gitattributes` + a test.
  `clap_mangen` / `clap_complete` (generate manpage + completions from the clap
  definitions) are **future improvements**, not initial scope.
- **Self-update:** subprocess `curl`, no embedded HTTP/TLS (recorded in tk-80's
  body); revisit a typed client (`ureq` / `minreq`) only if error handling gets
  fiddly. `indoc` declined (SQL is whitespace-insensitive; clap removes the
  help-text literals).

## Considered Options

- **Faithful mechanical port, track refactors as tickets.** Rejected: produces a
  permanent stratum of Zig-shaped Rust (mimicked vtables, manual lifetimes, the
  three error shapes) that a single-author project never reprioritizes away, and
  it forfeits the "stronger algebraic domain modeling" ADR-0004 named as Rust's
  draw. Faithful translation is right at the line level (don't gratuitously
  rework control flow) and wrong at the type level.
- **Idiomatic from scratch, unconstrained.** Rejected: changing language and
  design at once drifts behavior and risks silently violating a recorded
  invariant (e.g. 0010, 0015). The fix is not less idiomatic code but the spec +
  oracle constraints above — which is the chosen strategy.

## Consequences

- The ~25k-line port was *tractable, not small*: the method controlled risk and
  quality, it did not shrink the work.
- Refactor tickets shrank to **genuine design improvements** surfaced mid-port
  (e.g. data-driven dispatch), not cleanup of a mechanical translation — there
  is no Zig-shaped-Rust debt to retire.
- Extends ADR-0004 (which anticipated this second implementation; ADR-0019
  records the trigger). Under the Rust tree this also supersedes ADR-0014's
  comptime-builder mechanism (its styling contract carries over).

## History

- The original oracle plan was an intermediate Rust subprocess driver replaying
  the txtar files against a binary exposing **process-level determinism seams**
  (`TK_NOW` / `SOURCE_DATE_EPOCH`, `TK_RAND_SEED`), with subprocess faking via
  a PATH-shim or a `TK_FAKE_RUNNER` env mode, and a final migration to
  `trycmd`. As executed: tk-99 replaced the driver with the `insta` +
  `assert_cmd` harness, tk-105 removed the shipped env seams in favour of
  dependency injection + `insta` redactions, `TK_FAKE_RUNNER` was never built,
  and `trycmd` was dropped. The original text also claimed to resolve the tk-6
  fakeability question via those seams; it did not — the standing stance was
  recorded later as ADR-0031.
