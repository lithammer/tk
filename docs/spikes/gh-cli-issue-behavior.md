# Spike: `gh issue` CLI behaviour (tk-34 adapter)

> **Superseded in part by ADR-0054.** The personal-repository `bug` Label
> fallback and the typeless Epic path described below are gone: Bug and Epic
> Promotion require native Issue Types. This spike keeps its history.

The GitHub Backend Adapter's fixtures and failure classifier draw on live
`gh` probes and the source for the versions named below. The rule for using
that evidence is in
[ADR-0016](../adr/0016-mutation-failure-is-a-flat-classified-record.md).

- **Base probe gh version:** 2.94.0 (2026-06-10)
- **Bug Promotion probe gh version:** 2.98.0 (2026-08-20)
- **Sandbox:** private repo `lithammer/tk-gh-playground` — kept, not deleted
  (the auth token lacks `delete_repo` scope), so it persists as a re-probe
  sandbox for future `gh` versions / new failure modes.
- **Base probe date:** 2026-06-23
- **Bug Promotion probe date:** 2026-08-24

## Read / Backend Pull — `gh issue view <n> --json …`

Current Adapter field set: `number,title,body,state,issueType,labels,url`. The
original issue #1 probe, before the Adapter requested `labels`, returned:

```
{"body":"Body of an open issue","issueType":null,"number":1,"state":"OPEN","title":"Open task","updatedAt":"2026-06-23T08:43:33Z","url":"https://github.com/lithammer/tk-gh-playground/issues/1"}
```

- `state` is **UPPERCASE** `OPEN` / `CLOSED` (a PR also yields `MERGED`). The
  adapter maps `OPEN → open`, `CLOSED → done`, and treats anything else as a
  typed error (never reached for issues; the PR url guard fires first).
- `issueType` is `null` on an untyped issue. The object form
  (`{"id","name",…}`) could **not** be observed in the base probe:
  `gh issue create --type Bug` reported
  `type "Bug" not found; available types: ` (exit 1), then created a typeless
  issue. The later Bug Promotion probe found REST-listed types but still could
  not apply one; see below. The `name == "Bug" → Bug` mapping remains
  **source-derived** pending a probe on an organization repository with usable
  Issue Types.
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

Re-probed on 2026-08-09 before enabling GitHub Promotion. Task Tickets and
Epics use the typeless command `gh issue create --title <title> --body <body>`;
Epic membership is a later `gh issue edit --parent` Mutation. Bug Tickets use
the direct GraphQL path described below.

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

The typeless path never sends `--type`, parent, Dependency, Label, or Assignee
flags during creation. Reconciliation and explicit-risk retry are separate
recovery work; an indeterminate Promotion remains `applying` and is not
replayed automatically.

### Bug representation — Issue Type or personal-repository Label

Probed on 2026-08-24 against the user-owned
`lithammer/tk-gh-playground` repository with `gh` 2.98.0:

- The REST repository Issue Types endpoint listed enabled Task, Bug, and
  Feature types. Despite that list, `gh issue edit --type Bug` reported no
  available types and changed nothing. The REST list alone therefore does not
  prove that the repository can use a native Issue Type.
- A REST create with an unavailable `type` created issue #24 but left it
  typeless. The probe issue was closed after inspection. This is observed REST
  behavior, not a claim about `gh issue create` from source.
- A direct `createIssue` GraphQL mutation with an invalid Issue Type ID returned
  no issue. A title search confirmed that it created nothing.

Additional evidence collected on 2026-08-25 separates the schema contract from
one repository observation:

- **Primary schema evidence:** Live GitHub schema introspection reports
  `Repository.issueTypes` as nullable (`OBJECT IssueTypeConnection`, not
  `NON_NULL`), with the description “A list of the repository's issue types”.
- **Repository observation:** Querying user-owned `lithammer/tk` returned
  `{"data":{"repository":{"issueTypes":null}}}` with no errors. `gh repo
  view` reported `isInOrganization:false`.
- **Adapter policy and risk:** An initial null is treated as terminal native
  Issue Type absence so the supported personal-repository `bug` Label fallback
  remains reachable. The observation does not prove why GitHub returned null;
  the API exposes no discriminator, so an undocumented reason remains a risk.

