# Auto-apply forward migrations on open; refuse future-version stores

When an ordinary command or `tk init` opens an existing Repository Store,
tk checks the Store version first. It refuses a Store from a newer binary
(`FromFutureVersion`) and upgrades an older Store before the command uses it.
An upgrade needs the exclusive association lock: an older process that has
already opened the Store could otherwise read data in the new format. If
the Store is in use, let the other tk commands finish, then retry.
Current-version opens take the association lock in shared mode.

Migrations previously ran only in `tk init`. A newer binary opening an older
on-disk store therefore fell through the gate and surfaced a cryptic `no such
column` at the first write — exactly what shipped with migration 003
(`closing_reason`, tk-61): `tk done -m` failed until the user manually re-ran
`tk init`. tk-110 fixes that failure mode for 003 and every future migration.

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
- **Add an explicit `tk migrate` command.** Diesel, sqlx, Django, and EF Core
  migrate on an explicit command and fail closed on unknown versions. Those
  tools serve multi-instance server apps pointed at a shared, separately
  administered database, where a schema write under load is the hazard.
  tk's Store has one owner, and the Store lock can exclude other tk processes
  for the upgrade. `open` plus `init` already cover create and upgrade.
- **Lazy / on-demand migration (apply a column when a query first needs it).**
  Rejected: scatters schema knowledge across the read/write paths and defeats
  the single-entry-point invariant. The version gate is one place; keep it one
  place.

## Consequences

- **Reads can write.** Even `tk list` or `tk next` can upgrade an older Store.
- **`tk prime` migrates like any other open.** A passive prime hook against a
  behind-version store silently upgrades it and still prints the briefing,
  consistent with "tk owns its format"; ADR-0020 keeps prime silent on
  failure. Whether prime specifically should get a no-migrate path is tracked
  by tk-112, not reopened here.
- **Concurrency.** tk checks the version again under the exclusive lock.
  After upgrading it reopens under a shared lock, so the command that
  triggered the upgrade doesn't block other current-version commands.
- **Forward-only.** Down-migrations and rollback remain out of scope; the
  migration list only grows. Because an upgrade on open is unprompted and
  cannot be undone, ADR-0048 has the runner write a Store Backup before
  applying anything to a store that already has a schema.
- **Error surface.** An open-time migration failure maps to a distinct
  `OpenError::MigrationFailed` rendered as `failed to apply pending migrations
  to the Repository Store` followed by the underlying SQLite cause, rather than
  folding into the generic storage arm.
- **Table rebuilds.** [ADR-0028](./0028-schema-check-changes-use-a-foreign-keys-off-table-rebuild.md)
  describes when a migration turns off foreign key checks.
