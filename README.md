# tk

tk (pronounced "ticket") is an agent-first command-line tool for managing work items through a simple local interface and pluggable issue-tracker backends.

The goal is similar in spirit to Beads: make work visible to humans and agents from the command line. tk deliberately aims for a simpler architecture, local-first capture, and backend adapters for systems like GitHub Issues and Jira.

Supported platforms: Linux, Windows, Windows (ARM), and macOS.

## Install

### Linux and macOS

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

## Current Design

- tk is implemented in Zig 0.16.
- **Tickets** are backend-agnostic work items.
- **Epics** group related Tickets and require explicit closure.
- New Tickets and Epics are **local by default**, even when a Primary Backend exists.
- **Promotion** explicitly converts a Local Ticket or Local Epic into a backend-backed object.
- Promotion replaces the visible local ID with the backend ID and keeps the old local ID as an alias.
- A repo can have zero or one **Primary Backend**.
- **Backend Adapters** map tk domain concepts to backend systems.
- Backend Adapters expose pull and apply-mutation operations; the sync engine owns ordering, cursors, retries, and failure policy.
- The **Repository Store** is shared across workspaces for one repository and is untracked local state by default.
- The Repository Store uses SQLite.
- **Workspace Scope** is local-only and gives scope-aware commands repository context without becoming an implicit item target.
- Workspace Scope is stored in git worktree config for v1, with read-only branch-name inference as a fallback.
- `tk start <id>` marks work active; `tk stop <id>` moves active work back to open.
- `tk worktree start <id> [path]` creates a Ticket branch and scoped git worktree, defaulting to a sibling worktree path.
- `tk worktree` reports or changes the current Workspace Scope stored in git worktree config.
- `tk prime` prints static Markdown embedded from [src/commands/prime.md](./src/commands/prime.md) for new or compacted sessions.
- **Mutations** record durable local intent as named domain operations so they can be synced by Backend Adapters.
- Sync pulls backend state before applying pending Mutations, applies Mutations in global sequence order, and stops on the first failure.
- Failed Mutations retry on the next sync; explicit `tk sync --skip <mutation-id>` remains visible.
- `tk sync log` inspects pending, failed, skipped, and applied Mutations.

## Domain Language

The project glossary lives in [CONTEXT.md](./CONTEXT.md). Keep design documents and code aligned with that language.

Important distinctions:

- **Epic membership** groups work. **Dependency** describes blocking order.
- **Priority** is local-only in v1 and sorts `P0` before `P4`.
- **Item Status** is `open`, `active`, or `done` for both Tickets and Epics.
- Blocking is separate from Item Status: Dependencies point at blocking items,
  and External Blockers capture outside blockers.
- `active` means current work. **Assignee** support is deferred and may be
  omitted entirely.
- **Workspace Scope** is local-only, is not synced to backends, and is not an implicit item target.

## Where to Look

- [ARCHITECTURE.md](./ARCHITECTURE.md) — module map, boundaries, and
  Repository Store invariants.
- [AGENTS.md](./AGENTS.md) — agent-facing conventions (code documentation,
  error handling, testing).
- [docs/cli.md](./docs/cli.md) — the v1 CLI surface.
- [docs/adr/](./docs/adr/) — recorded design decisions.
- [docs/design-questions.md](./docs/design-questions.md) — open design work.

## Testing Direction

tk should use layered tests from the start:

- Zig unit tests for domain behavior and command handlers.
- Inline snapshots for small rendered outputs.
- Fake subprocess runners for Backend Adapter tests.
- txtar-based CLI scenario fixtures with a small script runner inspired by `rsc.io/script` and Rust's `trycmd`.

The CLI scenario runner should support multi-step command tests, expected stdout/stderr/exit checks, simple filesystem assertions, elision for unstable output, and an explicit snapshot update mode.

## CLI Direction

Creation should keep the common path short:

```sh
tk add -m "Update README"               # creates a task Ticket
tk add --bug -F bug-report.md           # creates a bug Ticket
tk add --epic -m "Jira backend"         # creates an Epic
tk add --bug -F - < rich-bug-report.md  # reads message from stdin
tk add --priority P1 -F -               # creates a higher-priority local Ticket
```

`tk add` uses git-commit-style message input: repeatable `-m/--message`,
`-F/--file`, or `-F -` for stdin. Editor mode for bare `tk add` is deferred
from v1. The first paragraph becomes the title and later paragraphs become the
body. `--bug` and `--epic` are mutually exclusive. `Epic` is not a Ticket Kind.

`tk next` selects the ready Ticket with the lowest Effective Priority, then lowest own Priority, then oldest creation order, within the active Workspace Scope. Effective Priority lifts a ready Ticket above its own Priority when it transitively blocks a higher-Priority Ticket, so a P3 chore that gates a P1 outranks an unrelated P2. The pick prints to stdout; when Effective Priority is lower than own Priority, a `<id>: Effective Priority <P> (via <id>)` rationale prints to stderr so `id="$(tk next)"` scripting stays clean. `tk next` does not select Epics. The default Priority is `P2`.

Priority is set with `tk add --priority P0..P4` or `tk update <id> --priority P0..P4`; v1 does not have a top-level priority command.

`tk list` defaults to a tree view: Epics are top-level rows, child Tickets are nested under their Epic, and unparented Tickets are top-level rows. `tk list --ready` keeps the tree shape and includes non-empty Epics as containers for ready child Tickets. Rows use decorative tree glyphs plus compact status, priority, and kind markers. They do not render Origin as a separate field; Local or Backend origin is normally inferred from the Display ID shape.

tk's default command paths should let agents manage local work safely. Commands that affect upstream state or sync repair, such as `promote` and `sync`, stay explicit and visible.

`tk promote <id>` promotes only the target. `tk promote <epic-id> --children` also promotes directly contained Local Tickets; it does not follow Dependencies.

The CLI uses `remote` for backend configuration and filters:

```sh
tk remote
tk remote set <kind>
tk remote clear
tk list --remote
```

V1 supports zero or one configured remote. Backend authentication is delegated to external CLIs such as `gh` or `acli`.
