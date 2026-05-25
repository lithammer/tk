# tk

tk (pronounced "ticket") is an agent-first command-line tool for managing work
items through a simple local interface and pluggable issue-tracker backends.

The goal is similar in spirit to Beads: make work visible to humans and agents
from the command line. tk deliberately aims for a simpler architecture,
local-first capture, and backend adapters for systems like GitHub Issues and
Jira.

Prebuilt releases are published for Linux, Apple Silicon macOS, Windows, and
Windows ARM.

## Install

### Linux and Apple Silicon macOS

```sh
curl -fsSL https://raw.githubusercontent.com/lithammer/tk/main/scripts/install.sh | sh
```

### Windows

Download `tk-x86_64-windows-gnu.exe` or `tk-aarch64-windows-gnu.exe` from the
[latest release](https://github.com/lithammer/tk/releases/latest), rename it to
`tk.exe`, and place it in a directory on `PATH`. `tk self-update` works after
that.

### Upgrade

Use `tk self-update`. Re-running the install script is also supported. Use the
variables below for version pinning or ABI switching.

### Environment variables

| Variable | Default | Effect |
| --- | --- | --- |
| `TK_INSTALL_DIR` | `/usr/local/bin` as root, `~/.local/bin` otherwise | Install directory. |
| `TK_VERSION` | latest release | Release version to install. |
| `TK_LINUX_ABI` | `musl` | Linux ABI variant: `musl`, or `gnu` on x86_64 Linux. |

### Build from source <a id="build-from-source"></a>

Run `mise install` to install the Zig version pinned in `.mise.toml`, then
`zig build`; the binary is written to `zig-out/bin/tk`.

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
```

Use `tk --help`, `tk <command> --help`, or `tk manpage` for the command
reference.

## Project docs

- [CONTEXT.md](./CONTEXT.md) — domain language and model.
- [ARCHITECTURE.md](./ARCHITECTURE.md) — module map, boundaries, and
  Repository Store invariants.
- [docs/adr/](./docs/adr/) — recorded design decisions.
