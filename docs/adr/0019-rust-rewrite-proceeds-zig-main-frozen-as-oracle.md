# The Rust rewrite proceeds; Zig main is frozen as the oracle

ADR-0004 chose Zig 0.16 for the *first* implementation as an exploration
opportunity and anticipated a second. Development now moves to Rust on
`rust-main`; `main` (Zig) is frozen and serves only as the oracle source for
the differential port governed by ADR-0018. ADR-0004 remains the historical
record of the first implementation.

## Rationale

The exploration produced concrete signal at exactly the friction points
ADR-0004 named:

- **Stronger algebraic domain modeling.** AGENTS.md now devotes a whole
  section to three hand-rolled error shapes (bare tag, `?*Diagnostic`
  out-param, typed `Outcome` union) that exist to work around Zig's
  payload-less error tags. Rust collapses them to `Result<T, E>` + enums
  (recorded as the typed-domain idiom in ADR-0018).
- **Mature CLI snapshot testing.** ~1,890 LOC of custom test infrastructure
  has accumulated (incl. the ~1,000-line `script.zig` testscript runner) to
  reproduce capabilities Rust ships in `insta` / `assert_cmd` / `trycmd`.
- **Pre-1.0 language churn.** Zig 0.16 is pinned exactly because the language
  is unstable; the tax is recurring.

The switch cost is low: one author, about three weeks of work, about 25,000
lines, and no externally visible users. ADR-0018 defines the method and aligns
the oracle plan with how `script.zig` runs.

## Considered Options

- **Continue in Zig.** Rejected: the friction predicted by ADR-0004 has
  materialized exactly where it was named (errors, test tooling, language
  churn), and the cost of switching compounds with every additional feature.
- **Defer to a later date.** Rejected: each new Zig feature would make a later
  switch costlier.

## Consequences

- All new development moves to `rust-main` per ADR-0018.
- `main` (Zig) is frozen — no feature work, only maintenance that keeps the
  oracle buildable from a pinned worktree during the port.
- The porting-slice tickets under tk-80 are unblocked. Slice-0 (`tk init`
  end-to-end + idiom ratification + oracle harness + determinism seams + lint
  allow-list) is the immediate deliverable; downstream slices land as thin
  stubs that inherit ADR-0018's constraints (domain → store → seams →
  commands → remote/sync).
- ADR-0004 stays as the historical record of the first-implementation
  decision and its predicted friction; this ADR records that its anticipated
  second implementation has been triggered.
