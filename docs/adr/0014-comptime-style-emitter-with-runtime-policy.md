# Styled output uses comptime-composed semantic styles with a runtime policy gate

`tk` renders ANSI-styled output through a comptime builder that
concatenates SGR open/close codes into named `Style` constants
(`palette.zig`), a runtime `Styler` that gates emission on a resolved
`ColorPolicy`, and a `Styled` format-value returned by `styler.wrap`
so styled spans compose into `print()` format strings as `{f}` args.
The policy is resolved once in `main.zig` from
`--color=auto|always|never` plus `CLICOLOR_FORCE`, `NO_COLOR`, and
`tty_stdout` / `tty_stderr`, then carried on `Deps`; commands never
re-resolve it. `tk-4` contains the concrete slice plan and the initial
palette.

## Considered Options

**Call-site shape.** Three shapes were on the table:

- *Free-form chameleon-style chaining* at every call site
  (`styler.bold().red().write(stdout, "[bug]")`). Rejected because
  every consumer would re-pick `bold` or `red` independently, causing
  palette drift across commands.
- *Hand-written SGR open/close pairs* (`Style{ .open = "\x1b[1m",
  .close = "\x1b[22m" }`) listed verbatim in `palette.zig`. Rejected
  because the close codes are easy to get wrong: `bold`'s close is
  `22` (not `0`), and a `bold + color` composite needs two separate
  closes in the right order. A comptime builder captures this once.
- *Comptime semantic styles*, the chosen shape. `palette.zig` exposes
  names (`header`, `kind_bug`, `priority_p0`); call sites use the
  names (`styler.wrap(styles.kind_bug, "[bug]")`). The comptime
  builder lives only at definition time; the shipped binary has
  prebaked `open` / `close` byte slices.

**Policy gate location.** Resolving the policy at flag-parse time in
`cli.zig` and carrying it on `Deps` was chosen over per-command
`--color` flags (which would multiply the parse surface and risk
drift) and over no gating (which can't honor `NO_COLOR`). zig-clap's
`terminating_positional = 0` config means trailing
`tk list --color=...` does not parse at the top level; this is
accepted because per-command overrides can be added later as a clean
additive extension on top of the inherited default.

**Vendoring chameleon.** Rejected because chameleon
(`tr1ckydev/chameleon`) has not migrated to Zig 0.16 and the project
would inherit a tracking burden. tk reimplements the small comptime
builder it needs in `src/render/style.zig`; the inspiration is
acknowledged in code comments.

## Consequences

- Nested-safe spans must touch **disjoint SGR families** (foreground
  color vs. bold/dim vs. underline vs. background). Closing an
  attribute resets that family to default; it does not restore a
  previously-set value. The constraint is documented on `Style` and
  asserted by a unit test that pairs every currently-defined outer ×
  inner combination. The initial palette is constraint-safe: outers
  live in bold/dim, inners in foreground color.
- `stderr` styling has plumbing (`tty_stderr` on `Deps`,
  `styler.forStderr()`) but no palette entries in this slice; `tk-43`
  introduces the stderr palette and wires it into command
  diagnostics.
- All txtar scenarios continue to assert plain output; the test
  harness defaults `tty_stdout = false`. End-to-end styled behaviour
  is pinned by Zig unit tests in `src/render/styler.zig`
  (table-driven over `palette.zig`) and by targeted
  `mem.indexOf`-style assertions in `tk-27` / `tk-29` using
  `Harness.withTtyStdout(true)`. `TK_UPDATE=1` continues to
  regenerate plain snapshots only.
