# A Rust rewrite preserves observable contracts and ports the implementation idiomatically

This ADR records the *method* a Rust rewrite of tk would follow. It does **not**
decide whether to undertake one: the go/no-go remains open and is Peter's call
(tk-80). That trigger decision, when made, amends
`docs/adr/0004-use-zig-0-16-for-the-first-implementation.md`, whose "*first*
implementation" wording already anticipates a second. This ADR settles only how
the port runs *if* it proceeds, so the porting-slice tickets under tk-80 inherit
a fixed method instead of relitigating it.

**Decision: preserve the observable contracts exactly; write the implementation
idiomatically.** Conservative about behavior, liberal about structure. This is
not a midpoint between a mechanical port and a from-scratch rewrite — it is the
idiomatic rewrite, constrained by the existing specification and a differential
oracle so it cannot drift.

## The specification is not the Zig source

tk's behavior is already externalized in three places; the port targets these,
not the `.zig` files (one reference implementation of them):

- **The ADRs** — the durable invariants: untracked Repository Store (0001),
  current-state store + Mutation outbox (0003), SQLite store (0005),
  done-is-terminal (0006), sync failure taxonomy (0009), pull-merge skips items
  with pending Mutations (0010), `tk next` orders by effective priority (0015),
  Mutation Failure record (0016).
- **The test corpus** — the CLI byte-output and exit-code contract, encoded
  black-box (txtar scenarios + command-handler + migration tests).
- **`CONTEXT.md`** — the domain vocabulary. Prose, so it fails no test; drift
  here yields idiomatic-but-vocabulary-frayed Rust. The names stay (Repository
  Store, Workspace Scope, Display ID, Mutation, Backend Adapter, …).

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

- **Oracle:** the txtar scenarios in `src/testing/scenarios/` are the
  byte-exact CLI contract. The Zig `src/testing/script.zig` runner dispatches
  `tk` lines **in-process** via `cli.runArgv` (`script.zig:336`) with injected
  fakes for clock / HTTP / RNG (`:323-333`), so "swap the spawned binary" is
  not possible as the first draft of this ADR assumed. Resolution: a small
  Rust subprocess driver replays the same scenario data files against the Rust
  binary, and the Rust binary exposes **process-level determinism seams**
  matching what the Zig in-process fakes provide — a clock override (e.g.
  `TK_NOW` / `SOURCE_DATE_EPOCH`), an RNG seed (`TK_RAND_SEED`), and, because
  all networking in the Rust port is subprocess (git / gh / acli / curl, per
  tk-80), network faking folds into faking the subprocess runner (PATH-shim or
  a `TK_FAKE_RUNNER` mode). One faking mechanism, not two. The scenario data
  files port verbatim; only the runner changes. This resolves the open `tk-6`
  fakeability TODO at `script.zig:224`. Migrating to `trycmd` stays **last** —
  the Rust subprocess driver is the intermediate oracle.
- **Freeze the Zig tree during the port.** A living oracle requires a still
  oracle. Justified by single-author / ~3-week-old / no externally visible
  users; feature work resumes on the Rust tree.
- **Order: one vertical slice end-to-end first** (`tk init` or `tk show` —
  touches store, render, resolver) to pin the Rust idioms (error plumbing,
  `Deps` shape, trait-vs-concrete seams) under concrete pressure before they
  entrench across ~25k lines. **Then** fan out bottom-up: domain values →
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
  fills that slot with its own curated stderr + exit-code contract
  (`messages.zig`, 0017); type erasure fights it. `color-eyre`'s colorized
  backtrace/span-trace reports are an *anti-goal* for tk's stable, verbatim,
  oracle-asserted error lines. A reporter is acceptable only in test/dev glue,
  not on the user-facing path.
- **Testing:** `insta` (snapshots; `INSTA_UPDATE` ≈ `TK_UPDATE=1`), `assert_cmd`
  + `predicates`, `tempfile`; `trycmd` last.
- **Lints:** `clippy::pedantic` enabled as **warnings** via
  `[workspace.lints.clippy]` in `Cargo.toml` from day one — cheap on greenfield,
  expensive to retrofit. The allow-list is a slice-0 deliverable (near-certain
  entries: `module_name_repetitions` for tk's domain-qualified type names;
  `must_use_candidate` if noisy). CI flips to `-D warnings` after slice-0
  ratifies the allow-list. `clippy::nursery` and `clippy::restriction` excluded
  as groups (WIP / mutually contradictory); individual lints from them only
  when specifically motivated.
- **Manpage:** `include_str!` the hand-authored `man/tk.1` (mirrors today's
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

- The ~25k-line port is *tractable, not small*: the method controls risk and
  quality, it does not shrink the work.
- Refactor tickets shrink to **genuine design improvements** surfaced mid-port
  (e.g. data-driven dispatch), not cleanup of a mechanical translation — there
  is no Zig-shaped-Rust debt to retire.
- Extends `docs/adr/0004`: that ADR anticipated this; the go/no-go amendment to
  0004 is separate and still open. Under a Rust tree this also supersedes
  ADR-0014's comptime-builder mechanism (its styling contract carries over).
- The porting-slice tickets under tk-80 (domain → store → seams → commands →
  remote/sync) are created only after the go/no-go decision.
