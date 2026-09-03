# Migrations back up the Repository Store before they run

ADR-0024 applies pending forward migrations when a store is opened. The first
command a newer binary runs therefore rewrites the user's only copy of their
work tracking, with no prompt and no undo.

Migration 011's review found that dropping `closing_reason` from the rebuild's
copy list passed the whole suite — 932 unit tests, 39 scenarios, zero failures
— while destroying, on upgrade, a Local Field the Repository Store is the only
copy of (ADR-0023). The `items` rebuild carries fourteen columns; five were
unpinned by any test at the time, and three survived deliberate mutation of the
copy list. A test suite cannot be relied on to catch a lossy copy, because the
rows a migration test seeds are not the rows a real store holds.

## Decision

`migrations::apply_all` writes one backup of the Repository Store before
applying any pending migration, when the store already has a schema:

```
vacuum into '<git-common-dir>/tk/backups/<timestamp>-v<version>.db'
```

`VACUUM INTO`, not a file copy: it produces a consistent image from the live
connection, and the store runs in WAL mode, where copying the file can tear a
database. It compacts, so a backup is smaller than the store it came from.

The backup directory is derived from the connection's own path rather than
passed in, so an in-memory connection writes nothing. The newest ten backups
are kept.

A backup that cannot be written aborts the migration. The store is left at its
old version and every command refuses to open it until the cause is fixed.

## The trigger is "a migration is about to run", not "a rebuild is about to run"

The obvious gate is ADR-0028's `ForeignKeys::Off` — the table rebuilds, where
the store is dropped and recopied and a mistake is unrecoverable rather than
merely wrong. It is the wrong gate, and the code says so:

- Migration 004 rewrites every Ticket row (`update items set selection_state =
  'accepted' where item_class = 'ticket'`) with foreign keys enforced.
- Migrations 013 and 014 backfill identity and provenance data, also enforced.
- Migration 007 is an `alter table add column` that runs *before* the first
  rebuild a store at version 6 reaches, so a backup gated on the rebuild is
  taken after 007 has already committed and is not the pre-upgrade state.

`ForeignKeys::Off` tracks whether a table's CHECK constraints changed, not
whether a migration can lose data. The two are not the same set, and the
ticket that asked for this feature contains the argument against its own gate:
a backup does not need to know what it is protecting.

Dropping the gate also removes the machinery it required. A per-rebuild backup
had to be counted (a version 4 store crosses six `ForeignKeys::Off` migrations,
and keeping the newest three would delete the only file holding the pre-upgrade
state), and a filename naming the migration about to run had to be corrected
when a concurrent migrator won the race. Backing up once, before the run,
removes both problems rather than solving them.

## The name is read out of the file, not predicted

The version in the filename is read back from the written backup with
`migrations::current_version` — the same function the version gate calls, over
the same `schema_migrations` table. `PRAGMA user_version` is a mirror written
in the same transaction; `schema_migrations` is authoritative.

This matters because two processes can migrate concurrently (ADR-0024). A
migrator that samples version 10, then vacuums after the winner has committed
011, holds a version 11 store. Naming the file from what was sampled would
label it 10. Naming it from the file's own contents cannot be wrong.

Two migrators that agree on the millisecond and the version produce the same
name, and the rename replaces. That is correct rather than tolerated: equal
names mean equal versions mean the same pre-migration state, so nothing is
lost, and no retention slot is spent twice on one state. A concurrent `tk add`
can make the two copies differ by a row; both remain valid pre-migration
backups.

The timestamp leads the filename so a plain lexicographic sort is
chronological, which is what pruning needs. Only `:` is illegal in a Windows
filename, so it is replaced and the milliseconds are kept.

## Considered Options

**Copy the file instead of `VACUUM INTO`.** Rejected: the store is WAL
(`commands/init.rs` sets `journal_mode`, which persists in the header), and a
copy taken while a writer is mid-transaction can tear. The three hand-made
`.bak` files in the one Repository Store that exists all pass `pragma
integrity_check`, so the hazard is real in principle and unobserved in the
copies that exist. `VACUUM INTO` removes the question rather than relying on
that luck holding.

