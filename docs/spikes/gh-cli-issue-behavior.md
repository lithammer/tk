# Spike: `gh issue` CLI behaviour (tk-34 adapter)

Behaviour of the `gh issue` subcommands the GitHub Backend Adapter
(`crates/tk/src/remote/github.rs`) drives, so the adapter's `FakeRunner`
fixtures and the `FailureClass` classifier rest on evidence rather than on
what tk's own code assumes (ADR-0016: the classifier must be spike-grounded,
not designed from imagination). Two kinds of evidence appear here and each
claim says which it is: behaviour captured against a real authenticated `gh`,
and — where one observation cannot settle what is *possible* — `gh`'s own
source, which is open. Both beat inferring gh's behaviour from tk's
expectations of it.

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

## Promotion — `gh issue create`

Re-probed on 2026-08-09 before enabling GitHub Promotion. Both Task Tickets
and Epics use the typeless command `gh issue create --title <title> --body
<body>`; Epic membership is a later `gh issue edit --parent` Mutation.

- A successful create prints one canonical issue URL on stdout. The trailing
  issue number is the backend key and yields Display ID `gh-<number>`.
- `gh issue create --type Bug` can exit 1 with empty stdout and stderr while a
  typeless issue appears later. Completed nonzero is therefore not evidence
  that creation had no effect, and arbitrary validation text is not enough to
  permit automatic replay.
- Bad credentials (`HTTP 401` / `Bad credentials`) reject before creation and
  certify no effect. `ExecutableNotFound` and `SpawnFailed` runner errors also
  happen before a child exists. A runner failure after successful spawn has
  unknown effect and is indeterminate.
- Empty or malformed stdout, network/server errors, and every other completed
  result without a trustworthy issue URL are indeterminate. A trustworthy
  receipt certifies creation even if the process also reports failure.

Probed again 2026-08-18 (tk-142) for the shape of a validation rejection:

- **The advertised 256-character title limit is not what is enforced.** A
  257-character title was accepted and stored intact, as was 1024. A
  70000-character title was rejected. The boundary sits somewhere above 1024 and
  was not pinned.
- The rejection arrives **GraphQL-shaped**, not HTTP-shaped: exit 1, empty
  stdout, stderr `GraphQL: Title is too long (maximum is 256 characters)
  (createIssue)`. `gh issue create` drives the `createIssue` mutation, so there
  is no `HTTP 422:` prefix to match — see the classifier table below.
- The same text appears from `updateIssue` when an edit carries the over-long
  title.

The Adapter consequently never sends `--type`, parent, Dependency, Label, or
Assignee flags during creation. Reconciliation and explicit-risk retry are
separate recovery work; an indeterminate Promotion remains `applying` and is
not replayed automatically.

## Relationships — `--parent` / `--remove-parent` / `--remove-sub-issue`

Probed 2026-08-10 on `gh` 2.97.0 before choosing the Epic-membership removal
argv (issues 7 and 8, `[tk-132-probe-2026-08-10-1901116e]`, closed after).
Every call passed **canonical issue URLs** for both the subject and the flag
value, matching the argv the Adapter builds from stored Backend keys.

- Sub-issues **are** available on this personal private repo, unlike issue
  types. All ten calls exited **0** with empty stderr and the edited issue's
  URL on stdout.
- `gh issue edit <child> --parent <parent>` attaches. `gh issue view <child>
  --json parent` reads back an object (`id`, `number`, `state`, `title`,
  `url`); `--json subIssues` on the parent reads back
  `{"nodes":[…],"totalCount":1}`.
- Both removal forms work and both clear the relationship:
  `gh issue edit <parent> --remove-sub-issue <child>` and `gh issue edit
  <child> --remove-parent` each leave `parent` as `null`.
- **Both removal forms are silent no-ops when the relationship is absent**, the
  observation that decided the argv:
  - `--remove-parent` on an issue with no parent → exit 0, no stderr.
  - `--remove-sub-issue <child>` where the child is not a sub-issue → exit 0,
    no stderr.

  ⇒ Neither form reports that it changed nothing, so a divergent parent cannot
  be detected from the exit code. `--remove-sub-issue` names the Epic tk
  expected and therefore leaves a *different* parent attached while reporting
  success; `--remove-parent` clears whichever parent the issue has and
  converges on tk's cleared `container_id`. The Adapter uses `--remove-parent`
  (ADR-0021).
- Read-back of `parent` / `subIssues` is available but unused: Backend Pull
  stays field-only, so this is the mechanism a future reconciliation slice
  would build on.

### Dependencies — `--add-blocked-by` / `--remove-blocked-by`

Probed 2026-08-18 on `gh` 2.97.0 (tk-142, issues 9 and 10). The Adapter has
always used these flags for `add_dependency` / `remove_dependency`, but until
this probe they were source-derived only — the 2026-08-10 pass above covered the
Epic-membership flags alone.

- Issue dependencies **are** available on this personal private repo, like
  sub-issues and unlike issue types.
