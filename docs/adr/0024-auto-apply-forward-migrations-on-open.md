# Auto-apply forward migrations on open; refuse future-version stores

The Repository Store opens through a single production constructor,
`open_existing` (every command funnels through `resolver::open_for_command`).
That chokepoint now makes **"schema is current" an invariant of holding a
`Store`**: the version gate is symmetric. A store recorded *newer* than this
binary is refused fail-closed (`FromFutureVersion`); a store recorded *behind*
`MAX_KNOWN_VERSION` has its pending forward migrations applied in place before
the handle is returned. No command performs migration; migration is a property
of having opened the store.

Migrations previously ran only in `tk init`. A newer binary opening an older
on-disk store therefore fell through the gate and surfaced a cryptic `no such
column` at the first write — exactly what shipped with migration 003
(`closing_reason`, tk-61): `tk done -m` failed until the user manually re-ran
`tk init`. tk-110 removes that footgun for 003 and every future migration.

This matches how local-first / embedded-database tools treat state they own.
The Repository Store is single-owner, app-exclusive, untracked local state
(ADR-0001), so the binary is entitled to upgrade its own format on open. The
fail-closed refusal of a future-version store follows Git's
repository-format precedent: an implementation that does not understand the
on-disk version must refuse to operate rather than guess.

## Considered Options

- **Keep migrating only in `tk init`.** Rejected: this *is* the bug. It leaves
  every existing store stranded at the old schema until a manual re-init, and
  the failure is a raw SQLite `no such column`, not a diagnosable tk error.
- **Add an explicit `tk migrate` command (the ORM / Git "explicit camp").**
  Diesel, sqlx, Django, and EF Core migrate on an explicit command and fail
  closed on unknown versions. That camp serves multi-instance server apps
  pointed at a shared, separately-administered database, where a surprise
  schema write under load is the hazard. tk's store has none of those
  properties — single owner, opened by one short-lived process at a time — so
  the hazard the explicit camp guards against does not apply (the "startup
  migration is inherently unsafe" claim was investigated and refuted against
  primary sources). `open` plus `init` already cover create and upgrade; a
  standalone command would be ceremony with no store this tool can't reach.
- **Lazy / on-demand migration (apply a column when a query first needs it).**
  Rejected: scatters schema knowledge across the read/write paths and defeats
  the single-chokepoint invariant. The version gate is one place; keep it one
  place.

## Consequences

- **Reads can write.** Opening a behind-version store with `tk list` or
  `tk next` performs an `ALTER TABLE`. Accepted as the cost of the invariant.
  The `version == MAX_KNOWN_VERSION` fast path takes no write lock, so the
  common case is unaffected.
- **`tk prime` migrates like any other open.** A passive prime hook against a
  behind-version store silently upgrades it and still prints the briefing,
  consistent with "tk owns its format"; ADR-0020 keeps prime silent on
  failure. Whether prime specifically should get a no-migrate path is tracked
  by tk-112, not reopened here.
- **Concurrency.** Two processes can both read the behind version and race to
  `ALTER TABLE`, which would otherwise throw `duplicate column`. `apply_one`
  opens its transaction with `BEGIN IMMEDIATE` (taking the write lock up front,
  so a second migrator waits on the existing 5s `busy_timeout`) and re-reads
  the recorded version *inside* the lock, skipping a migration the winner
  already applied. This closes the read-then-write TOCTOU and also hardens
  `tk init`.
- **Forward-only.** Down-migrations and rollback remain out of scope; the
  migration list only grows.
- **Error surface.** An open-time migration failure maps to a distinct
  `OpenError::MigrationFailed` rendered as `failed to apply pending migrations
  to the Repository Store` followed by the underlying SQLite cause, rather than
  folding into the generic storage arm.
- **Migrations run with `foreign_keys = on`.** Fine for every current
  migration (`ADD COLUMN`, `CREATE TRIGGER`). A future table-rebuild migration
  (the SQLite 12-step `ALTER`) needs foreign keys *off*, which cannot be
  toggled inside a transaction — that migration will have to arrange its own
  pragma handling outside the per-migration transaction.
