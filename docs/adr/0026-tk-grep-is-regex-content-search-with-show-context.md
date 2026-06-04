# tk grep is regex content search rendered as streamed tk-show context; ranked recall belongs to tk search

`tk grep <pattern>` (tk-113) finds **Tickets** and **Epics** whose **title** or
**body** text matches a **regular expression**, and renders each match as a
**`tk show`**-style block with the body collapsed to the matching lines plus
surrounding context, like `grep -C`. It is the content-search companion to
**`tk search`**: where **`tk search`** answers *"which item is it?"* by title and
returns **`tk list`** rows, **`tk grep`** answers *"where does this text
appear?"* and returns matches in context. ADR-0025 established that the distinct
*output* — show-style line context, not a list row — is what justifies the
second verb.

The pattern is a regular expression **by default** (Rust `regex` crate,
RE2-family, equivalent to `grep -E`; no backreferences or lookaround). Matching
runs **per line in Rust** over the title (line 0) and body, streamed one item at
a time from the **Repository Store** straight to stdout, so peak memory is one
item regardless of store size. Matches render in creation order and are **never
ranked**.

## Considered Options

The decision turned on the matching model. The `grep -C` line-context output is
matcher-agnostic — every model must materialise a matched body in Rust and slice
line hunks — so it did not discriminate the options; *name-honesty*, the
*frozen default-semantics contract*, and *output shape* did.

- **Literal substring (the `tk search` model).** Rejected as a *trap*, not
  merely as under-powered. A command named `grep` connotes pattern matching, and
  the one change that is **not** additive later is flipping the default from
  literal to regex: the day you do, every pattern containing `. * + ? ( ) | [ ]`
  silently changes meaning for existing scripts. "Literal now, regex later" is
  therefore the one path that cannot be taken back.
- **Regular expression (chosen).** A strict superset of literal: a
  metacharacter-free pattern matches exactly as the literal `instr` would, so
  adopting regex loses no recall and adds power additively, with
  `-F`/`--fixed-strings` as the future literal escape hatch. The RE2 dialect
  (no backreferences/lookaround) is the same well-understood limit ripgrep
  carries. Matching **per line in Rust** lets `^`/`$` anchor at line boundaries
  for free and keeps a clean 1:1 match-to-hunk mapping; because **`tk grep`**
  must materialise matched bodies in Rust anyway to build context, it needs
  neither a SQL `regexp()` function nor rusqlite's `functions` feature.
- **FTS5 full-text.** Rejected for **`tk grep`**. It is tokenised, not
  substring/regex (`auth` misses `authentication`), and its output is
  bm25-ranked rows with `snippet()` excerpts — a relevance list that cannot
  inhabit grep's deterministic, position-ordered line-context block. It also
  drags an external-content index, sync triggers on the `strict` `items` table,
  an auto-applied forward migration (ADR-0024), and staleness risk on the
  **Backend** sync-apply path. Its ranked-snippet shape is the *feature* of a
  future ranked **`tk search`** mode, where it belongs.
- **Levenshtein / fuzzy.** Rejected for **`tk grep`**, and more firmly than
  literal: `grep` is never typo-tolerant, and edit distance yields a
  threshold-gated, distance-ranked result with no slot in the line-context
  block. `spellfix1`/`editdist3` are absent from tk's bundled SQLite, so it
  would require the Rust `strsim` crate. Its home is typo-tolerant *title*
  recall in **`tk search`** — the "fuzzy title recall" ADR-0025 already names as
  search's distinct value.

The deciding insight: a ranked, thresholded, scored result is structurally
incompatible with streaming one matched item to stdout at a time, so **positional
order and "never ranked" are the same decision** — and that decision is what
keeps both FTS5 and fuzzy on the **Search** side of the line, as separate future
work, not part of tk-113.

## Consequences

These user-facing contracts are frozen because none can change additively later:

- **Default matching is regex** (RE2-family, ≈ `grep -E`; no
  backreferences/lookaround). Help text states the dialect so the divergence
  from classic BRE `grep` (`+ ? | ( )` are metacharacters) is explicit.
- **Case-sensitive by default**, with a future `-i`/`--ignore-case`. This
  deliberately diverges from **`tk search`**'s case-insensitive title match;
  the divergence matches the `grep` namesake and is intentional.
- **Whole-store, every Item Status**, and **`TK_SCOPE` is ignored** (ADR-0022):
  a lookup must not be silently narrowed — you grep precisely because you do not
  know where the text lives.
- **Matches render in creation order and are never ranked.** Relevance/edit-distance
  ordering is out of scope and reserved for future **`tk search`** modes.
- **Exit codes overload `0`/`1` as a `grep -q`-style predicate** (ARCHITECTURE.md):
  `0` = at least one match, `1` = no match. This overload *is* the predicate, so
  an agent or script asks "does any item mention X?" with `tk grep X >/dev/null`
  and a `-q` flag is not required to get yes/no behaviour. A no-match keeps
  stderr empty so a script distinguishes "no match" from "broken" by checking
  stderr, as with `tk self-update --check`. A malformed or empty/all-whitespace
  pattern is a usage error (`2`), validated before the store is opened. (The
  no-match `1` carries no stderr diagnostic, unlike the existing `Exit::Failure`
  definition; the command owns reconciling that with the `Exit` taxonomy.)
- **Output is `tk show`-style blocks only, no `tk list` chrome:** label line +
  facet bar + a `DESCRIPTION` collapsed to the matching lines ± 3 context lines
  (matching `git diff`'s default), with `--` between non-contiguous hunks within
  one body and a blank line between item blocks. Relationship/blocker sections
  (`PARENT`/`TICKETS`/`BLOCKED BY`/`BLOCKING`/`EXTERNAL BLOCKERS`) are omitted —
  they are references, not text the pattern matched. A title-only match renders
  the label line + facet bar with no body hunk; the highlighted title is the
  match cue.

Implementation and additive follow-ups:

- **Streamed, matched in Rust, no `functions` feature.** Items are read in
  `created_seq` order and tested per line as they stream, so `tk grep PATTERN |
  head` stops early on a broken pipe and peak memory is one item. No schema or
  migration; one new direct `regex` dependency, default features trimmed the
  jiff/rand way (ADR-0011).
- **Matched text is highlighted through the `Styler`** (ADR-0014), so it is
  policy-gated: highlighted on a TTY, plain bytes when piped or under
  `NO_COLOR`, preserving the byte-stable scriptable output. The highlight span
  is still sanitised — rendering interleaves `sanitize` with the SGR open/close
  rather than emitting them sequentially. This is cosmetic and additive, not a
  frozen contract.
- **Deferred, addable without a contract change:** `-C`/`--context N`,
  `-F`/`--fixed-strings`, `--limit`, status/Origin filters, a `-c` count mode,
  and `-q`/`--quiet` — mirroring ADR-0025's no-flags-first discipline. `-q` is
  deferred specifically because the `0`/`1` exit overload already delivers the
  predicate; `-q` would add only output suppression plus a first-match
  early-exit (`break` on the first hunk), and the early-exit saves nothing at
  the **Repository Store**'s scale. It lands, with its early-exit, only if a
  real need appears.
- **FTS5 ranked full-text and fuzzy/Levenshtein recall** are spun out as future
  **`tk search`** enhancements (separate Tickets and ADRs), where ranked output
  is the feature rather than an obstacle.
