# Repository Stores live in durable user data

A Repository Store belongs to the user and survives deletion of its Workspaces.
Fresh Stores live under the platform-local data directory, keyed by an opaque
Store ID. Git records an association with the Store; it does not own the data.
This replaces the Git-directory location in ADR-0001 and the Store and lock
paths in ADRs 0009, 0033, 0036, 0041, and 0048.

## Location and ownership

`dirs::data_local_dir()` supplies the data root: XDG data home or local share
on Linux, Application Support on macOS, and LocalAppData on Windows. tk requires
an absolute result. It has no fallback location or tk-specific environment
variable. The process shim resolves the root once and passes it through `Deps`.

```text
<local data>/tk/
  init.lock
  stores/
    .migrations/                  # unpublished legacy migration images
    <32 lowercase hexadecimal characters>/
      store.json
      tk.db
      backups/
      association.lock            # held while a Store is open
      remote.lock                 # created when a Remote workflow needs it
```

A typed Store ID holds 128 random bits from the existing opaque-ID generator.
Creation reserves its directory with an exclusive create; a collision refuses
without opening or overwriting its contents. SQLite still owns Ticket/Epic
state, the Mutation Log, and schema versions (ADR-0005). The manifest owns
Store identity and association. New directories, including missing ancestors,
receive `0700` on Unix. Existing directory permissions stay as the user set them.

Manifest version 1 has this shape:

```json
{
  "version": 1,
  "store_id": "0123456789abcdef0123456789abcdef",
  "association": { "git_common_dir": "/home/user/src/project/.git" },
  "evidence": {
    "previous_git_common_dirs": [],
    "git_remote_urls": ["https://github.com/owner/project.git"]
  }
}
```

The associated path is filesystem-canonical. A Store has at most one live
repository association; linked Workspaces share its Git Common Directory.
An independent repository with a copied pointer fails association validation.
Repository paths, URLs, and history do not define Store identity.

## Opening and publication

Ordinary commands read all `tk.storeId` values with
`git config --local --no-includes --null --get-all`. Only one valid ID is
accepted. Global, command, included, and worktree-specific values have no
pointer authority. The Store directory, manifest version, manifest ID, and
canonical association must validate before SQLite opens. A Store directory
symlink is refused. Ordinary access never scans for a substitute Store.

