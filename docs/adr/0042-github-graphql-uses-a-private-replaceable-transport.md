# GitHub GraphQL uses a private replaceable transport

The GitHub Backend Adapter owns typed GraphQL operations behind a private
transport port. The production transport runs them through `gh api graphql`
and the existing `ProcRunner`. A future pure Rust transport can replace it
without changing the GraphQL operations or the Backend Adapter interface.
GraphQL stays an implementation detail rather than Backend vocabulary.

The port is private and does not add runtime transport selection. Its contract
has three outcomes: `NotStarted`, `Completed`, and `OutcomeUnobserved`.
`Completed` retains the raw body and a transport-neutral delivery status even
when delivery reports failure. `NotStarted` distinguishes an unavailable
transport from one that failed to start a request. `OutcomeUnobserved` means a
request started but tk could not observe its result. Reads, ordinary edits,
and non-idempotent creation can then keep their different failure and
effect-certainty rules. The CLI transport owns subprocess arguments and maps
their results onto this vocabulary. The port has no `ProcError`, exit-code,
stdout, or stderr fields: the CLI maps stdout to the response body and stderr
to transport detail.

The port exchanges GraphQL wire values rather than typed GitHub results. A
request carries its target host, operation name, document, and JSON variables;
an exchange retains the raw response body and transport evidence. Typed
GraphQL operations above the port decode the response envelope and map it into
tk's domain types. The envelope retains each error's optional response path so
an operation with deterministic aliases can map item-specific failures back to
its input; an absent or unknown path remains a request-wide failure. The CLI
transport sends the standard JSON request on stdin to
`gh api graphql --input -` with an explicit JSON content type; a future Rust
transport can send the same body over HTTP. The CLI transport parses the
complete stdout response even when `gh` reports GraphQL errors with a non-zero
exit.

Source evidence comes from `cli/cli` tag `v2.98.0`, which matches the installed
`gh` used for the tk-181 measurements:

- `pkg/cmd/api/api.go` documents `--input -`, and `openUserFile` maps `-` to
  stdin. `apiRun` also selects `ApiOptions.Hostname` before making the request.
- `pkg/cmd/api/http.go` passes that host to `ghinstance.GraphQLEndpoint` for
  the `graphql` path.
- `pkg/cmd/api/api.go`'s `parseErrorResponse` copies the response body, and
  `processResponse` writes that copy before returning the GraphQL error. The
  `GraphQL error` case in `pkg/cmd/api/api_test.go` checks both stdout body and
  stderr error text. Thus a non-zero `gh` result can still carry a complete
  GraphQL envelope on stdout.

Test evidence, not a live probe, checks tk's side of the contract. The CLI
transport tests pin the stdin bytes, content-type header, and host argument.

Tests follow the same seam. Typed-operation tests use a fake GraphQL transport
for response envelopes, pagination, identity checks, and effect certainty. A
focused CLI-transport suite uses `FakeRunner` for exact argv, host routing,
request encoding, and subprocess evidence. The two suites do not duplicate
the same semantic cases.

Every direct GraphQL path uses this port: exact-key Backend Pull, Bug
creation, Issue Type pagination, and Label pagination. Native `gh issue` and
`gh repo` operations remain unchanged. Each GraphQL query is a private typed
operation that binds its document and variables to an associated response
type; callers cannot choose an unrelated decoder for a request.