Source at the installed `gh` 2.98.0 tag establishes the available read
surfaces. `api/query_builder.go` exposes `issueType` and `labels` to
`gh issue view --json`, and `isInOrganization` to `gh repo view --json`.
`api/queries_issue.go` shows that `RepoIssueTypes` requests only the first 50
Issue Types and no page information, so it cannot prove that an enabled `Bug`
type is absent. The Adapter instead pages the `issueTypes` GraphQL connection
to exhaustion.

Bug Promotion uses one direct `createIssue` GraphQL mutation. Preflight and
Apply prefer an enabled, case-insensitive `Bug` Issue Type. When none exists, a
personal repository may use an existing case-insensitive `bug` Label; an
organization repository has no fallback. The mutation sends exactly one of
`issueTypeId` and `labelIds`, which creates the issue and its Ticket Kind
representation as one effect. The Adapter never creates or renames repository
taxonomy.

## Relationships — `--parent` / `--remove-parent` / `--remove-sub-issue`

Probed 2026-08-10 on `gh` 2.97.0 before choosing the Epic-membership removal
argv (issues 7 and 8, `[tk-132-probe-2026-08-10-1901116e]`, closed after).
Every call passed **canonical issue URLs** for both the subject and the flag
value, matching the argv the Adapter builds from stored Backend keys.

- Sub-issues **are** available on this personal private repo, unlike usable
  Issue Types. All ten calls exited **0** with empty stderr and the edited
  issue's URL on stdout.
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

### Error shapes in `gh issue`

The HTTP status determines how `gh issue` renders an API error. This is the
behaviour in gh v2.97.0 with go-gh v2.13.0 (`pkg/api/errors.go` and
`pkg/api/graphql_client.go`).

- A **non-2xx** response becomes `HTTPError`, rendered
  `HTTP <code>: <message> (<url>)`.
- An **HTTP 200 response with GraphQL errors** becomes `GraphQLError`. Its
  text starts with `GraphQL: <message>` and includes the path when present.

GitHub reports issue validation failures in a GraphQL `errors` array at
HTTP 200.

| class | status | evidence |
|---|---|---|
| `auth` | ✅ | bad `GH_TOKEN` → exit 1, stderr `HTTP 401: Bad credentials (https://api.github.com/graphql)` + `Try authenticating with:  gh auth login -h github.com`. All three anchors (`HTTP 401`, `Bad credentials`, `gh auth login`) present. |
| not-found | ✅ (→ `unknown` by design) | `gh issue view 999999` → exit 1, stderr `GraphQL: Could not resolve to an issue or pull request with the number of 999999. (repository.issue)`. Note the **lowercase** "an issue or pull request" — this *validates* dropping `sync_conflict`: the classifier deliberately does not match this brittle, variable string, so a deleted Adopted issue → `unknown`. |
| `rate_limited` | ❌ unobserved, anchor sound | The anchors match message text (`rate limit exceeded` / `secondary rate limit`), so they fire in either shape. GitHub reports a primary GraphQL rate limit as 200-with-errors and a secondary one as 403; the anchors are indifferent to which. |
| Title validation | ✅ (→ `unknown`) | On 2026-08-18, an overlong title produced exit 1 and `GraphQL: Title is too long (maximum is 256 characters) (createIssue)`. The title edit fixture uses the observed `updateIssue` diagnostic. |
| `transient` | ❌ unobserved, anchor sound | 502/503/504 are transport-level, so they arrive HTTP-shaped and the `HTTP 502/503/504` anchors can fire. |

Each failed command in the `gh issue` probe exited **1**. The Adapter
classifies the diagnostic text.

The `gh issue` GraphQL error format also holds in gh v2.105.0.
[go-gh v2.17.0 formatter](https://github.com/cli/go-gh/blob/v2.17.0/pkg/api/errors.go#L59)

tk also reads GraphQL error messages from the response body that
`gh api graphql` writes to stdout.
[gh v2.105.0 response handling](https://github.com/cli/cli/blob/v2.105.0/pkg/cmd/api/api.go#L484)

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
- tk's own Promotion Operation behaviour is recorded separately in
  [promotion-operation-canary.md](./promotion-operation-canary.md).
