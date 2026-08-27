# GitHub GraphQL uses a private replaceable transport

The GitHub Backend Adapter owns typed GraphQL operations behind a private
transport port. The first production adapter runs them through
`gh api graphql` and the existing `ProcRunner`; a future pure Rust adapter can
replace that transport without changing the GraphQL operations or the Backend
Adapter interface. GraphQL stays an implementation detail rather than Backend
vocabulary.

The port is private and does not add runtime transport selection. Its contract
has three outcomes: `NotStarted`, `Completed`, and `OutcomeUnobserved`.
`Completed` retains the raw body and diagnostics even when the delivery
mechanism reports failure; the CLI transport therefore keeps stdout, stderr,
and exit status when `gh` exits non-zero. The other outcomes distinguish a
request that never ran from one whose effect may be unknown. Reads, ordinary
edits, and non-idempotent creation can then keep their different failure and
effect-certainty rules. The CLI transport owns `gh` argument encoding and maps
the port's target host onto `gh` authentication and routing; those details do
not enter typed GraphQL operations.

The port exchanges GraphQL wire values rather than typed GitHub results. A
request carries its target host, operation name, document, and JSON variables;
an exchange retains the raw response body and transport evidence. Typed
GraphQL operations above the port decode the response envelope and map it into
tk's domain types. The envelope retains each error's optional response path so
an operation with deterministic aliases can map item-specific failures back to
its input; an absent or unknown path remains a request-wide failure. The CLI
adapter sends the standard JSON request on stdin to `gh api graphql --input -`
with an explicit JSON content type; a future Rust adapter can send the same
body over HTTP. The CLI adapter parses the complete stdout response even when
`gh` reports GraphQL errors with a non-zero exit.

Tests follow the same seam. Typed-operation tests use a fake GraphQL transport
for response envelopes, pagination, identity checks, and effect certainty. A
focused CLI-transport suite uses `FakeRunner` for exact argv, host routing,
request encoding, and subprocess evidence. The two suites do not duplicate
the same semantic cases.

The first migration moves every existing direct GraphQL path onto this port:
exact-key Backend Pull, Bug creation, Issue Type pagination, and Label
pagination. Native `gh issue` and `gh repo` operations remain unchanged. Each
GraphQL query is a private typed operation that binds its document and
variables to an associated response type; callers cannot choose an unrelated
decoder for a request.
