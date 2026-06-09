---
name: tk-changelog
description: >-
  Generate user-facing release notes from git commits. Filters out internal
  noise, rewrites developer commits as user-facing language, and presents a
  draft for review.
model: sonnet
metadata:
  short-description: Draft user-facing release notes from commits.
---

# tk Changelog

## Overview

Translate developer-facing git commits into user-facing release notes for a
GitHub Release. Filters out internal changes, rewrites the rest in language
that makes sense to people running the `tk` CLI, and presents a draft for
review.

## Workflow

### 1. Gather Commits

Determine the commit range. Default to the latest release tag through HEAD:

```bash
LATEST_TAG=$(git tag -l 'v[0-9]*.[0-9]*.[0-9]*' \
  --merged HEAD --sort=-v:refname | head -n 1)
git log "$LATEST_TAG"..HEAD --format="%h %s%n%n%b"
```

For the inaugural release there is no `v*` tag yet — only `zig-final`, which
marks the end of the Zig implementation. Use the Rust-rewrite boundary as the
baseline so the notes cover the Rust port rather than 200+ Zig-era commits:

```bash
git log zig-final..HEAD --format="%h %s%n%n%b"
```

If a specific range is provided (e.g. between two tags), use that instead.

Use the full commit body for context. The subject line alone rarely captures
enough detail to write good user-facing notes.

### 2. Filter

Drop commits that are invisible to people running `tk`:

- Version bump commits ("Bump version to ...")
- Formatting, clippy, and lint fixes
- CI/workflow changes
- Agent skill, ADR, or other documentation-only changes
- Refactors with no user-visible effect
- Test-only changes
- Merge commits

When in doubt, keep the commit and let the user remove it in review.

### 3. Rewrite

Rewrite each remaining commit as a user-facing note:

- Use plain language, not implementation jargon.
- Focus on what changed for the user, not how it was implemented.
- Use tk's domain vocabulary where it is the user-facing term (e.g. Scope,
  Display ID, Remote), not internal type names.
- Consolidate related commits into a single note when they describe
  incremental steps toward one visible change.
- Keep each note to one sentence.

**Examples:**

| Commit message | Release note |
| --- | --- |
| Record a closing reason when marking items done | `tk done` now records why an item was closed |
| Shorten and colour the tk list Scope hint | `tk list` shows a shorter, colourised Scope hint |
| Auto-apply forward migrations at the Store open chokepoint | The Repository Store now upgrades its schema automatically on open |
| Consolidate Scope resolution into a single scope::resolve helper | *(filtered out — internal refactor)* |
| Sanitise terminal output per UTF-8 char, not per byte | *(filtered out unless user-visible — judge from the body)* |

### 4. Categorize

Group notes under these three section labels (omit empty sections):

- **NEW** — new commands, flags, or behaviour
- **IMPROVED** — enhancements to existing behaviour
- **FIXED** — bug fixes

Emit the labels as **plain text**, uppercase, each on its own line, followed by
a blank line and `-` bullets. Do not write markdown headings (`#`/`##`)
yourself — the release pipeline turns these labels into headings.

```
NEW

- ...
- ...

IMPROVED

- ...

FIXED

- ...
```

### 5. Present Draft

Show the draft release notes to the user via `AskUserQuestion`. The user may
edit, reorder, add, or remove entries before confirming. The confirmed block
becomes the body of the annotated release tag that `tk-release` creates.

## Standalone Usage

The skill can be invoked on its own to preview what release notes would look
like without starting a release:

> "Show me what the changelog looks like since v0.1.0"

In this case, stop after presenting the draft. Do not create any tags, commits,
or GitHub releases.

## Boundaries

- This skill produces release notes only. It does not bump versions, push, or
  create the GitHub Release. For the full release flow, use `tk-release`.
