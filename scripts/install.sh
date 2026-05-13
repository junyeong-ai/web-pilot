#!/usr/bin/env bash
# WebPilot installer.
#
# Single responsibility: land a verified `webpilot` binary on PATH and
# hand off to `webpilot setup` for everything else (Claude skill, Chrome
# extension, Native Messaging host). The binary owns its own lifecycle —
# update via `webpilot self update`, uninstall via `webpilot uninstall`.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/junyeong-ai/web-pilot/main/scripts/install.sh | bash
#
# Environment:
#   WEBPILOT_VERSION       Pin to a specific tag (e.g. v0.3.0). Default: latest.
#   WEBPILOT_INSTALL_DIR   Install path. Default: $HOME/.local/bin.
#   WEBPILOT_REPO          Override repo (e.g. fork). Default: junyeong-ai/web-pilot.
#   WEBPILOT_NO_SETUP=1    Skip the post-install `webpilot setup` walkthrough.
#
set -euo pipefail

REPO="${WEBPILOT_REPO:-junyeong-ai/web-pilot}"
INSTALL_DIR="${WEBPILOT_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${WEBPILOT_VERSION:-}"

# --- output ------------------------------------------------------------------

if [ -t 2 ]; then C_DIM=$'\033[2m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'; C_RED=$'\033[31m'; C_RST=$'\033[0m'
else            C_DIM=; C_GRN=; C_YEL=; C_RED=; C_RST=
fi
say()  { printf '  %s→%s %s\n' "$C_DIM" "$C_RST" "$*" >&2; }
ok()   { printf '  %s✓%s %s\n' "$C_GRN" "$C_RST" "$*" >&2; }
warn() { printf '  %s!%s %s\n' "$C_YEL" "$C_RST" "$*" >&2; }
die()  { printf '  %s✗%s %s\n' "$C_RED" "$C_RST" "$*" >&2; exit 1; }

require() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }
require curl
require tar

# --- platform ----------------------------------------------------------------

case "$(uname -s)" in
    Linux)  os="unknown-linux-gnu" ;;
    Darwin) os="apple-darwin" ;;
    *)      die "unsupported OS: $(uname -s)" ;;
esac
case "$(uname -m)" in
    x86_64|amd64)   arch="x86_64" ;;
    arm64|aarch64)  arch="aarch64" ;;
    *)              die "unsupported architecture: $(uname -m)" ;;
esac
target="${arch}-${os}"

# --- resolve version ---------------------------------------------------------

if [ -z "$VERSION" ]; then
    say "Resolving latest release"
    redirect=$(curl -fsSLI --connect-timeout 10 -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO/releases/latest") \
        || die "could not reach GitHub releases"
    case "$redirect" in
        */releases/tag/v*)
            VERSION="${redirect##*/releases/tag/v}"
            VERSION="${VERSION%%[/?#]*}"
            ;;
        *)
            die "no published release at github.com/$REPO — pin one with WEBPILOT_VERSION=vX.Y.Z"
            ;;
    esac
fi
VERSION="${VERSION#v}"
[[ "$VERSION" =~ ^[0-9][0-9A-Za-z._+-]*$ ]] || die "invalid version: $VERSION"

# --- download + verify -------------------------------------------------------

archive="webpilot-${VERSION}-${target}.tar.gz"
base="https://github.com/$REPO/releases/download/v${VERSION}"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/webpilot-install.XXXXXX")
staged=""
cleanup() {
    rm -rf "$tmp"
    if [ -n "$staged" ] && [ -f "$staged" ]; then
        rm -f "$staged"
    fi
    return 0  # bash propagates the EXIT trap's last $? as the script's exit status
}
trap cleanup EXIT

say "Downloading $archive"
curl -fsSL --retry 3 --connect-timeout 10 -o "$tmp/$archive" "$base/$archive" \
    || die "download failed: $base/$archive"
curl -fsSL --retry 3 --connect-timeout 10 -o "$tmp/$archive.sha256" "$base/$archive.sha256" \
    || die "checksum download failed: $base/$archive.sha256"

say "Verifying checksum"
if   command -v sha256sum >/dev/null 2>&1; then
    ( cd "$tmp" && sha256sum -c "$archive.sha256" >/dev/null )
elif command -v shasum >/dev/null 2>&1; then
    ( cd "$tmp" && shasum -a 256 -c "$archive.sha256" >/dev/null )
else
    die "no sha256 tool found (need sha256sum or shasum)"
fi

# --- extract + atomic install -----------------------------------------------

tar -xzf "$tmp/$archive" -C "$tmp"
extracted="$tmp/webpilot-${VERSION}-${target}/webpilot"
[ -x "$extracted" ] || die "archive missing webpilot binary"

mkdir -p "$INSTALL_DIR"
staged="$INSTALL_DIR/.webpilot.install.$$"
cp "$extracted" "$staged"
chmod 0755 "$staged"
mv -f "$staged" "$INSTALL_DIR/webpilot"
staged=""  # mv consumed it; tell trap there's nothing to clean

if [ "$os" = "apple-darwin" ]; then
    codesign --force --sign - "$INSTALL_DIR/webpilot" 2>/dev/null || true
fi

ok "Installed webpilot v$VERSION to $INSTALL_DIR/webpilot"

# --- PATH guidance -----------------------------------------------------------

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        warn "$INSTALL_DIR is not on PATH"
        printf '\n    Add to your shell profile (~/.zshrc, ~/.bashrc):\n' >&2
        # shellcheck disable=SC2016  # literal $PATH for the user to copy
        printf '      export PATH="%s:$PATH"\n\n' "$INSTALL_DIR" >&2
        ;;
esac

# --- hand off to `webpilot setup` -------------------------------------------
#
# `curl ... | bash` consumes stdin with the script content, so a binary
# spawned from inside the pipeline cannot read from stdin. We re-route stdin
# from /dev/tty when one is available so `webpilot setup` can prompt the
# user. If neither $1 nor /dev/tty exists, we just print the next step.

if [ "${WEBPILOT_NO_SETUP:-0}" = "1" ]; then
    exit 0
fi

if [ -r /dev/tty ]; then
    printf '\n' >&2
    "$INSTALL_DIR/webpilot" setup < /dev/tty
else
    # shellcheck disable=SC2016  # literal `webpilot setup` is a command to copy
    printf '\n  Next: run `%s` to install the Claude skill and Chrome extension.\n' \
        "webpilot setup" >&2
fi