- `gh issue edit <blocked> --add-blocked-by <blocking>` attaches. All calls
  exited **0** with empty stderr and the edited issue's URL on stdout, matching
  the membership flags.
- `gh issue view <blocked> --json blockedBy` reads back
  `{"nodes":[{"id","number","state","title","url"}],"totalCount":1}`; the
  counterpart reads back symmetrically under `--json blocking`.
- `--remove-blocked-by <blocking>` clears the edge.
- **Removal is a silent no-op when the relationship is absent** — exit 0, no
  stderr — the same trap the membership removal forms have. A divergent
  dependency graph therefore cannot be detected from the exit code either.

## Failure modes — the `FailureClass` classifier

### The two error shapes, read from gh's source

`gh` renders an API failure in one of exactly two shapes, and which one a caller
sees is decided by the transport, not by the kind of mistake. Read at cli/cli
`v2.97.0` with `go-gh` `v2.13.0` (`pkg/api/errors.go`,
`pkg/api/graphql_client.go`):

- A **non-2xx** response becomes `HTTPError`, rendered
  `HTTP <code>: <message> (<url>)`.
- A **2xx response carrying an `errors` array** becomes `GraphQLError`, rendered
  `GraphQL: <message> (<path>)`, with no status code anywhere in it.

GitHub reports GraphQL validation failures with HTTP 200 and an `errors` array,
so they always take the second shape. Every issue operation this Adapter drives
is a GraphQL mutation — `createIssue`, `updateIssue`, `AddSubIssue`,
`RemoveSubIssue`, `AddBlockedBy`, `RemoveBlockedBy`, `IssueClose` — and there are
no REST calls in gh's issue command tree at that tag.

⇒ An anchor keyed on an `HTTP <code>:` prefix can only ever match a failure that
happened at the transport layer. An anchor keyed on message text matches in
either shape. The table below rests on that distinction.

| class | status | evidence |
|---|---|---|
| `auth` | ✅ | bad `GH_TOKEN` → exit 1, stderr `HTTP 401: Bad credentials (https://api.github.com/graphql)` + `Try authenticating with:  gh auth login -h github.com`. All three anchors (`HTTP 401`, `Bad credentials`, `gh auth login`) present. |
| not-found | ✅ (→ `unknown` by design) | `gh issue view 999999` → exit 1, stderr `GraphQL: Could not resolve to an issue or pull request with the number of 999999. (repository.issue)`. Note the **lowercase** "an issue or pull request" — this *validates* dropping `sync_conflict`: the classifier deliberately does not match this brittle, variable string, so a deleted Adopted issue → `unknown`. |
| `rate_limited` | ❌ unobserved, anchor sound | The anchors match message text (`rate limit exceeded` / `secondary rate limit`), so they fire in either shape. GitHub reports a primary GraphQL rate limit as 200-with-errors and a secondary one as 403; the anchors are indifferent to which. |
| `validation` | ⛔ **structurally unreachable** | Provoked 2026-08-18 (tk-142): an over-long title on `createIssue` → exit 1, stderr `GraphQL: Title is too long (maximum is 256 characters) (createIssue)`, which matches no anchor and classifies `unknown`. Per the shapes above a validation failure is reported at HTTP 200, so the `HTTP 422` anchor can never fire for any operation the Adapter performs. Tracked in tk-148. |
| `transient` | ❌ unobserved, anchor sound | 502/503/504 are transport-level, so they arrive HTTP-shaped and the `HTTP 502/503/504` anchors can fire. |

Every observed `gh` error exited **1** (never a discriminating code), confirming
the classifier correctly gates on stderr substrings, not the exit code (exit-4
for auth is unreliable — cli/cli#9338).

## Reconciliation with tk-34

- Observed behaviour matches the implementation; **no production code changes
  required**.
- Fixture fidelity: the not-found and auth `FakeRunner`/classifier fixtures were
  updated to the observed verbatim strings.
- Unverified on this sandbox: the `issueType` object form (Bug kind), and the
  `rate_limited` / `transient` anchors — both sound per the shapes above, neither
  provokable here. Re-probe against an org repo with issue types, or when those
  failure modes occur in the wild — the `tk-gh-playground` repo is kept for
  exactly this.
- Corrected 2026-08-18, first by canary (tk-142) and then from gh's source: the
  Dependency flags are observed rather than inferred, and `validation` is
  structurally unreachable rather than merely unobserved.
- **Fixture defect, tracked in tk-148**: the `FakeRunner` and classifier cases in
  `remote/github.rs` that assert `HTTP 422: …` for `createIssue` and for the
  `--parent` edit encode a shape gh cannot emit for those mutations. ADR-0016
  requires fixtures rest on observation, so an invented shape there is a defect
  rather than untidiness. The `HTTP 422` strings elsewhere in the tree are opaque
  detail payloads in persistence and rendering tests and make no claim about gh.
- tk's own Promotion Operation behaviour is recorded separately in
  [promotion-operation-canary.md](./promotion-operation-canary.md).
