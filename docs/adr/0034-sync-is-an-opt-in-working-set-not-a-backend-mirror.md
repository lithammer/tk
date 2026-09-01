# Sync is an opt-in working set, not a Backend mirror

> **Amended by ADR-0043.** The working set below still contains every Adopted
> Item not yet `done`, but its stored predicate is now `status = 'open'`.
> `status` stores Lifecycle, and Work State does not affect Pull eligibility.
>
> **Amended by ADR-0046.** `tk done` plus Sync Skip is no longer the escape for
> a deleted Backend object: skipping a failed close restores local `open`.
> Explicit Detach removes the Backend Binding while retaining a Local Item.

tk is a local-first work tracker. A configured Remote does not mirror its
Backend into the Repository Store; instead the user **Adopts** specific Backend
issues to work them locally. **Backend Pull** refreshes only the Adopted items
that are not yet `done`, and tk never discovers or imports un-Adopted issues.
This supersedes the original tk-34 Pull design — a single
`gh issue list --state all` mirror — recorded in ADR-0021.

## Why opt-in, not mirror

The mirror model fills the local tracker with unchosen work.
`merge_backend_snapshots` imports every Backend issue as an `accepted`
Backend Ticket (ADR-0027), and `accepted` is exactly the state `tk next`
selects and `tk list` surfaces. A full Pull of a large Jira project or a
long-lived GitHub repository can therefore add thousands of Tickets the
user never chose straight into the selection queue, the opposite of a
"lightweight local tracker."

A fixed pull cap (the deferred `--limit 1000` + truncation warning) only bounds
how much unchosen work is imported and silently drops everything past the cap;
`--state all` makes that likely for a mature repository, which may have more
than 1,000 closed issues.

Opt-in removes this problem: the working set is bounded by what
the user Adopted, not by the size of the Backend. The 1,000-item cap, the truncation
warning, and the since-timestamp delta optimisation all become moot — there is
no list to bound.

## What changes

- **Adopt is the sole Backend → tk intake path in v1.** Promotion (tk →
  Backend, deferred) is its inverse. Both yield a Backend Ticket and both are
  explicit.
- **Backend Pull refreshes an exact working set.** The sync engine derives the
  active Adopted key set (`status in ('open','active')`) and gives the complete
  set to the Backend Adapter. The Adapter may batch those exact-key reads, but
  cannot list or discover other Backend items. Adopt receives canonical intake
  data and inserts a Backend Ticket; Pull receives field-only refreshes and
  cannot insert or alter Backend identity, Display ID, Origin, or Item Class.
  Pull collects every refresh before its one merge transaction.
- **No auto-discovery.** A Backend issue the user has not Adopted never appears
  in tk; discovery stays the Backend's own UI.

## Considered Options

- **Mirror the whole Backend every sync** (the original tk-34 / ADR-0021
  design). Rejected: fills the selection queue and requires caps or truncation
  for large Backends.
- **Mirror, but import as `triage`** so imported items stay out of `tk next`.
  Rejected: still pulls and stores the entire Backend (the scale ceiling is
  unsolved) and still floods `tk list`.
- **Opt-in working set.** Chosen.

## Consequences

- No auto-discovery is the accepted trade. For a large shared Backend (a Jira
  team project) this is a feature, not a regression; for a small personal repo,
  a future bulk `tk adopt --all-open`-style convenience can adopt many at once
  without a second sync engine.
- tk-34 is re-scoped to the opt-in Adapter: canonical `adopt_ticket(input)`, a
  set-oriented Pull read, and Apply, with no `gh issue list`, no truncation
  handling, and no since-timestamp watermark. The GitHub Adapter batches Pull
  through exact-key GraphQL reads instead of starting one `gh issue view`
  subprocess per item. It executes bounded batches sequentially; concurrency
  is deferred until measurements justify its rate-limit and error-order policy.
  One private maximum key count bounds each batch after canonical GitHub
  identifiers are validated. Its value follows the query shape and GitHub's
  documented limits; live probes verify compatibility rather than define the
  bound. It is not a public tk setting.
- Pull diagnostics remain deterministic after batching. Item-specific GraphQL
  errors are mapped back to Backend keys, and the earliest failed key in the
  original input order wins; GitHub's error-array order is not a contract.
  Whole-request transport failures are reported when their sequential batch is
  reached.
- Pull performance is a request-shape contract, not a wall-time promise. Tests
  prove that an empty working set makes no Backend call and that non-empty work
  is grouped into bounded Backend batches; reproducible measurements record the
  observed improvement without treating GitHub or network latency as tk
  behavior.
- A new ticket owns the `tk adopt` command; it depends on tk-34 (the adapter)
  and tk-106 (the configured Remote), mirroring how tk-106 was carved out ahead
  of tk-34.
- A permanently deleted or inaccessible Backend object makes its per-key fetch
  fail. Pull remains all-or-nothing; explicit Detach removes that key from the
  working set without asking tk to infer deletion from the read failure.
- Amends ADR-0021: its relationship-deferral decision and its issueType →
  TicketKind mapping stand, but its Pull mechanism (`gh issue list --state all`
  → full snapshot) is replaced by fetch-by-key refresh.
- Amends ADR-0010: the merge "insert a new backend Item" path (Scenario A) now
  fires from Adopt, not from Backend Pull.
