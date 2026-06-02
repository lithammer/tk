---
name: tk-version-bump
description: >-
  Bump the tk crate version in Cargo.toml. Analyzes commits since the last
  release tag, recommends a semver bump level, and asks the user to confirm
  before editing and committing.
model: sonnet
metadata:
  short-description: Analyze commits and bump the tk crate version.
---

# tk Version Bump

## Overview

Analyze commits since the last release tag, recommend a semantic version bump
(major, minor, or patch), and execute the bump after user confirmation.

The version lives in `crates/tk/Cargo.toml`. `build.rs` embeds it into the
binary as `tk v<version> (<triple>)`, and the Release workflow guards that
`v<crate-version>` equals the release tag (`.github/workflows/release.yml`).
This bump is therefore the source of truth for the released version.

This skill bumps the version and commits — it does **not** create a git tag or
push. The GitHub Release (see `tk-release`) creates the `v<version>` tag and
triggers the binary build.

## Prerequisites

- `cargo` on PATH (reads crate metadata and syncs `Cargo.lock`).
- `jq` on PATH (parses `cargo metadata`).
- Working tree must be clean.
- Must be on the `main` branch.

## Workflow

### 1. Gather Context

```bash
# Latest release tag. Empty before the first release: only `zig-final` exists,
# which marks the end of the Zig implementation (the Rust-rewrite boundary).
git tag -l 'v[0-9]*.[0-9]*.[0-9]*' --merged HEAD --sort=-v:refname | head -n 1

# Current crate version (same query the Release workflow uses for its guard)
cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "tk") | .version'

# Commits since the last release tag (replace TAG with the result above). For
# the inaugural release, use the Rust-rewrite boundary instead: zig-final..HEAD
git log TAG..HEAD --oneline
```

If a `v*` release tag exists, verify its version matches the crate version. If
they differ, stop and tell the user to resolve the drift first — the Release
workflow will reject a mismatched tag.

If there are no commits in the range, stop and tell the user there is nothing
to release.

### 2. Analyze Commits

| Level     | When to use                                                                       |
| --------- | --------------------------------------------------------------------------------- |
| **major** | Breaking CLI/flag/exit-code changes, removed commands, incompatible Store schema  |
| **minor** | New commands or flags, new user-visible behaviour, significant enhancements       |
| **patch** | Bug fixes, refactoring, tooling, documentation, cosmetic tweaks                   |

The highest-impact commit sets the floor. If any commit warrants a minor bump,
recommend at least minor even if the rest are patches.

**Pre-1.0 note:** tk is below 1.0 (it currently reports `v0.0.0`). While below
1.0, minor bumps are the primary vehicle for new features and major is reserved
for the 1.0 milestone. The first release establishes the inaugural version
rather than bumping from `0.0.0` — agree the starting version with the user
(`0.1.0` is the natural first cut).

### 3. Present Recommendation

Use `AskUserQuestion` to present:

- The list of commits in the range.
- Your recommended bump level with reasoning.
- All three options (major, minor, patch) with the calculated next version for
  each.

### 4. Execute the Bump

After the user confirms a version:

```bash
# Guard: clean tree, on main
git diff --quiet && git diff --cached --quiet
test "$(git rev-parse --abbrev-ref HEAD)" = "main"
```

Then set the version (replace NEXT with the calculated version):

- Edit the `version = "..."` line under `[package]` in `crates/tk/Cargo.toml`
  to `NEXT`.
- Run `cargo build`. The workspace `Cargo.lock` pins tk's own version, so the
  build rewrites the lock; `cargo build` also confirms the crate still compiles
  at the new version.

```bash
# Commit Cargo.toml and the refreshed lock together. No tag — tk-release / the
# GitHub Release creates the v-tag.
git add crates/tk/Cargo.toml Cargo.lock
git commit -m "Bump version to NEXT"
```

### 5. Report

After committing, display:

- The version change (e.g. 0.0.0 -> 0.1.0).
- That no tag was created: the GitHub Release (`tk-release`) tags `vNEXT` on
  `main`, and that tag-creation event triggers the cross-compile, smoke, and
  binary-attach pipeline.

## Boundaries

- Commit messages follow the repo convention: imperative mood, under 72
  characters, no trailing period (see AGENTS.md / CLAUDE.md).
- This skill does not push or create a release. For the full flow, use
  `tk-release`.
