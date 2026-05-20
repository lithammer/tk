#!/bin/sh
# Install tk via curl|bash, per ADR 0013.
#
# Usage:
#     curl -fsSL https://raw.githubusercontent.com/lithammer/ticket/main/scripts/install.sh | sh
#
# Environment variables:
#     TK_VERSION       Release tag to install (e.g. v0.0.1). Defaults to latest.
#     TK_INSTALL_DIR   Destination directory. Defaults to /usr/local/bin (root)
#                      or $HOME/.local/bin (non-root).
#     TK_LINUX_ABI     Linux ABI variant: "musl" (default, fully static) or
#                      "gnu" (glibc-dynamic, x86_64 only). See ADR 0012.
#
# POSIX shell only -- runs under /bin/sh on macOS, Alpine, Debian, etc. No
# bashisms. No `set -o pipefail` (not POSIX).
set -eu

REPO="lithammer/ticket"

# --- 1. Detect OS and arch -------------------------------------------------

OS="$(uname -s)"
ARCH="$(uname -m)"

# Normalize arch aliases.
case "$ARCH" in
    amd64) ARCH="x86_64" ;;
    arm64)
        # On Darwin, uname -m already prints arm64; normalize to aarch64 for
        # Linux where common aliases vary.
        if [ "$OS" = "Linux" ]; then
            ARCH="aarch64"
        fi
        ;;
esac

# Linux ABI selection. Default to musl (fully static, runs anywhere).
TK_LINUX_ABI="${TK_LINUX_ABI:-musl}"
case "$TK_LINUX_ABI" in
    musl|gnu) ;;
    *)
        echo "tk: TK_LINUX_ABI must be 'musl' or 'gnu' (got: $TK_LINUX_ABI)" >&2
        exit 1
        ;;
esac

TRIPLE=""
case "$OS" in
    Linux)
        case "$ARCH" in
            x86_64)
                TRIPLE="x86_64-linux-${TK_LINUX_ABI}"
                ;;
            aarch64)
                if [ "$TK_LINUX_ABI" = "gnu" ]; then
                    echo "tk: aarch64-linux-gnu is not a supported release target." >&2
                    echo "    Only aarch64-linux-musl is published for aarch64 Linux." >&2
                    exit 1
                fi
                TRIPLE="aarch64-linux-musl"
                ;;
        esac
        ;;
    Darwin)
        case "$ARCH" in
            arm64|aarch64)
                TRIPLE="aarch64-macos"
                ;;
            x86_64)
                echo "tk: x86_64-macos is not a supported release target." >&2
                echo "    Build from source: https://github.com/lithammer/ticket#source-build" >&2
                exit 1
                ;;
        esac
        ;;
esac

if [ -z "$TRIPLE" ]; then
    echo "tk: unsupported platform: $OS $ARCH" >&2
    echo "    Supported: Linux x86_64, Linux aarch64, Darwin arm64" >&2
    echo "    Build from source: https://github.com/lithammer/ticket#source-build" >&2
    exit 1
fi

# --- 3. Determine install destination --------------------------------------

if [ -n "${TK_INSTALL_DIR:-}" ]; then
    DEST_DIR="$TK_INSTALL_DIR"
elif [ "$(id -u)" = "0" ]; then
    DEST_DIR="/usr/local/bin"
else
    DEST_DIR="${HOME}/.local/bin"
fi

# --- 4 & 5. Build asset URL ------------------------------------------------

ASSET="tk-${TRIPLE}"
if [ -n "${TK_VERSION:-}" ]; then
    URL="https://github.com/${REPO}/releases/download/${TK_VERSION}/${ASSET}"
else
    URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
fi

# --- 6. Preflight write check (BEFORE any download) ------------------------

if [ ! -d "$DEST_DIR" ]; then
    if ! mkdir -p "$DEST_DIR"; then
        echo "tk: cannot create $DEST_DIR" >&2
        echo "    re-run with sudo, or set TK_INSTALL_DIR=<writable-path>" >&2
        exit 1
    fi
fi

WRITE_TEST="$DEST_DIR/.tk.write_test.$$"
if ! touch "$WRITE_TEST" 2>/dev/null; then
    echo "tk: cannot write to $DEST_DIR" >&2
    echo "    re-run with sudo, or set TK_INSTALL_DIR=<writable-path>" >&2
    exit 1
fi
rm -f "$WRITE_TEST"

# --- 7. Download to staging (same filesystem as final destination) ---------

STAGING="$DEST_DIR/.tk.tmp.$$"
# shellcheck disable=SC2064
# Expand STAGING at trap-definition time so cleanup uses the path we just set.
trap "rm -f '$STAGING'" EXIT

DOWNLOAD_OK=0
if command -v curl >/dev/null 2>&1; then
    if curl -fsSL --output "$STAGING" "$URL"; then
        DOWNLOAD_OK=1
    fi
elif command -v wget >/dev/null 2>&1; then
    if wget -qO "$STAGING" "$URL"; then
        DOWNLOAD_OK=1
    fi
else
    echo "tk: neither curl nor wget is available" >&2
    exit 1
fi

if [ "$DOWNLOAD_OK" -ne 1 ]; then
    echo "tk: failed to download $URL" >&2
    exit 1
fi

# --- 8. Smoke verification before placing ----------------------------------

chmod +x "$STAGING"

if ! VERSION_OUTPUT="$("$STAGING" --version 2>&1)" || [ -z "$VERSION_OUTPUT" ]; then
    echo "tk: downloaded binary failed verification ($STAGING --version)" >&2
    exit 1
fi
NEW_VERSION="$VERSION_OUTPUT"

# --- 9. Detect prior install ----------------------------------------------

OLD_VERSION=""
if [ -x "$DEST_DIR/tk" ]; then
    OLD_VERSION="$("$DEST_DIR/tk" --version 2>/dev/null || true)"
fi

# --- 10. Atomic placement --------------------------------------------------

mv "$STAGING" "$DEST_DIR/tk"
# Disarm the cleanup trap: STAGING no longer exists at that path.
trap - EXIT

# --- 11. Install manpage (warn-and-continue) -------------------------------

if ! "$DEST_DIR/tk" manpage --install >&2; then
    echo "warning: failed to install manpage; binary install succeeded" >&2
fi

# --- 12. Render success line ----------------------------------------------

if [ -z "$OLD_VERSION" ]; then
    echo "Installed tk $NEW_VERSION at $DEST_DIR/tk"
elif [ "$OLD_VERSION" = "$NEW_VERSION" ]; then
    echo "Reinstalled tk $NEW_VERSION at $DEST_DIR/tk"
else
    echo "Upgraded tk: $OLD_VERSION -> $NEW_VERSION at $DEST_DIR/tk"
fi

# --- 13. PATH advice (stderr, informational) -------------------------------

case ":${PATH:-}:" in
    *":$DEST_DIR:"*) ;;
    *)
        echo "tk: $DEST_DIR is not on your PATH" >&2
        echo "    add it to your shell startup:" >&2
        echo "    export PATH=\"$DEST_DIR:\$PATH\"" >&2
        ;;
esac
