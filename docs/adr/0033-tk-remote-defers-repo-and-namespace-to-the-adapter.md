# `tk remote` configures the Primary Backend minimally; repo resolution and Display ID namespace are deferred to the Backend Adapter

tk-106 ships `tk remote set` / `tk remote clear` / `tk remote` (show) so a GitHub
Remote becomes configurable and CLI-testable end-to-end, unblocking tk-34 (the
GitHub Backend Adapter). Two things the tk-106 ticket scoped *in* are
deliberately cut, both for the same reason: they require backend-specific
knowledge the Backend Adapter owns (tk-34 for GitHub, tk-35 for Jira), not the
config-CRUD command. This applies the project's "defer evidence-determined
types" rule (AGENTS.md) and the ADR-0016 precedent at the command layer.

## The Display ID namespace a Remote occupies is per-(kind, config), not a property of the kind

The ticket scoped a `validate_remote_against_local_prefix` guard: refuse
`tk remote set github` when the backend's Display ID namespace would collide
with the local Display ID prefix (e.g. local prefix `gh` versus GitHub's
`gh-<n>`).

But that namespace is not a static property of the backend kind:

- **GitHub** has no native short key — issues are numbers inside `owner/repo` —
  so tk *chooses* a prefix (`gh`). That choice is tk-34's contract.
- **Jira** hands tk the prefix. A Jira issue key is already `PROJECT-NUM`
  (`SS-123`, `DSC-456`, `PNR2-789`), so the natural Display ID for a
  Jira-backed Ticket is its native key and the namespace prefix is the
  configured `project` — backend/config-determined, read from the Remote's
  configuration, not from the kind.

So the namespace is a function of `(kind, config)`. A static
`BackendKind::display_prefix()` cannot express "the project key out of this
Remote's config," and hardcoding one would pre-commit both adapter contracts
before either adapter exists. The guard therefore belongs to the tickets that
actually mint backend Display IDs: tk-34 owns the GitHub `gh` namespace, tk-35
owns the Jira project-key namespace, and each adds the set-time check (which
physically lives in `tk remote set`) once its namespace contract is real.

Correctness does not depend on the set-time guard. `merge_backend_snapshots`
already raises `MergeError::DisplayIdCollision` per snapshot during Backend Pull
(ADR-0010), so a colliding Display ID is caught regardless. The set-time check
was only fail-fast UX; it is cheap to add later and carries no correctness debt
in the interim.

## The GitHub repo is resolved from the checkout, not persisted

The ticket scoped `tk remote set github [<repo>]` storing
`config_json = {"repo":"OWNER/NAME"}`.

The Repository Store is bound 1:1 to a git repository: it lives at
`<git-common-dir>/tk/tk.db` and is shared by all Workspaces of that repository.
Git remotes live in the common dir's config, so `gh` and `git` resolve the same
GitHub repository from any Workspace. `OWNER/NAME` is therefore a stable,
derivable property of the one git repository the store already belongs to.
Persisting it only creates a second copy that can drift from the real remote.

So tk-106 writes `config_json = {}` — recording only that the Primary Backend is
GitHub — and defers repo resolution to tk-34's adapter, which lets `gh issue`
resolve the repository from the checkout at Mutation Apply / Backend Pull time
(no `--repo`). The hard cases — forks whose issues live upstream, multiple
remotes, GitHub Enterprise hosts — are owned by `gh` and persisted in the
repository's git config via `gh repo set-default`, stable across Workspaces; tk
neither reimplements nor caches that resolution.

This cut cascades through the command surface:

- `tk remote set github` takes no arguments, writes `config_json = {}`, and is
  idempotent (insert when absent, no-op when already GitHub). There is no
  set-time `gh` call, no `<repo>` argument, and no override.
- Because there is no stored repo to re-point, there is no "replace with a
  different repo" hazard, so the orphan guard is needed only on
  `tk remote clear` (refuse when pending or failed Mutations would be orphaned).
- `tk remote` (show) prints only the configured kind; it stays store-only and
  does not call `gh` to display the target repository.

The cost is that tk-34 must resolve its repository from the checkout rather than
sync to an arbitrary pinned repo. Accepted: pointing a repository's store at a
*different* GitHub repository is a near-always-wrong configuration.

## Considered Options

- **Implement the prefix guard now, on a static `BackendKind::display_prefix()`.**
  Rejected: the namespace is per-(kind, config) — Jira's prefix is the
  configured project key — so a static kind→prefix map is the wrong model, and
  committing both GitHub's `gh` and a Jira prefix before either adapter exists
  pre-commits contracts that belong to tk-34 / tk-35. The merge-time
  `MergeError::DisplayIdCollision` already guarantees correctness.
- **Store the resolved repo at set time** (`gh repo view` to derive `OWNER/NAME`,
  plus a `<repo>` override and a guarded replace path). Rejected: it caches git
  state that can drift, and drags a set-time `gh` dependency, a repo argument, an
  override, and a replace-orphan hazard into a slice whose only job is to record
  that the Primary Backend is GitHub. The repo is derivable from the one git
  repository the store belongs to.
- **Support `tk remote set jira` in v1.** Rejected: no Jira adapter is coming
  (tk-35 is deferred), and its configuration shape (`site` + `project`) would be
  a guess committed before the consumer exists. `jira` remains a valid
  `remotes.backend_kind` and a `BackendKind` variant so a future fixture, show,
  or tk-35 can use it, but `tk remote set jira` is rejected at the command layer
  with a precise diagnostic (Usage, exit 2 — the same exit a value-restricted
  `<kind>` would yield).

## Consequences

- tk-34 resolves its repository from the checkout via `gh`, and owns both the
  GitHub Display ID namespace (`gh`) and the set-time collision guard if it
  wants one. tk-35 owns the Jira project-key namespace and the Jira
  configuration shape.
- `config_json = {}` is the v1 GitHub Remote contract. Nothing reads the field
  yet, so tk-34 may add fields if it discovers it needs them.
- `MergeError::DisplayIdCollision` (ADR-0010) is the only Display ID collision
  backstop until an adapter adds a set-time guard.
- `BackendKind { Github, Jira }` is introduced with `text()` (the SQL spelling
  the `remotes.backend_kind` CHECK stores) and CLI parsing, but no
  `display_prefix()`. The command is born on the ADR-0032 diagnostics seam,
  returning `Result<Exit, CommandError>`.
