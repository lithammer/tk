---
name: tk-release
description: >-
  Orchestrate a full tk release: preflight checks, local build and test,
  version bump, push, and GitHub Release creation. Delegates version bumping to
  tk-version-bump and release notes to tk-changelog.
metadata:
  short-description: Full release workflow from preflight to GitHub Release.
---

# tk Release

## Overview

End-to-end release workflow. Validates preconditions, runs the local checks
that CI gates on, bumps the crate version, pushes `main`, and creates a GitHub
Release.

Creating the Release is what ships binaries. The release tag (`vX.Y.Z`)
triggers `.github/workflows/release.yml`, which cross-compiles all five
supported triples from one Linux host (ADR-0011), smoke-tests each on a native
runner, and attaches the smoke-passing binaries to the Release. This skill does
**not** build release artifacts locally — the local build is a sanity gate
only.

## Prerequisites

- `cargo` on PATH (the pinned toolchain in `rust-toolchain.toml` installs via
  rustup on first use).
- `jq` on PATH (reads crate metadata).
- `gh` (`brew install gh`), authenticated (`gh auth status`).
- `shellcheck` for the script lint (optional locally; CI enforces it).

## Workflow

### 1. Preflight Checks

Abort the release if any check fails.

```bash
# Tools
command -v cargo >/dev/null 2>&1
command -v jq >/dev/null 2>&1
command -v gh >/dev/null 2>&1
gh auth status

# Clean working tree
git diff --quiet && git diff --cached --quiet

# On main
test "$(git rev-parse --abbrev-ref HEAD)" = "main"

# Up to date with origin/main (avoid tagging a stale HEAD)
git fetch origin main
test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"

# Crate version readable
CURRENT=$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "tk") | .version')
test -n "$CURRENT" && test "$CURRENT" != "null"

# Latest release tag matches crate version (no drift). Empty before the first
# release — only `zig-final` exists — in which case skip this check.
LATEST_TAG=$(git tag -l 'v[0-9]*.[0-9]*.[0-9]*' \
  --merged HEAD --sort=-v:refname | head -n 1)
if [ -n "$LATEST_TAG" ]; then
  test "${LATEST_TAG#v}" = "$CURRENT"
fi
```

### 2. Build and Test

Run the same checks CI gates on (`.github/workflows/ci.yml`), plus a release
build to catch profile-specific failures. Abort on any failure. These are
sanity gates — the shipped binaries are cross-compiled by CI after the Release
fires, not here.

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
cargo build --release

# Scripts CI shellchecks (skip if shellcheck is not installed locally)
shellcheck -s sh scripts/install.sh
shellcheck scripts/smoke.sh
```

### 3. Version Bump

Read and follow the workflow in
`.agents/skills/tk-version-bump/SKILL.md`. It handles commit analysis, user
confirmation, the `crates/tk/Cargo.toml` edit, the `Cargo.lock` sync, and the
bump commit.

It does not create a tag — step 5 does, by creating the Release. After the
bump, capture the new version:

```bash
NEXT=$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "tk") | .version')
```

### 4. Push

```bash
git push origin main
```

Push before creating the Release: `gh release create --target main` tags the
current `main` HEAD, and the Release workflow's version guard rejects the tag
unless the tagged commit's crate version equals `vNEXT`.

If the push fails, report the error. Local state is intact and the user can
retry manually.

### 5. Create the GitHub Release

Read and follow the workflow in `.agents/skills/tk-changelog/SKILL.md` to
generate user-facing release notes for the commits since the previous tag (or
since `zig-final` for the inaugural release). The changelog skill handles
filtering, rewriting, and user review.

After the user confirms the notes, create the Release. This creates the
`vNEXT` tag on `main` and fires the build/smoke/attach pipeline:

```bash
gh release create "vNEXT" \
  --target main \
  --title "vNEXT" \
  --notes "..."
```

Do not pass `--draft`: a draft Release does not create the git tag and does not
reliably fire the `release: created` trigger, so binaries would never attach.
Do not pass `--verify-tag`: the tag does not exist yet — `gh release create`
creates it.

If `gh release create` fails, report the error. The bump commit is already
pushed, so the user can create the Release manually on GitHub.

### 6. Report

Display:

- The version change (e.g. 0.0.0 -> 0.1.0).
- The link to the GitHub Release.
- That the Release workflow is now building and attaching binaries; the user
  can watch it with `gh run watch` or on the Actions tab. Binaries appear on
  the Release once their per-triple smoke check passes.

## Boundaries

- For the version bump decision, follow `tk-version-bump`.
- For release notes generation, follow `tk-changelog`.
- This skill does not build or upload release binaries — `release.yml` does
  that on the GitHub runners after the Release fires.
