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
Every user-data table must be empty, including all Mutation states and Plan
membership. Zeroed initialization sequences and the seeded Display ID
prefix do not count as user data. The prefix can come from any linked
Workspace's name; it cannot be inferred from the Git Common Directory.
Other configuration records and advanced sequences require explicit recovery.

Unreadable, corrupt, unsupported, or incompletely inspected images never count
as vacant. Missing expected tables or inconsistent schema-version records also
refuse repair. Older backup schemas are inspected as stored. Any refusal leaves
the ranked recovery report and explicit commands available. Vacancy neither
changes candidate rank nor overrides live or unknown ownership.

tk permits attachment when the manifest names the current Git Common Directory, or when the former directory is readable and its local config no longer points to this Store. tk invokes `git --git-dir <former path> config --local --no-includes --null --get-all tk.storeId` so parent repository discovery cannot stand in for ownership inspection. Git 2.53.0's [`builtin/config.c`](https://github.com/git/git/blob/v2.53.0/builtin/config.c) selects repository config and refuses `--local` outside a repository. Failed Git invocations, unreadable directories, and directory symlinks leave ownership unknown.

An absent former Git Common Directory permits attachment only when its immediate parent is readable and a directory listing confirms that the final component is absent. A missing ancestor remains unknown: it could be an unavailable volume. After moving a whole checkout, restore access to the former Git Common Directory's parent before attachment. tk does not infer release from an absent ancestor.

Path inspection cannot distinguish deletion from a volume silently unmounted beneath a still-readable parent. Restore unavailable volumes before recovery.

Any legacy `<git-common-dir>/tk` entry is preserved and refused, including during attachment and explicit new creation. Legacy migration belongs to tk-236. Invalid, missing, mismatched, unsupported, or unreadable manifests need manual restoration; pending files never replace that requirement.

## Lifecycle synchronization and interrupted attachment

All initialization and attachment commands hold the stable data-root `init.lock` exclusively through their checks and pointer writes.

Each Store has a stable `association.lock`: ordinary opening takes a shared OS lock before validating the manifest, rechecks the local pointer, and retains the lock in `Store` until its SQLite connection closes. Attachment holds that Store lock exclusively before inspecting ownership or changing metadata. Contention returns exit 1 with retry guidance. Independent Stores can remain open concurrently; initializers sharing a data root serialize. Remote workflows hold their lock while the Store remains open.

This protocol prevents two supported initializers from attaching the same Store and prevents an old opener from writing after attachment changes ownership. Locks release on process exit. tk never deletes or replaces lock files. External config edits, older binaries without these locks, and moving a repository during an active command are outside this synchronization protocol.

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