Git 2.53.0 source establishes the config behavior:
[`builtin/config.c`](https://github.com/git/git/blob/v2.53.0/builtin/config.c)
selects the repository config for `--local` and applies the includes option;
[`config.c`](https://github.com/git/git/blob/v2.53.0/config.c) writes config
through a lock file. The real-Git scenarios exercise those paths, including
linked Workspaces and duplicate values.

Healthy init validates and opens the Store without rewriting Git config or
manifest evidence. Foreign and future databases are refused before file
pragmas change; older schemas still migrate with the backup contract below.

Fresh init holds an OS lock on `init.lock` across the evidence check, creation,
and pointer installation. The lock serializes tk initializers sharing a data
root, releases on process exit, and leaves a stable file. Contention asks the
caller to retry. Fresh init needs neither a Remote nor commit history.

Creation writes and closes the database, creates `backups/`, then writes and
syncs the manifest. Unix also syncs the Store directory and its parent. Only
then does plain init add the Git pointer. It checks the pointer again before
and after the write; explicit `--new` can replace an existing value. A failure retains the new
Store, including incomplete metadata. A later plain init refuses rather than
allocating another Store over evidence of the interrupted creation.

## Evidence and the recovery boundary

Fresh init stores sorted, deduplicated Git URLs without changing their spelling.
It retains only `http`, `https`, `ssh`, or `git` transport URLs without userinfo,
queries, fragments, or control characters. It omits local paths, SCP syntax,
and remote-helper syntax. This deliberately loses some useful evidence to keep
credentials out of the manifest. Healthy init and ordinary opens never refresh
that evidence. Historical paths start empty.

Plain `tk init` scans manifests only when the current association is not healthy. It ranks candidates by referenced Store ID, associated current canonical path, historical canonical path, then exact safe Git remote URL overlap. Ties sort by Store ID; tk shows every matching fact. It lists invalid entries for manual restoration and inspects only shortlisted databases, read-only, without migrations. Ordinary opening reads only the selected manifest.

`tk init --attach <Store ID>` requires a valid manifest whose Store ID matches its directory name, and a readable tk database with a supported schema and passing integrity checks. It preserves Tickets, Local Fields, Mutations, the Plan, and Store Backups.

`tk init --new` creates a distinct Store and preserves prior Stores, reporting a missing referenced Store as possible data loss. It never recreates a referenced ID.

Both options refuse a healthy current association and cannot be combined. There is no force option. Plain init creates only with no pointer, legacy data, or plausible candidate.
It can also repair one uniquely identified Vacant Store: exactly one candidate
must match a referenced Store ID or the current canonical Git Common Directory,
and the local pointer must be absent or a single valid ID. Historical paths and
Remote URLs alone never authorize repair. Unrelated Vacant Stores do not block
fresh initialization or supply identity.

Automatic repair uses the same manifest validation, ownership release checks,
and publication path as explicit attachment. Under the exclusive Store lock,
it checks the database and every file in `backups/` read-only, without migration.
No table may hold user data, including Mutations in any state or Plan
membership. Zeroed initialization sequences and the seeded Display ID
prefix do not count as user data. The prefix can come from any linked
Workspace's name; it cannot be inferred from the Git Common Directory.
Other configuration records and nonzero sequence values require explicit recovery.

Unreadable, corrupt, unsupported, or incompletely inspected images never count
as vacant. tk also refuses repair when expected tables are missing or schema
version records disagree. It inspects older Store Backups without upgrading them.
If vacancy inspection fails or finds user data, tk returns the ranked recovery
report with explicit commands. Vacancy neither changes candidate rank nor overrides live
or unknown ownership.

tk permits attachment when the manifest names the current Git Common Directory, or when the former directory is readable and its local config no longer points to this Store. tk runs `git --git-dir . config --local --no-includes --null --get-all tk.storeId` from that exact directory so parent repository discovery cannot stand in for ownership inspection. Passing `.` also avoids Windows verbatim paths: Git for Windows 2.55.0.windows.5 rejects their `?` in [`is_valid_win32_path`](https://github.com/git-for-windows/git/blob/v2.55.0.windows.5/compat/mingw.c). Failed Git invocations, unreadable directories, and directory symlinks leave ownership unknown.

An absent former Git Common Directory permits attachment only when its immediate parent is readable and a directory listing confirms that the final component is absent. A missing ancestor remains unknown: it could be an unavailable volume. After moving a whole checkout, restore access to the former Git Common Directory's parent before attachment. tk does not infer release from an absent ancestor.

Path inspection cannot distinguish deletion from a volume silently unmounted beneath a still-readable parent. Restore unavailable volumes before recovery.

Plain init migrates legacy `<git-common-dir>/tk` Stores using the protocol below. Attachment and explicit new creation preserve and refuse legacy data and pending migrations. Invalid, missing, mismatched, unsupported, or unreadable manifests need manual restoration; pending files never replace that requirement.

## Lifecycle synchronization and interrupted attachment

All initialization and attachment commands hold the stable data-root `init.lock` exclusively through their checks and pointer writes.

Each Store has a stable `association.lock`: ordinary opening takes a shared OS lock before validating the manifest, rechecks the local pointer, and retains the lock in `Store` until its SQLite connection closes. Attachment holds that Store lock exclusively before inspecting ownership or changing metadata. Contention returns exit 1 with retry guidance. Independent Stores can remain open concurrently; initializers sharing a data root serialize. Remote workflows hold their lock while the Store remains open.

This protocol prevents two supported initializers from attaching the same Store and prevents an old opener from writing after attachment changes ownership. Locks release on process exit. tk never deletes or replaces the lock files of an authoritative Store. A pre-pointer migration image is private staging; retry may discard that image and its unused association lock. External config edits, older binaries without these locks, and moving a repository during an active command are outside this synchronization protocol.

Attachment writes a uniquely named pending manifest beside `store.json`, syncs and closes it, then atomically renames it over `store.json` and syncs the directory on Unix. Only then does it replace all repository-local pointer values through Git's config lock. The manifest retains the former canonical path and merges sorted, deduplicated safe remote observations. Healthy operations never refresh evidence.

A failure before the rename leaves the prior valid manifest authoritative. If directory sync fails after the rename, the new manifest is already in place. A failed pointer write retains the newly published manifest and all database contents; explicit `tk init --attach <Store ID>` can retry at the new path. If Git committed the pointer before its outcome was lost, plain `tk init` confirms the healthy association. Pending files can remain after interruption, but are never used to reconstruct missing or corrupt metadata.

## Existing contracts

Store Backups remain beside `tk.db`, under `backups/`, with the same naming,
pre-migration gate, and ten-copy retention (ADR-0048). The stable Remote workflow
lock moves with the Store. Its ownership and scope do not change.

ADR-0017's two init success prefixes stay verbatim; their path now names the
durable database. Init uses the shared Store errors for foreign/future SQLite
files and failed migrations. Association, data-root, collision, legacy, and
recovery refusals add literal message bodies. Other command messages stay
unchanged. Prime still succeeds silently on any Store-open failure (ADR-0020).

The scenario and concurrency tests run `cli::run_argv` in a child test process
with real Git and an injected data root. This amends ADR-0031's built-binary
requirement for those tests: Windows resolves LocalAppData through a known-folder
API, so an environment override cannot isolate it. Test-only root and RNG inputs
exist in the test harness, never in the shipped binary. Release smoke tests
continue to exercise the production process shim on native CI runners.

## Legacy migration protocol

Stop all tk processes before running `tk init` on a legacy Store, and keep old
binaries stopped through retries and cleanup. Migration moves
`<git-common-dir>/tk` into `<local data>/tk/stores/<Store ID>`. The legacy path
is migration input only. Ordinary commands direct users to init; Prime stays
silent. Attach and new refuse legacy data and pending migrations.

Init holds `init.lock` and a stable `tk-migration.lock` in the Git Common
Directory, and takes any existing legacy `remote.lock`. It then retains SQLite
exclusive locking mode on the legacy database. It switches WAL to DELETE and commits an exclusive transaction
before taking the image. Switching out of WAL checkpoints live WAL data and
requires an exclusive database lock. Contention refuses migration. The lock
stays held through pointer installation and source verification. SQLite 3.46.0,
bundled by libsqlite3-sys 0.30.1, defines this in `sqlite3PagerCloseWal` and
`pager_end_transaction` in its amalgamation.

This lock excludes writes while held; it does not prove that every connection
has closed. An idle rollback-mode connection can survive it. Windows also
requires closing SQLite before deleting its database: SQLite's Windows VFS
opens database handles without delete sharing. Keeping old processes stopped
prevents writes between closing SQLite and deleting the source. A rename alone
cannot stop an old process from writing.

On Unix, closing any descriptor for a database inode releases that process's
POSIX locks. The fingerprint reader therefore holds its descriptor until the
SQLite connection closes. Source databases with hard links are refused: reading
and closing a backup alias would otherwise release the same lock. SQLite's
`os_unix.c` describes this constraint; the competing-writer CLI scenario checks
that the source remains locked after hashing and publication.

The private migration source module owns the path and handles together.
`LegacySource` holds the Remote lock during inspection and progress publication;
freezing it transfers ownership into `Frozen`, which owns the SQLite connection,
fingerprint descriptor, and Remote lock. Inventory and snapshot reads go through
that owner. Its fields close SQLite before the fingerprint descriptor, then
release the Remote lock.

Cleanup reacquires those locks and checks the surviving inventory against the
receipt. It represents an already removed database separately and accepts only
an empty source directory in that state. It closes the source handles before
deleting verified files, with the database last.

The inventory reads `remote.lock` through the handle that owns its exclusive
lock. Windows [denies access through a second handle](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex),
even in the same process.

A versioned `tk-migration.json` in the Git Common Directory records a random
Store ID, a separate random token, the canonical Git Common Directory and the
directory containing Stores (`<local data>/tk/stores`). Init publishes the
record through a flushed pending file before staging starts and keeps it
through legacy cleanup. A matching receipt beside the destination manifest
binds that attempt to its source and records SHA-256 fingerprints of the
legacy files. Neither a matching path nor an ordinary manifest authorizes
resuming a migration. The stable `store.json` remains version 1 and owns only
identity and association.

Before pointer installation, legacy remains authoritative. Init builds a fresh
`VACUUM INTO` image under `stores/.migrations/<Store ID>`, on the destination
filesystem, and copies every Store Backup. It validates the images and manifest,
and flushes files before publishing the
complete directory. Retry uses the recorded Store ID and rebuilds its own
staged or published image from the locked source; it never reuses an older
snapshot. Unrelated or unverifiable destinations are preserved and refused.

After publication, init installs and verifies the Git pointer, flushes Git's
local config, and checks both sides of the association. The destination is now
authoritative. Init verifies surviving legacy files against the receipt before
cleanup; changed or unexpected files retain both copies for manual recovery.
Partial cleanup permits missing recorded files. It removes only verified files,
the database last, and removes the source progress record after cleanup.
Post-pointer retries never copy legacy data over the destination.

File contents are flushed on all platforms; directories and their parents are
also flushed on Unix, including newly created data-root ancestors.
Process-interruption recovery applies on all supported
platforms. These operations do not establish Windows sudden-power-loss safety.
A failed flush refuses source cleanup. Store Backup names and retention stay
unchanged; relocation copies all backups before later schema upgrades run.

The progress record has this shape (paths below are examples):

```json
{
  "version": 1,
  "store_id": "0123456789abcdef0123456789abcdef",
  "token": "fedcba9876543210fedcba9876543210",
  "common": "/home/user/src/project/.git",
  "root": "/home/user/.local/share/tk/stores"
}
```

The receipt is `migration.json` beside `store.json`. It has `progress` (the
record above) and `files` (relative legacy filenames mapped to SHA-256 hex
strings). Init removes the receipt and progress record after source cleanup;
neither changes the stable manifest. A leftover empty `.migrations` directory
has no Store identity and is excluded from candidate discovery.

Migration success prints `Migrated Repository Store <Store ID> from <legacy
directory> to <durable directory>`. Legacy open refusal says `legacy Repository
Store data exists; stop all tk processes, then run 'tk init'; data was preserved`.
Source divergence after cutover says `legacy files changed after cutover;
preserve both Stores and restore manually`. Retry does not overwrite either
Store in that state.

The CLI scenarios cover process exits from progress publication through final
receipt removal, partial cleanup, stale pre-pointer images, changed post-pointer
sources, every Mutation state, full row preservation, backup bytes, linked
Workspaces, active WAL, old connections, competing operations, hard links, and
injected storage faults. The legacy-connection fixture uses bundled SQLite
without tk lifecycle locks. CI runs the scenario suite on Linux, macOS, and
Windows. The composed lifecycle also covers linked Workspace edits, a moved
checkout, interrupted reattachment, and preserved Plan and backup contents.
A scenario holds Git's config lock while running ordinary commands against a
healthy Store, then checks that config and manifest bytes and modification
times stay unchanged.

A Unix scenario tests native root resolution with isolated `HOME` and
`XDG_DATA_HOME` settings.
Windows scenarios inject an isolated data root: they do not redirect the user's
known-folder configuration or prove native LocalAppData resolution. The alias
reopening scenario uses Unix symlinks; Windows alias reopening remains untested.
Native macOS and Windows results require their CI runners; a Linux run alone
does not verify those platforms.
