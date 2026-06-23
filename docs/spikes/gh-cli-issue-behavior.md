# Spike: `gh issue` CLI behaviour (tk-34 adapter)

Observed behaviour of the `gh issue` subcommands the GitHub Backend Adapter
(`crates/tk/src/remote/github.rs`) drives, captured against a real
authenticated `gh` so the adapter's `FakeRunner` fixtures and the
`FailureClass` classifier rest on observation, not source-reading (ADR-0016:
the classifier must be spike-grounded, not designed from imagination).

- **gh version:** 2.94.0 (2026-06-10)
- **Sandbox:** private repo `lithammer/tk-gh-playground` — kept, not deleted
  (the auth token lacks `delete_repo` scope), so it persists as a re-probe
  sandbox for future `gh` versions / new failure modes.
- **Date:** 2026-06-23

## Read / Backend Pull — `gh issue view <n> --json …`

Adapter field set: `number,title,body,state,issueType,updatedAt,url`. Verbatim
(issue #1):

```
{"body":"Body of an open issue","issueType":null,"number":1,"state":"OPEN","title":"Open task","updatedAt":"2026-06-23T08:43:33Z","url":"https://github.com/lithammer/tk-gh-playground/issues/1"}
```

- `state` is **UPPERCASE** `OPEN` / `CLOSED` (a PR also yields `MERGED`). The
  adapter maps `OPEN → open`, `CLOSED → done`, and treats anything else as a
  typed error (never reached for issues; the PR url guard fires first).
- `issueType` is `null` on an untyped issue. The object form
  (`{"id","name",…}`) could **not** be observed: issue types are not
  configurable on a personal private repo — `gh issue create --type Bug` →
  `type "Bug" not found; available types: ` (exit 1), and it still creates the
  issue typeless. So the `name == "Bug" → Bug` mapping stays **source-derived**;
  re-probe on an org repo with issue types configured.
- `url` is `…/issues/<n>` for an issue, `…/pull/<n>` for a PR.
- A single `gh issue view <n>` writes one JSON object (not an array).

## Pull requests — the shared number space

`gh issue view <PR#>` resolves a pull request and returns it as an
issue-shaped object — it does **not** error. Confirmed exit 0 / no panic with
the **full** adapter field set (including the issue-only `issueType`, the
cli/cli#9301 panic risk):

- `cli/cli#13705` (open PR): `{"issueType":null,…,"url":"https://github.com/cli/cli/pull/13705"}`
- `lithammer/tk#19` (merged PR): `{"issueType":null,"number":19,"state":"MERGED","url":"https://github.com/lithammer/tk/pull/19"}`

⇒ The adapter's `url`-ends-in-`/pull/<n>` guard is correct and necessary, and
the **single fetch is panic-safe** for our field set. A merged PR's `MERGED`
state never reaches the state parse — the url guard rejects the PR first.

## Apply — `edit` / `close` / `reopen`

- **Success is exit 0**, even though these print to **stderr** on success:
  - `gh issue close <open>` → exit 0, stderr `✓ Closed issue OWNER/REPO#N (title)`
  - `gh issue reopen <closed>` → exit 0, stderr `✓ Reopened issue …`
  - `gh issue edit …` → exit 0, **stdout** is the issue URL.
- **Idempotent no-ops exit 0** with an informational stderr line — the key
  fixture assumption, confirmed verbatim:
  - `gh issue close <already-closed>` → exit 0, stderr
    `! Issue OWNER/REPO#N (title) is already closed`
  - `gh issue reopen <already-open>` → exit 0, stderr
    `! Issue OWNER/REPO#N (title) is already open`

  ⇒ The adapter MUST judge success by **exit code**, never stderr-emptiness.
- `--body ""` **blanks** the body (confirmed: the body became `""`); omitting
  `--title` left the title unchanged. The adapter always sends both flags with
  the real values.
- **Correction to source-reading:** a bare `gh issue close` (no `--reason`)
  records `stateReason: "COMPLETED"` on GitHub — *not* an empty/absent reason
  as source-reading suggested. tk never sets or reads `stateReason` (ADR-0023:
  closing reason is a Local Field), so the two-state `done ↔ CLOSED` mapping is
  unaffected; the default is simply GitHub's, not tk's.

## Failure modes — the `FailureClass` classifier

| class | observed? | evidence |
|---|---|---|
| `auth` | ✅ | bad `GH_TOKEN` → exit 1, stderr `HTTP 401: Bad credentials (https://api.github.com/graphql)` + `Try authenticating with:  gh auth login -h github.com`. All three anchors (`HTTP 401`, `Bad credentials`, `gh auth login`) present. |
| not-found | ✅ (→ `unknown` by design) | `gh issue view 999999` → exit 1, stderr `GraphQL: Could not resolve to an issue or pull request with the number of 999999. (repository.issue)`. Note the **lowercase** "an issue or pull request" — this *validates* dropping `sync_conflict`: the classifier deliberately does not match this brittle, variable string, so a deleted Adopted issue → `unknown`. |
| `rate_limited` | ❌ not provokable on demand | source-derived anchors (`rate limit exceeded` / `secondary rate limit`). |
| `validation` | ❌ not provokable on demand | source-derived anchor (`HTTP 422`). |
| `transient` | ❌ not provokable on demand | source-derived anchors (`HTTP 502/503/504`). |

Every observed `gh` error exited **1** (never a discriminating code), confirming
the classifier correctly gates on stderr substrings, not the exit code (exit-4
for auth is unreliable — cli/cli#9338).

## Reconciliation with tk-34

- Observed behaviour matches the implementation; **no production code changes
  required**.
- Fixture fidelity: the not-found and auth `FakeRunner`/classifier fixtures were
  updated to the observed verbatim strings.
- Stays source-derived (unverifiable on this sandbox): the `issueType` object
  form (Bug kind), and the `rate_limited` / `validation` / `transient` stderr
  anchors. Re-probe against an org repo with issue types, or when those failure
  modes occur in the wild — the `tk-gh-playground` repo is kept for exactly
  this.
