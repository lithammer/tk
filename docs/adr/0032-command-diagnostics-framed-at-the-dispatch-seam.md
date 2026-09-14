# Command diagnostics are framed once at the dispatch seam

Command handlers stop writing their own stderr. Each `run()` returns
`Result<Exit, CommandError>`; `cli::run_argv` is the single seam that renders
the `tk <command>:` frame, applies stderr styling, and maps the failure to its
exit code. This replaces the per-command "handler writes its own diagnostic"
shape ADR-0018 recorded, removes the per-command `const COMMAND` and the ~44
inline `tk <command>:` prefix literals, and makes the stderr colour policy
(gh-64) a change at the seam instead of a 16-file scatter.

## The grep-able unit is the message body, not the full line

ADR-0017 required every stable user-visible line to be findable by exact-string
search, and rejected an `f(command_name)` builder because a synthesized line
like `tk start: Repository Store is busy…` "would exist nowhere in the source".
The seam *is* that builder, so this ADR narrows 0017's rule rather than keeping
it intact:

- The **message body** is the verbatim, grep-able literal. It lives in the typed
  error's `#[error("…")]` attribute (`thiserror`), which is already where the
  Rust port put it — the Zig-era `messages` module 0017 described no longer
  exists.
- The **`tk <command>:` frame** is synthesized once at the seam from the
  dispatched `Command` variant. It is not grep-able as part of a full line, and
  that is accepted.

This is not a new loss. `resolver::render_storage_error` already synthesized
`tk {command}: Repository Store is busy; retry the command` for every command
that called it; `grep 'tk show: Repository Store is busy'` already returned
nothing in the Rust tree. The seam only generalizes the established pattern: a
body literal (`Repository Store is busy; retry the command`) stays greppable,
the framed full line does not.

Invariant: **no typed error's `Display` may contain the `tk <command>:`
prefix.** `self_update::QueryError` currently violates this (its variants bake
in `tk self-update: …`); its prefixes are stripped as part of the migration so
the seam does not double-prefix it.

## CommandError

```rust
pub enum CommandError {
    /// Operation failed (exit 1). May forward a subprocess's own stderr
    /// verbatim as a separate line after tk's frame.
    Failure { body: String, tail: Option<Vec<u8>> },
    /// Command invoked incorrectly (exit 2). Detected during argument
    /// validation, before any subprocess runs — so it never forwards a tail.
    Usage { body: String },
}
```

- **An enum, not a struct with a separate exit-disposition field.** The exit
  code is derived by `match` (`Failure → Exit::Failure`, `Usage → Exit::Usage`),
  so the mapping is self-evident and there is no separate discriminant type to
  name. A dedicated `ExitClass`/`FailureKind` enum was rejected: "Class"
  (`item_class`: Ticket vs Epic) and "Kind" (`ticket_kind`: task vs bug) are
  both schema-anchored domain terms, and reusing either for CLI plumbing is the
  vocabulary fraying ADR-0018 warns against.
- **The disposition is chosen at construction, not derived from the error
  type.** `--priority cannot be set on an Epic` (Usage) and `parent '…' is not
  an Epic` (Failure) are sibling validation paths, and most Usage errors are not
  typed errors at all. `Exit::Ok` / `Exit::NoMatch` never become a
  `CommandError`; they flow through the `Ok(Exit)` arm, which keeps `tk grep`'s
  empty-stderr "no match" contract automatic.
- **`tail: Option<Vec<u8>>` is bytes**, because a subprocess's stderr is not
  guaranteed UTF-8, and only on `Failure`, because usage errors are caught
  before anything is spawned. The seam styles its own frame and writes the tail
  verbatim — the forwarded line is the subprocess's voice, not tk's. Only
  `self_update` (smoke-check / manpage-install failures) forwards a tail today;
  tk-34 / tk-35 (gh / acli / Jira adapters) will too.
- **`body: String`, not `Box<dyn Display>` or a dynamic reporter.** The
  per-layer typed errors keep their `#[error]` literals as the grep target;
  `CommandError` is a transport that formats the body once at the boundary. This
  is not the `anyhow`/`eyre` dynamic-reporting ADR-0018 declined — there is no
  context chain, downcast, or backtrace.

## Sync's command names

Sync returns a local wrapper carrying a command name and `CommandError`.
Dispatch passes both to the existing `finish()` function. This keeps sync's
three names out of the shared error type, at the cost of a distinct return
type for sync.

The name follows the failure, not just the arguments: Sync Log errors use
`sync log`; Sync Skip validation errors use `sync --skip`, while its storage
errors retain `sync`. Other sync errors also use `sync`. Diagnostic bytes
and exit codes remain unchanged.

Sync releases its workflow lock before dispatch writes an error. Backend
work and Store writes have ended by then; diagnostic output needs no lock.
The handler reports a committed Sync Skip before starting Backend work.
A stopped sync prints its summary to stdout and returns
`Ok(Exit::Failure)`, with no stderr diagnostic. Multiline recovery guidance
belongs in the error body because tk writes it; the tail holds only
subprocess output.

## Error-prefix styling

gh-64 gives `CommandError::Failure` and `CommandError::Usage` the same
red+bold `tk <command>:` prefix through `deps.styler.for_stderr()`. The
style includes the colon and ends before the following space. The body,
including any later lines, and the forwarded subprocess tail retain their
bytes. `Exit::NoMatch` remains silent.

This style belongs to the shared command-error renderer. Parser errors,
internal failures, successful-command warnings, and informational stderr
output are outside its scope, even when their text contains a similar
prefix. gh-64 adds no warning style.

The existing per-stream color policy in ADR-0014 governs emission. Checks
cover both error variants, independent stdout/stderr TTY states, forced
color, `NO_COLOR` precedence, unchanged subprocess bytes, and silent
no-match results. Plain error-path scenario snapshots stay unchanged.

## Migration

Incremental, command-by-command — each dispatch arm can independently return the
legacy `Exit` or the new `Result<Exit, CommandError>` with no shared
scaffolding, mirroring ADR-0018's "pin the idioms on one slice, then fan out":

1. Refactor the shared render helpers (`resolver::render_open_error` /
   `render_storage_error`, `discovery::render_failure`) to *produce* a
   `CommandError::Failure` instead of writing stderr.
2. Convert one representative command end-to-end (`show`: open + storage +
   not-found, no Usage, no tail) to pin the seam render path.
3. Fan out the resolver-backed commands, then `update` / `grep` (which add
   `Usage`), then `self_update` last (the only `tail` case, plus the
   `QueryError` prefix-stripping fixup).

The `insta` scenario snapshots guard byte-for-byte stderr at every step.
gh-64 lands after tk-128 brings sync's remaining diagnostics through the
dispatch seam, completing the migration begun in tk-127. Styling a subset
of commands' error prefixes would be inconsistent. Unforced non-TTY output
stays byte-identical when styling lands.

`Exit::Internal` (exit 3, main's catch-all for I/O faults) is orthogonal and
untouched.

## Considered Options

- **Keep ADR-0017's full-line grep goal.** Rejected: it forbids centralizing the
  prefix at all, which sinks the seam and leaves the colour policy as a 44-site
  scatter the Zig implementation already declined as not worth it.
- **Struct + separate disposition enum** (`CommandError { disposition, body,
  tail }`). Rejected: needs a name for the disposition type, and every
  domain-flavoured candidate frays loaded vocabulary; the enum form also makes
  "Usage never forwards a tail" unrepresentable-by-construction rather than a
  convention.
