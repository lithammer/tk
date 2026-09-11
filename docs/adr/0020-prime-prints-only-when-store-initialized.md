# Prime prints only when a Repository Store is initialized

> [ADR-0052](./0052-prime-composes-a-project-aware-briefing.md) defines the
> briefing that Prime builds after opening the Store.

`tk prime` prints its project-aware briefing only when the current directory
has an openable Repository Store; with no openable store it exits 0 with empty
stdout and empty stderr. This inverts the original contract — Prime began as
"no Repository Store precondition; safe for hooks before `tk init`" — because
the agent hook that runs Prime moved from a single repo to a global
`SessionStart` / `PreCompact` hook that fires in every directory. The
hook-safe goal is unchanged; only the mechanism flips, from always-print to
silence outside an initialized repo.

## Considered Options

Detection uses the existing `open_existing` seam and treats **every**
`OpenError` as silent — no store, outside a git repository, git missing, a
foreign database, a future-version store, or a SQLite fault all take the
empty-success path. Keeping genuine faults loud was rejected: Prime is not the
diagnostic surface, every other `tk` command opens the store and reports
corruption or a future-version store the moment it runs, and a global hook that
leaks stderr or a non-zero exit into every session is the exact noise this
change exists to remove. A lighter `git rev-parse` + file-exists probe that
skips `open_existing` was rejected because it would print the briefing for a
foreign or future-version store it cannot actually use.

Silence ends once `open_existing` succeeds. Prime's project-aware briefing
reads current Store state; a failure in those later reads is a command defect,
not evidence that the current directory is unrelated to tk. Prime builds the
whole briefing before writing stdout, so such a failure reports its diagnostic
on stderr, exits nonzero, and leaves stdout empty rather than emitting a
partial briefing or falling back to context-free guidance.

## Consequences

The scenario harness tests briefing format (single trailing newline, header
prefix, no CR bytes) and print-versus-silent behavior through the command.
It covers initialized, git-without-init, outside-git, and unopenable Stores,
as well as read failures after opening.
