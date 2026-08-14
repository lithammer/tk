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
`tk sync log`, then use `tk promote reconcile <id> <backend-key>` after
confirming the created object. Use `tk promote retry <id>` only when creating
the Backend object again is safe.

When the Backend will never accept a Promotion, `tk promote cancel <id>`
withdraws the whole `tk promote` invocation it belongs to and returns those
items to local. It reaches no Backend, so it works even with a broken Remote.

Use `tk --help`, `tk <command> --help`, or `man tk` for the command
reference.

## License

[MIT](./LICENSE)
