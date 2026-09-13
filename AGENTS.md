# Agent Notes

- Read [README.md](./README.md) for the project overview.
- Read [CONTEXT.md](./CONTEXT.md) — domain language and model — before changing
  domain language.
- Read [ARCHITECTURE.md](./ARCHITECTURE.md) — module map, boundaries, and
  Repository Store invariants — before changing module boundaries or those
  invariants.
- Read [docs/adr/](./docs/adr/) — recorded design decisions — before revisiting
  them.
- Read [CODING_STANDARDS.md](./CODING_STANDARDS.md) when reviewing code changes.

## Evidence About External Tools

tk shells out to external CLIs, and they are open source — `gh` is `cli/cli`
over the `cli/go-gh` API layer.

**Their source is the primary source for how they behave.** A probe observes one
instance; the primary source settles what is possible. Read it at the tag
matching the installed version, and say which of the two a spike, fixture, or
classifier claim rests on. A claim that some behaviour is impossible or
unobservable rests on the primary source.
