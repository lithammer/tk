---
name: tk-version-bump
description: >-
  Recommend the next tk release version. Analyzes commits since the last
  release tag, recommends a semver bump level, and asks the user to confirm the
  version to tag.
model: sonnet
metadata:
  short-description: Analyze commits and recommend the next tk version.
---

# tk Version Bump

## Overview

Analyze commits since the last release tag and recommend a semantic version
bump (major, minor, or patch). This skill is a **pure advisor**: it outputs the
agreed version string for the caller to use. It does not edit files, commit, or
tag — `tk-release` turns the version into the `vNEXT` tag.

## Prerequisites

- `git` on PATH.
- Run from a tk checkout (no clean-tree or branch requirement — this skill only
  reads history).

## Workflow

### 1. Gather Context

```bash
# Latest release tag. Empty before the first release: only `zig-final` exists,
# which marks the end of the Zig implementation (the Rust-rewrite boundary).
git tag -l 'v[0-9]*.[0-9]*.[0-9]*' --merged HEAD --sort=-v:refname | head -n 1

# Commits since the last release tag (replace TAG with the result above). For
# the inaugural release, use the Rust-rewrite boundary instead: zig-final..HEAD
git log TAG..HEAD --oneline
```

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

**Pre-1.0 note:** tk is below 1.0. While below 1.0, minor bumps are the primary
vehicle for new features and major is reserved for the 1.0 milestone. Before the
first `v*` tag exists, establish the inaugural version with the user (`0.1.0` is
the natural first cut) rather than computing a bump from a previous tag.

### 3. Present Recommendation

Use `AskUserQuestion` to present:

- The list of commits in the range.
- Your recommended bump level with reasoning.
- All three options (major, minor, patch) with the calculated next version for
  each.

### 4. Report the Agreed Version

Output the confirmed version string (e.g. `0.1.0`) for the caller. Do not edit
`Cargo.toml`, commit, or tag — `tk-release` turns this version into the
annotated `vNEXT` tag.

## Boundaries

- This skill is read-only and produces a version recommendation. It does not
  bump files, push, or create a release. For the full flow, use `tk-release`.
