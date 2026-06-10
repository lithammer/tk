---
name: tk-release
description: >-
  Orchestrate a tk release: preflight checks, local build and test, version
  recommendation, and an annotated changelog tag that triggers the draft
  Release. Delegates version recommendation to tk-version-bump and release notes
  to tk-changelog.
metadata:
  short-description: Tag a tk release; CI assembles a draft to publish.
---

# tk Release

## Overview

End-to-end release workflow up to the tag. Validates preconditions, runs the
local checks CI gates on, and pushes an annotated `vX.Y.Z` tag whose message
carries the changelog.

Pushing the tag triggers `.github/workflows/release.yml`, which cross-compiles
all five supported triples, smoke-tests each on a native runner, and assembles a
**draft** Release carrying the smoke-passing binaries, the install scripts, and
the changelog (rendered from the tag message to markdown). This skill does
**not** build artifacts and does **not** publish the Release — the local build
is a sanity gate, and publishing the draft is a deliberate human step.

There is no version bump commit: the build injects the tag name as `TK_VERSION`
so `tk --version` reports it, and `crates/tk/Cargo.toml` stays at its `0.0.0`
placeholder.

## Prerequisites

- `cargo` on PATH (the pinned toolchain in `rust-toolchain.toml` installs via
  rustup on first use).
- `jq` on PATH (reads crate metadata).
- `gh` (`brew install gh`), authenticated (`gh auth status`) — used to watch the
  run and find the draft.
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

# Up to date with origin/main. The tag is created on HEAD; it must already be
# pushed so the tag points at a commit on origin.
git fetch origin main
test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"
```

### 2. Build and Test

Run the same checks CI gates on (`.github/workflows/ci.yml`), plus a release
build to catch profile-specific failures. Abort on any failure. These are
sanity gates — the shipped binaries are cross-compiled by CI after the tag is
pushed, not here.

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
cargo build --release

# Scripts CI shellchecks (skip if shellcheck is not installed locally)
shellcheck -s sh scripts/install.sh
shellcheck scripts/smoke.sh
```

### 3. Decide the Version

Read and follow `.agents/skills/tk-version-bump/SKILL.md`. It analyzes commits
since the last tag and returns the agreed version. It no longer edits files —
capture the version string:

```bash
NEXT=0.1.0   # whatever tk-version-bump returns
```

Verify the tag does not already exist (locally or on origin):

```bash
git tag -l "v${NEXT}"                       # must be empty
git ls-remote --tags origin "refs/tags/v${NEXT}"  # must be empty
```

If the tag exists, stop — a release for that version was already started.

### 4. Generate the Changelog

Read and follow `.agents/skills/tk-changelog/SKILL.md` to produce user-facing
notes for the commits since the previous tag (or since `zig-final` for the
inaugural release). It returns the reviewed plain-text `NEW`/`IMPROVED`/`FIXED`
block.

### 5. Tag and Push

Compose the annotated tag message — a `tk vNEXT` subject line, a blank line,
then the changelog block — and create the tag with `-F` so the body is exactly
the reviewed text. The subject keeps `git tag -n` readable and lets the workflow
extract the body cleanly.

```bash
{
  printf 'tk v%s\n\n' "$NEXT"
  cat <<'NOTES'
NEW

- ...

IMPROVED

- ...

FIXED

- ...
NOTES
} | git tag -a "v${NEXT}" -F -

git push origin "v${NEXT}"
```

No commit is pushed — only the tag. The push fires the Release workflow.

If `git push` fails, report the error. The tag exists locally; the user can
delete it (`git tag -d v${NEXT}`) and retry, or push manually.

### 6. Report

Display:

- The version being released (e.g. v0.1.0).
- That the Release workflow is building, smoking, and assembling a **draft**
  Release. Watch it with `gh run watch` or the Actions tab.
- That the result is a **draft** the user must review and publish manually —
  publishing is what locks immutability. Once the run finishes:

  ```bash
  gh release view "v${NEXT}" --web        # review assets + rendered notes
  gh release edit "v${NEXT}" --draft=false # publish (or use the GitHub UI)
  ```

- That the draft should carry all five binaries plus `install.sh` /
  `install.ps1`; a missing triple means its smoke check failed — investigate
  before publishing rather than shipping a partial release.

## Boundaries

- For the version recommendation, follow `tk-version-bump`.
- For release notes generation, follow `tk-changelog`.
- This skill does not build binaries or publish the Release — `release.yml`
  builds and assembles the draft after the tag is pushed, and a human publishes
  it.
