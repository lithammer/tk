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
then does tk add the Git pointer. It checks the pointer again before and after
the write; it never replaces an existing value. A failure retains the new
Store, including incomplete metadata. A later init refuses rather than
allocating another Store over evidence of the interrupted creation.

## Evidence and the recovery boundary

Fresh init stores sorted, deduplicated Git URLs without changing their spelling.
It retains only `http`, `https`, `ssh`, or `git` transport URLs without userinfo,
queries, fragments, or control characters. It omits local paths, SCP syntax,
and remote-helper syntax. This deliberately loses some useful evidence to keep
credentials out of the manifest. Healthy init and ordinary opens never refresh
that evidence. Historical paths start empty.

Until the recovery slices land, a missing pointer permits fresh creation only
when all existing entries are valid, unrelated Store manifests. A current or
historical path match, a retained URL match, or an unreadable or incomplete
entry refuses creation. Even an unrelated broken entry therefore needs manual
inspection first. A same-Remote clone can require recovery or an explicit new
Store choice, neither of which this slice supplies.

Any legacy `<git-common-dir>/tk` entry is preserved and refused, including on
ordinary opens. There is no legacy fallback. Candidate discovery and explicit
reattachment belong to tk-234; Vacant Store repair belongs to tk-235; legacy
migration belongs to tk-236. Corrupt metadata needs manual restoration. This
slice does not expose `--attach`, `--new`, or a force option.

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
