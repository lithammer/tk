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

## Pending Promotion visibility

`tk promote` commits Promotion intent before running sync synchronously.
On success, tk records Backend identity before the command returns;
failure or interruption can leave the intent unresolved. Other readers can
also observe it while the command runs.

`tk list`, `tk search`, `tk show`, `tk grep`, `tk plan`, and ordinary `tk next`
must identify each Pending Promotion they render. These Items retain Local
Origin and a local Display ID while later backend-applicable changes queue
behind their Promotion. Hiding that Binding in any of these views makes the
same Item appear unbound depending on which command finds it.

Compact Item rows place `[pending promotion]` immediately before the title.
This covers `list`, `search`, `plan`, ordinary `next`, and the related Item
rows in `show`'s `PARENT`, `TICKETS`, `BLOCKED BY`, and `BLOCKING` sections.
The main Item headers in `show` and `grep` render
`Binding: pending promotion` in the header metadata. Bare Display ID
references in diagnostics and relationship annotations gain no label.
The text names the existing domain concept without a new glyph or legend;
`~` and `⚑` keep their distinct meanings for non-Promotion Mutations.

Both labels cover `pending`, `failed`, and `applying` Promotion Mutations.
They report durable Promotion intent without a recorded Backend identity;
they do not assert that no Backend object exists or that another sync will
resolve the Promotion. The existing Mutation views in `tk show` and
`tk sync log` supply the state detail.

The label and Binding row appear only for Pending Promotion, regardless of
Item Status. They disappear when tk records Backend identity or withdraws
the Promotion intent, even if other Mutations remain unresolved. Unbound
Local Items and Items with Backend identity gain no Binding row; withdrawn
Mutation history remains available in the existing views.

The Repository Store derives the label from the Item's Origin and its own
unresolved Promotion Mutations, alongside the displayed identity in the same
query snapshot. Multi-Item reads resolve that set together rather than
calling the per-Item Binding resolver for each row. The query plan must be
checked on tk's bundled SQLite with representative Mutation history; one SQL
call alone does not rule out repeated Mutation scans.

This guarantees consistency within each Item row. `show` can still observe
different snapshots across its root and related-Item reads during concurrent
writes; this change does not promise a single snapshot for the whole command.

`tk next --quiet` keeps its bare Display ID output.

`tk grep --list` is also an explicit exception. Its one-line
`<display-id>: <title>` form omits all metadata, including Pending Promotion
labels and Binding rows for `pending`, `failed`, and `applying` Promotions.
Default `tk grep` output keeps the Binding row described above.
