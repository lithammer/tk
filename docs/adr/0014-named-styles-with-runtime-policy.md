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
can't honor `NO_COLOR`). The original decision called for a top-level
`--color=auto|always|never` flag. That flag is not implemented; tk-154 owns
the decision to add it or drop that part of this ADR.

## Consequences

- The resolved per-stream color decision is ANSI or plain output. tk emits
  SGR bytes only and downgrades unsupported legacy Windows consoles to plain.
  Users on a modern terminal or a VT-enabled console see colour normally;
  users on legacy `cmd.exe` get plain output rather than literal escape
  codes.
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
