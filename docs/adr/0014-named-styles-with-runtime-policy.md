# Styled output uses named semantic styles with a runtime policy gate

`tk` renders ANSI-styled output through a fixed palette of named `anstyle::Style`
constants (`HEADER`, `KIND_BUG`, `PRIORITY_P0`, …). A `Styler` gates emission
on a resolved `ColorChoice` for each stream; call sites wrap text in a named
style and compose it into format arguments. The policy is resolved once at
process startup from `NO_COLOR`, `CLICOLOR_FORCE`, and per-stream TTY
detection, then carried on `Deps`; commands never re-resolve it.

## Considered Options

**Call-site shape.** Three shapes were on the table:

- *Free-form chained styling* at every call site
  (`styler.bold().red().write(stdout, "[bug]")`). Rejected because every
  consumer would re-pick `bold` or `red` independently, causing palette
  drift across commands.
- *Hand-written SGR open/close pairs* listed verbatim in a palette module.
  Rejected because the close codes are easy to get wrong: `bold`'s close is
  `22` (not `0`), and a `bold + color` composite needs two separate closes
  in the right order. The shared close renderer derives these from the style.
- *Named semantic styles*, the chosen shape. The palette exposes names
  (`HEADER`, `KIND_BUG`, `PRIORITY_P0`); call sites use the names. The palette
  defines style values; `anstyle` renders their SGR opens, and tk renders
  family-specific closes.

**Policy gate location.** Resolving the policy once at startup and
carrying it on `Deps` was chosen over per-command `--color` flags (which
would multiply the parse surface and risk drift) and over no gating (which
can't honor `NO_COLOR`). The original decision also called for a top-level
`--color=auto|always|never` flag; see the Amendment below.

## Consequences

- The resolved per-stream color decision is ANSI or plain output. If a Windows
  terminal cannot enable VT and `TERM` does not advertise ANSI support, tk
  downgrades that stream to plain output. Redirected output can still carry
  forced ANSI.
- Nested-safe spans must touch **disjoint SGR families** (foreground color
  vs. bold/dim vs. underline vs. background). Closing an attribute resets
  that family to default; it does not restore a previously-set value. The
  palette documents this constraint, and a unit test checks every allowed
  outer/inner pair. Row and header styles use bold/dim; their inner spans
  use foreground color. The red+bold error label never nests.
- `Styler::for_stderr()` selects the stderr policy. The shared command-error
  renderer uses a red+bold error label, as scoped in ADR-0032. It uses the
  existing policy: nonempty `NO_COLOR` disables styling; otherwise,
  nonempty `CLICOLOR_FORCE` forces it, including on non-TTY stderr;
  otherwise, stderr's own TTY state decides. Unforced non-TTY output
  stays byte-identical.
- Scenario snapshots assert plain output. A separate scenario checks forced
  stderr color and `NO_COLOR` precedence. Palette tests pin the SGR bytes;
  dispatch tests check prefix boundaries and independent stream choices.

## Amendment: the colour policy is env and TTY only

The top-level `--color=auto|always|never` flag the original decision called
for was never built, and is dropped rather than finished. Nothing is removed
from the CLI; the env and TTY chain above has always been the whole policy.

The flag would reach nothing the chain does not. `--color=never` is
`NO_COLOR=1`, `--color=always` is `CLICOLOR_FORCE=1` — with `NO_COLOR=`
beside it when one is inherited, since `NO_COLOR` wins the chain — and
`--color=auto` is the default, because `resolve_styler_from_env` reads an
empty value as unset. What a flag adds is a second spelling for each arm,
visible in `--help` where the env vars are not; tk(1)'s ENVIRONMENT section
carries them instead.

Against that, a flag lands on the wrong side of the startup seam. `main`
resolves the styler and wraps both streams in `anstream::AutoStream` before
`run_argv` parses argv, so a parsed flag arrives after the streams it would
govern. Honoring it needs either a second argv reader in `main`, free to
drift from clap's, or the parse moved out of `run_argv` — which the
command-handler tests call directly, each injecting its own `Styler` through
`Deps`. Both give the policy a second source of truth, which is the drift
the "resolved once, never re-resolved" rule exists to prevent.

**Scope.** This policy governs what tk writes through `Deps`. clap's help
and usage errors sit outside it. `render_clap_error` prints `err.render()`
through `StyledStr`'s `Display` impl, which walks the text parts and emits
no SGR, so clap's chrome is plain on a terminal and off it. Styling it is a
separate decision needing its own evidence, not an extension of this one.
