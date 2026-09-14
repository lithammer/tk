# tk

tk (pronounced "ticket") is an agent-first command-line tool for managing work
items through a simple local interface and pluggable issue-tracker backends.

The goal is to make work visible to humans and agents from the command line.
tk aims for a simple architecture, local-first capture, and backend adapters
for systems like GitHub Issues and Jira.

Prebuilt releases are published for Linux, Apple Silicon macOS, and Windows.

## Install

### Linux and Apple Silicon macOS

```sh
curl -fsSL https://github.com/lithammer/tk/releases/latest/download/install.sh | sh
```

### Windows

```powershell
irm https://github.com/lithammer/tk/releases/latest/download/install.ps1 | iex
```

This installs to `%LOCALAPPDATA%\tk\bin` and adds it to your User `PATH`;
restart your terminal afterwards. `tk self-update` keeps it current.

### Upgrade

Use `tk self-update`. Re-running the install script is also supported. Use the
variables below for version pinning or ABI switching.

### Environment variables

| Variable | Default | Effect |
| --- | --- | --- |
| `TK_INSTALL_DIR` | `~/.local/bin` on Linux and macOS; `%LOCALAPPDATA%\tk\bin` on Windows | Install directory. |
| `TK_VERSION` | latest release | Release version to install. |
| `TK_LINUX_ABI` | `musl` | Linux ABI variant: `musl`, or `gnu` on x86_64 Linux. |

### Build from source <a id="build-from-source"></a>

Run `cargo build --release`; the binary is written to `target/release/tk`.

## Quick start

```sh
tk init
tk add -m "Update README"
tk add --bug -F bug-report.md
tk add --epic -m "Jira backend"
tk add --parent tk-2 -m "Map Jira issue fields"
tk list
tk next
tk done tk-1
tk remote set github
tk promote tk-1
```

If Backend creation has an indeterminate outcome, inspect the Mutation with
`tk sync log`, then pick the exit that matches what you find upstream. Use
`tk promote reconcile <id> <backend-key>` after confirming the created object,
`tk promote retry <id>` only when creating the Backend object again is safe, or
`tk promote cancel <id>` to give up on it.

`tk promote cancel <id>` withdraws the whole `tk promote` invocation the item
belongs to and returns those items to local. It reaches no Backend, so it works
even with a broken Remote. Withdrawing a Promotion whose creation outcome was
never observed reports that any object it created is now untracked — tk holds no
identity for it, so finding and adopting or closing it is yours to do.

Use `tk --help`, `tk <command> --help`, or `man tk` for the command
reference.

## Repository Store and recovery

Your Repository Store lives outside the checkout and survives its deletion.
All linked Workspaces share it. The location is:

| Platform | Store directory |
| --- | --- |
| Linux | `$XDG_DATA_HOME/tk/stores/<Store ID>/`, or `~/.local/share/tk/stores/<Store ID>/` |
| macOS | `~/Library/Application Support/tk/stores/<Store ID>/` |
| Windows | `<LocalAppData>/tk/stores/<Store ID>/` |

`tk init` prints the database path. Git's repository-local `tk.storeId` and
that Store's `store.json` record the association. Do not copy the Git setting
to another repository to share a Store.

| Situation | Action |
| --- | --- |
| Fresh repository | Run `tk init`. |
| Healthy association | Work normally; `tk init` reports the existing Store. |
| Legacy Store in Git metadata | Stop all tk processes, run `tk init`, and keep old binaries stopped until cleanup finishes. |
| Lost pointer or moved checkout | Run `tk init` to inspect ranked evidence and copy a complete `tk init --attach <Store ID>` command. A uniquely identified Vacant Store may be repaired automatically. |
| Start separately after a broken association | Run `tk init --new`; prior Stores remain intact. |
| Missing or corrupt manifest | Restore the metadata manually; init cannot reconstruct it. |
| Interrupted migration | Retry `tk init`; retain both copies and all progress records until it succeeds. |
| Interrupted attachment | Retry the same attach command; use plain `tk init` if the association is already healthy. |

Attachment requires release of the former repository's ownership. If the former
checkout's parent path is unavailable, restore access before retrying: tk cannot
tell a deleted checkout from a missing volume. After moving a whole checkout,
the old checkout directory must be readable so tk can establish that its `.git`
entry is absent. Healthy associations refuse both `--attach` and `--new`.

Migration preserves Tickets, Local Fields, Plans, Mutations, and Store Backups.
Before pointer installation, retry rebuilds from legacy data; afterward, the
new Store is authoritative and retry finishes cleanup. If legacy files changed
after cutover, preserve both Stores and restore manually. Windows recovery
covers process interruption; sudden-power-loss safety is not established.

Ordinary commands read Git metadata and leave the manifest untouched, but need
write access to the Store for SQLite and lock files. In a sandbox, grant the
specific `tk/stores/<Store ID>/` directory as a writable root. Initialization
and reassociation also need to write Git config and the data-root initialization
lock.

## License

[MIT](./LICENSE)
