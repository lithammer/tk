# Immutable releases via draft-publish with a tag-carried changelog

tk enables GitHub immutable releases, where a published Release freezes
its tag and assets so distributed binaries cannot be altered after the
fact. Because immutability locks assets at publish time, the previous
flow — publish a Release, then upload binaries to it — no longer works.
The release is now assembled as a *draft* and published by a human as
the last step.

The annotated git tag is the whole release spec. The tag *name*
(`vX.Y.Z`) drives the shipped version: the workflow injects it as
`TK_VERSION`, which `build.rs` bakes into `TK_VERSION_STRING`, so
`tk --version` reports the tag without any `crates/tk/Cargo.toml` bump.
The manifest `version` stays a frozen `0.0.0` placeholder and is only
the local-build fallback. The tag *message* carries the changelog as
plain text under the `NEW` / `IMPROVED` / `FIXED` labels `tk-changelog`
already emits; the release workflow promotes those three known labels to
`####` markdown headings and uses the result as the Release body. Local
release authoring is therefore `git tag -a vX.Y.Z -F <notes>` followed
by `git push origin vX.Y.Z`; pushing the `v*` tag fires the workflow,
which builds and smokes every triple (ADR-0011) and assembles a draft
Release carrying the smoke-passing binaries, `install.sh`, `install.ps1`
(see below), and the rendered notes. The draft is pinned to the tagged
commit (`--target <sha>`) so commits landing on `main` before publish
never move where the published tag points.

The install scripts ship as Release assets, and the install command in
the README resolves them from `releases/latest/download/install.{sh,ps1}`
instead of `raw.githubusercontent.com/.../main/scripts/` (amending
ADR-0013). The bootstrap script is then pinned to a published, immutable
release rather than a mutable branch path.

## Considered Options

`workflow_dispatch` with the version as a form input was considered:
its git tag is born only at publish, so an aborted release leaves
nothing behind. It was rejected because the changelog cannot ride a tag
that does not yet exist, splitting release authoring across a dispatch
form and a separate notes step. Carrying the changelog as markdown
directly in the tag message was rejected because tag messages are
plain-text by convention (`git tag -n`, `git show`); rendering to
markdown at the workflow boundary keeps the tag readable in git while
the Release page still gets headings.

Keeping `crates/tk/Cargo.toml` as the version source — bumping and
committing it per release, guarded by a workflow check against the tag —
was rejected: the bump commit is pure ceremony for a crate that is not
published to a registry, and injecting `TK_VERSION` from the tag removes
both the edit and the entire class of tag-vs-manifest drift the guard
existed to catch.

## Consequences

The release workflow triggers on `push: tags: v*` rather than
`release: created`; the version-drift guard is gone (the tag is the only
source); and the asset step creates a draft instead of uploading to an
existing Release. Re-runs for the same tag clear a prior *draft* and
re-assemble, but refuse to touch an already-published (immutable)
Release. `tk-version-bump` no longer edits files — it only recommends
the next version — and `tk-release` no longer creates the Release; it
pushes the tag and reports the run, leaving publish as a human gate.
A dangling tag is the cost of aborting after the tag is pushed: recovery
is `git push --delete origin vX.Y.Z`. Enabling the immutable-releases
repository setting is a one-time manual step done after the first
draft-publish run is verified. The `releases/latest/download/` install
URL 404s until a published release carries the scripts, so the README
URL switch must land with or after the first new-scheme release.
