# Keep user-visible message constants verbatim, not generated

> **Amended by ADR-0032.** The grep-able unit narrowed from the full line to the
> message *body*: the `tk <command>:` frame is now synthesized once at the
> dispatch seam, and the verbatim literal lives in each typed error's
> `#[error("…")]` (the `messages` module described below no longer exists). The
> principle — every stable user-visible line findable by exact-string search —
> survives at body granularity.


The per-command message families in the `messages` module —
`<cmd>_missing_store`, `<cmd>_out_of_memory`, `<cmd>_store_busy_retry`,
`<cmd>_id_not_found_prefix`, and the byte-identical
`<cmd>_id_not_found_suffix` — are mechanical instances of a
`"tk <command>: …"` template, but they are kept as flat, verbatim
string constants rather than synthesized at compile time by a
`f(command_name)` builder. Every stable user-visible line must be
findable by an exact-string search: a generated line (e.g. `tk start:
Repository Store is busy; retry the command`) would exist nowhere in
the source, so a user pasting it from their terminal, a maintainer
jumping from a failed snapshot's expected bytes, or `grep 'tk start:'`
would all come up empty. Navigability is a core goal for an
agent-first tool, and the `messages` module is the single source of
truth for stable strings; the per-command repetition is the price of
that and is not a cleanup target.
