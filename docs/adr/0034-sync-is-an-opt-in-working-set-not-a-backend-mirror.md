# Sync is an opt-in working set, not a Backend mirror

tk is a local-first work tracker. A configured Remote does not mirror its
Backend into the Repository Store; instead the user **Adopts** specific Backend
issues to work them locally. **Backend Pull** refreshes only the Adopted items
that are not yet `done`, and tk never discovers or imports un-Adopted issues.
This supersedes the original tk-34 Pull design — a single
`gh issue list --state all` mirror — recorded in ADR-0021.

## Why opt-in, not mirror

The mirror model does not merely strain throughput — it swamps the local
tracker. `merge_backend_snapshots` imports every Backend issue as an `accepted`
Backend Ticket (ADR-0027), and `accepted` is exactly the state `tk next`
selects and `tk list` surfaces. A full Pull of a real Backend — a 15k-item Jira
project, a long-lived GitHub repo — therefore dumps thousands of Tickets the
user never chose straight into the selection queue, the opposite of a
"lightweight local tracker."

A fixed pull cap (the deferred `--limit 1000` + truncation warning) only bounds
*how much* swamping happens, and silently drops everything past the cap;
`--state all` makes that realistic rather than theoretical, since a mature repo
exceeds 1000 closed issues on its own.

Opt-in dissolves the problem at the root: the working set is bounded by what
the user Adopted, not by the size of the Backend. The 1000 cap, the truncation
warning, and the since-timestamp delta optimisation all become moot — there is
no list to bound.

## What changes

- **Adopt is the sole Backend → tk intake path in v1.** Promotion (tk →
  Backend, deferred) is its inverse. Both yield a Backend Ticket and both are
  explicit.
- **Backend Pull becomes refresh-by-key.** The sync engine derives the active
  Adopted key set (`status in ('open','active')`) and the Backend Adapter
  fetches exactly those (`gh issue view <n>` per key). `merge_backend_snapshots`
  is reused unchanged: Adopt drives the insert path (Scenario A), Pull drives
  the refresh paths (B/C).
- **No auto-discovery.** A Backend issue the user has not Adopted never appears
  in tk; discovery stays the Backend's own UI.

## Considered Options

- **Mirror the whole Backend every sync** (the original tk-34 / ADR-0021
  design). Rejected: swamps the selection queue and caps/truncates at scale, as
  above.
- **Mirror, but import as `triage`** so imported items stay out of `tk next`.
  Rejected: still pulls and stores the entire Backend (the scale ceiling is
  unsolved) and still floods `tk list`.
- **Opt-in working set.** Chosen.

## Consequences

- No auto-discovery is the accepted trade. For a large shared Backend (a Jira
  team project) this is a feature, not a regression; for a small personal repo,
  a future bulk `tk adopt --all-open`-style convenience can adopt many at once
  without a second sync engine.
- tk-34 is re-scoped to the opt-in adapter: a `fetch_snapshots(keys)` read
  primitive plus Apply, with no `gh issue list`, no truncation handling, and no
  since-timestamp watermark.
- A new ticket owns the `tk adopt` command; it depends on tk-34 (the adapter)
  and tk-106 (the configured Remote), mirroring how tk-106 was carved out ahead
  of tk-34.
- A permanently-deleted Adopted issue makes its per-key fetch fail; v1 Backend
  Pull is all-or-nothing, so the escape is `tk done` (a `done` item leaves the
  refresh set) plus `tk sync --skip` for the resulting close Mutation. A
  dedicated un-adopt is a likely follow-up.
- Amends ADR-0021: its relationship-deferral decision and its issueType →
  TicketKind mapping stand, but its Pull mechanism (`gh issue list --state all`
  → full snapshot) is replaced by fetch-by-key refresh.
- Amends ADR-0010: the merge "insert a new backend Item" path (Scenario A) now
  fires from Adopt, not from Backend Pull.
