# Styled output uses named semantic styles with a runtime policy gate

`tk` renders ANSI-styled output through a fixed palette of named semantic
styles (`header`, `kind_bug`, `priority_p0`, …) backed by precomposed SGR
open/close byte sequences, a runtime `Styler` that gates emission on a
resolved `ColorPolicy`, and a styled-span wrapper composed into format
arguments at call sites. The policy is resolved once at top-level dispatch
from `--color=auto|always|never` plus `CLICOLOR_FORCE`, `NO_COLOR`, and
per-stream TTY detection, then carried on `Deps`; commands never re-resolve
it.

## Considered Options

**Call-site shape.** Three shapes were on the table:

- *Free-form chained styling* at every call site
  (`styler.bold().red().write(stdout, "[bug]")`). Rejected because every
  consumer would re-pick `bold` or `red` independently, causing palette
  drift across commands.
- *Hand-written SGR open/close pairs* listed verbatim in a palette module.
  Rejected because the close codes are easy to get wrong: `bold`'s close is
  `22` (not `0`), and a `bold + color` composite needs two separate closes
  in the right order. A precomposed builder captures this once.
- *Named semantic styles*, the chosen shape. The palette exposes names
  (`header`, `kind_bug`, `priority_p0`); call sites use the names. The
  composition runs at definition time so the shipped binary has prebaked
  `open` / `close` byte sequences.

**Policy gate location.** Resolving the policy at flag-parse time and
carrying it on `Deps` was chosen over per-command `--color` flags (which
would multiply the parse surface and risk drift) and over no gating (which
can't honor `NO_COLOR`). The argument parser's positional-stopping config
means trailing `tk list --color=…` does not parse at the top level; this is
accepted because per-command overrides can be added later as a clean
additive extension on top of the inherited default.

## Consequences

- The resolved per-stream color decision is the standard three-arm
  no-color / ANSI / legacy-windows-console union. tk emits SGR bytes only
  and treats the legacy-console mode as `no-color` (output stays plain).
  Users on a modern terminal or a VT-enabled console see colour normally;
  users on legacy `cmd.exe` get plain output rather than literal escape
  codes.
- Nested-safe spans must touch **disjoint SGR families** (foreground color
  vs. bold/dim vs. underline vs. background). Closing an attribute resets
  that family to default; it does not restore a previously-set value. The
  constraint is documented on the `Style` type and asserted by a unit test
  that pairs every currently-defined outer × inner combination. The initial
  palette is constraint-safe: outers live in bold/dim, inners in foreground
  color.
- `stderr` styling has plumbing (per-stream TTY flag on `Deps`, a
  `for_stderr()` selector) but no palette entries in this slice; the
  stderr palette and its wiring into command diagnostics land in a later
  slice.
- All scenario tests continue to assert plain output; the test harness
  defaults TTY detection to false. End-to-end styled behaviour is pinned by
  table-driven unit tests over the palette and by targeted substring
  assertions in command-handler tests that force TTY detection on.
