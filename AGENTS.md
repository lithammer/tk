# Agent Notes

- Read [README.md](./README.md) for the project overview.
- Read [GLOSSARY.md](./GLOSSARY.md) — domain language and model — before changing
  domain language.
- Read [ARCHITECTURE.md](./ARCHITECTURE.md) — module map, boundaries, and
  Repository Store invariants — before changing module boundaries or those
  invariants.
- Read [docs/adr/](./docs/adr/) — recorded design decisions — before revisiting
  them.

## Pre-v1 Development

tk is pre-v1. Choose the best design, even when it breaks existing CLI
behavior, APIs, or recorded decisions. Update callers and affected docs
in the same change; remove superseded code. The exception is live data:
changes that make an existing Repository Store incompatible must provide
a migration path.

## The Live Repository Store

Run `tk` against the user's Repository Store only with a binary built from
`main`. A branch build that adds a migration upgrades the Store on first open,
and every older binary then refuses it (ADR-0024). Exercise branch builds
against a scratch `XDG_DATA_HOME`.

- `scripts/tk-main <args>` runs a cached `origin/main` build; use it to read
  Tickets and for the live Store.
- `scripts/dev-tk <args>` runs this worktree's build against a scratch Store
  under `target/dev-tk`.

## Evidence About External Tools

tk shells out to external CLIs, and they are open source — `gh` is `cli/cli`
over the `cli/go-gh` API layer.

**Their source is the primary source for how they behave.** A probe observes one
instance; the primary source settles what is possible. Read it at the tag
matching the installed version, and say which of the two a spike, fixture, or
classifier claim rests on. A claim that some behaviour is impossible or
unobservable rests on the primary source.
