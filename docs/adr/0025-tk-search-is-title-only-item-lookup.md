# tk search is title-only item lookup; ID is tk show, content search is a deferred tk grep

`tk search <query>` (tk-79) finds **Tickets** and **Epics** whose title
contains the query as a case-insensitive literal substring, across the whole
**Repository Store** and every **Item Status**, rendering matches as flat
**`tk list`** rows. It deliberately does **not** match **Display IDs** or
**Aliases** — exact identifier lookup is **`tk show`** — and does **not**
search body text. Body and content search are deferred to a separate future
**`tk grep`** command.

Two read commands earn two verbs only because they return different things.
**`tk search`** answers *"which item is it?"* and returns items (list rows).
**`tk grep`** answers *"where does this text appear?"* and returns matches in
context — a **`tk show`** block with the body collapsed to the matching lines
plus `grep -C`-style surrounding context. If the two differed only by which
fields they scan, the names would be colliding synonyms: someone hunting body
text through **`tk search`** would silently get "no matches" and wrongly
conclude the item does not exist. The distinct output is what justifies the
second verb.

## Considered Options

- **One `tk search` with a `--body` flag widening the match to body text.**
  Rejected: a body-only hit renders as a reused **`tk list`** row whose visible
  title does not contain the query, so it reads as a false positive — the row
  has no slot for match provenance. Showing *where* the text matched needs
  `grep -C`-style snippets, which is exactly **`tk grep`**'s output, not a flag
  on a list-row renderer.
- **Match Display IDs and Aliases (exact + prefix), the original tk-79
  framing.** Rejected: exact-identifier lookup duplicates **`tk show`**, and the
  only non-redundant case — partial/prefix recall — is thin against short
  sequential **Display IDs**. Search's distinct value is fuzzy *title* recall,
  the thing **`tk show`** cannot do. ID matching is re-introducible additively
  if prefix recall ever proves real.
- **Honour `TK_SCOPE` so an ambient Epic narrows search.** Rejected: a lookup
  must not be silently narrowed. Confining search to a scoped Epic would hide
  the very item the user searched for whenever it lives elsewhere — the
  invisible-ambient-state smell ADR-0022 rejected when it removed inferred
  Scope. You search precisely because you do not know where the item is.

## Consequences

- **`tk search`** is whole-store, all-statuses, and title-only, which makes it
  the first sanctioned lookup for `done` work. CONTEXT.md's done-browsing
  deferral is updated: general done-browsing through **`tk next`** /
  **`tk list`** stays deferred, but finding a specific completed item by title
  is supported.
- The first slice ships with **no flags** beyond the required positional
  `<query>`. `--limit`, **Local** / **Backend** **Origin**, **Ticket Kind**,
  **Priority**, status filtering, and sorting are deferred and added when a need
  appears; `--` escapes a leading-dash query.
- `render_row` and the **`tk list`** chrome (separator, `Total` line,
  status/blocked legend) extract to a shared render helper. **`tk list`** output
  stays byte-identical — guarded by its existing scenario snapshots — and
  **`tk search`** renders the same rows flat, without **List Tree** nesting.
- **`tk search`** is the first caller to feed `done` rows to that renderer, an
  input **`tk list`** never produces (every list view is open/active-only). A
  `done` item can still carry an unresolved blocker — closing an item resolves
  nothing — so the shared renderer must not show a finished item dimmed with
  `⊘`: a `done` row never renders the blocked treatment. This is byte-safe for
  **`tk list`**, which never renders `done`.
- A follow-up Ticket owns **`tk grep`**: content search over title and body,
  rendered as **`tk show`** with `grep -C`-style body context, carrying its own
  pattern/regex semantics and exit-code contract. Until it lands, body/content
  search is intentionally absent, not an oversight.