**Pass the backup directory into `apply_all`.** Rejected. `Connection::path`
returns an empty string for an in-memory database, so deriving the directory
makes every in-memory test a no-op by construction. An explicit parameter would
have added an argument to 77 call sites — 54 of them in the migration tests
alone — whose only value at 75 of them is "off". A parameter that exists to be
disabled is a flag, and the directory is a fact about the connection.

**Add `tk restore`.** Rejected, and not on scope. Restoring a version 11 file
under a version 16 binary re-runs migrations 012 through 016 on open (ADR-0024)
— including the one that lost the data, if the binary is still the one that
lost it. A restore command would be a trap dressed as a fix. What the backup
guarantees is narrower and sufficient: the bytes still exist and are readable.
Recovery from the incident that motivated this is reading a column out of the
old file and re-applying it, not rolling the store back.

**Keep three backups.** Rejected. Three was proposed for an "undo the upgrade I
just ran" window. Discovery here is late by construction — the migration 011
loss was found by code review, not by anyone noticing missing data — and the
migration list moves fast: ten migrations landed in the month before this
decision, three of them on one day. For a developer building from source, three
backups is a window of days. The user's own hand-made backups have sat
unpruned for three and a half months. Ten is bounded, which is what the
objection to an unbounded directory inside `.git` actually asks for, and costs
roughly fourteen megabytes against a 1.4 MB store.

**Fail open when the backup cannot be written.** Rejected: migrating
unprotected is the state this decision exists to end. The surface is thin — a
backup needs about the store's size, and the directory is one tk created — so
the cost of closing it is small and the cost of leaving it open is the whole
feature.

## Consequences

**A migration can now fail for a reason that is not the migration.** A full
disk or an unwritable `backups/` refuses the upgrade, which refuses every
command, since ADR-0024 migrates on open. The first real execution of this path
is the first migration added after this decision, against a store holding real
work.

`tk prime` stays silent on that failure, as it already does for every other
open failure (ADR-0020).

**`<git-common-dir>/tk/backups/` is store-owned state.** It is created at
`0700`, matching `tk init`'s treatment of `tk/`. It is inside `.git` and
therefore untracked (ADR-0001). Pruning only considers files matching the
generated shape, so hand-made backups beside `tk.db` are never deleted.

**Recovery needs `sqlite3`, which tk does not ship.** tk is a single static
binary (ADR-0011) that embeds SQLite rather than exposing it. The manual says
so, rather than leaving it to be discovered at the moment it is needed.

**A backup is a rollback-mode file.** `VACUUM INTO` attaches its output fresh
and nothing switches it to WAL; the metadata it copies carries `user_version`
and `application_id` and no journal mode. Measured: a WAL source vacuumed into
a target yields an output reporting `delete`. Copying one over `tk.db` without
removing `tk.db-wal` and `tk.db-shm` is a hazard, and the restored store is not
in WAL until `tk init` runs again.

**The `.partial-<pid>` working file is unique per PID namespace, not
globally.** A repository reachable from two PID namespaces — bind-mounted into
a container and worked on from both sides — can produce two writers with the
same pid against one `backups/`. The stale-file removal that clears a
`SIGKILL` leftover would then let one writer unlink the other's in-flight
target. Accepted: the leftover it fixes is reachable, and the collision needs a
shared directory across namespaces.

**Nothing announces the backup yet.** The manual is the only place it is
described, so a user who does not know data is missing has no reason to read
about it. Retention is what stands in for that until an on-upgrade notice
lands; that notice is a separate decision, because a success-path line from the
store-open path is a category of output ADR-0032 removed from the command
handlers and would have to reintroduce deliberately.

**This does not replace the copy-fidelity tests.** Those catch a lossy copy
before release; this catches what they missed, after. Both stay.
