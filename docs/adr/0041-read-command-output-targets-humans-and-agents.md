# Read command output targets humans and agents, not parsers

tk's read commands — `tk list`, `tk show`, `tk search`, `tk grep`, `tk next`,
`tk sync log` — render text for a person at a terminal or for an AI agent
reading that same text. tk ships no structured serialization of that output:
no `--json`, no `--porcelain`. Programmatic access to work items is the
**Repository Store** itself, the SQLite database at `.git/tk/tk.db`.

## Why not a machine format

Both intended consumers can read the rendered text. A structured envelope adds
field names and punctuation to every call. At a terminal, rendered text is
easier to scan than JSON. Normal read commands do not require field-by-field
traversal.

The store answers the programmatic question better than a flag could. It has a
declared schema with CHECK constraints, and it holds columns no renderer shows
— `origin`, `backend_key`, `selection_state`, `closing_reason`, and both
timestamps. A `--json` flag would publish a narrower view through a second
surface.

A machine format would also be a second contract. ADR-0017 fixes the verbatim
user-facing strings; a machine format would add field names, nesting, null
handling, and its own versioning, kept beside the renderer and free to drift
from it.

## What this does not decide

Chrome is not serialization. Dropping a legend, a totals line, or a rule from
a read command leaves text a person still reads, so it is decoration rather
than a second format. This ADR does not rule on it, and a low-chrome mode does
not contradict it.

## Considered Options

- **Add `--json` to the read commands.** Rejected. It serves an audience tk
  does not have, duplicates a subset of the store through a second versioned
  surface, and commits the project to a contract beyond ADR-0017's.
- **Adapt output shape to whether stdout is a terminal.** Rejected, though the
  convention is mainstream: `gh issue list` emits tab-separated fields when
  piped and an aligned table on a terminal, `ls` columnizes only on a terminal,
  and ripgrep's `--heading` follows the same rule. Those tools choose between a
  rich table and a degraded one, where adapting earns its keep, and each still
  ships an explicit machine contract over the top — `gh --json`,
  `git --porcelain` — because the terminal-default shape is not a stable
  promise. tk's read output has no such spectrum to move along, so output that
  changes with where stdout points would cost reproducibility and buy nothing.
- **Render for readers, and leave programmatic access in the store.** Chosen.

## Consequences

- Output shape does not depend on where stdout points. A reader gets the same
  text piped, redirected, or on a terminal. Only styling adapts, and ADR-0014
  already scopes that to colour through a per-stream `IsTerminal` probe.
- tk forgoes the `gh --json` integration path. A tool that needs structured
  access must either parse the rendered text or read the store.
- Programmatic access already lives in the store, and a consumer reading it is
  coupled to a schema that migrations change. This ADR records where that
  access is, not a promise that the schema holds still.
- A future request for machine-readable output is answered here first. Reopen
  this decision on evidence of a consumer that is neither a person nor an agent
  reading text; a flag added without that evidence contradicts the premise
  rather than extending it.
- ADR-0017 stays the only contract over what the read commands emit.
