#!/usr/bin/env bash
# Release smoke: confirm a built tk binary loads, links its SQLite copy,
# applies migrations, embeds prime.md, and round-trips a Ticket through the
# Repository Store. Used by .github/workflows/release.yml per-platform smoke
# jobs and reusable locally:
#
#     scripts/smoke.sh ./dist/tk-x86_64-unknown-linux-musl
#
# See ADR 0011 for the broader release strategy.
set -euo pipefail

TK="${1:?usage: $0 <path-to-tk-binary>}"

if [[ ! -f "$TK" ]]; then
    echo "smoke: binary not found at $TK" >&2
    exit 1
fi

# Downloaded artifacts often lose the exec bit; restore it for ELF/Mach-O.
# PE binaries do not need this on Windows.
case "$TK" in
    *.exe) ;;
    *) chmod +x "$TK" ;;
esac

# Resolve to an absolute path before cd so the relative argument keeps working
# inside the temp workspace.
case "$TK" in
    /*) ;;
    *) TK="$PWD/$TK" ;;
esac

# Verify the release binary embeds man/tk.1 byte-for-byte. This must run
# before the `cd "$WORK"` below because man/tk.1 lives in the repo root.
MANPAGE_SMOKE="/tmp/tk.1.smoke.$$"
"$TK" manpage > "$MANPAGE_SMOKE"
diff "$MANPAGE_SMOKE" man/tk.1
rm -f "$MANPAGE_SMOKE"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

git init --quiet
git config user.email smoke@example.com
git config user.name "Smoke Test"

"$TK" init
"$TK" prime > /dev/null
"$TK" add -m "smoke ticket"
"$TK" list | grep -F "smoke ticket"

echo "smoke: ok"
