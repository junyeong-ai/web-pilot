#!/usr/bin/env bash
# WebPilot installer.
#
# Lands a `webpilot` binary on PATH and hands off to `webpilot setup` for the
# Claude skill, Chrome extension, and Native Messaging host. The skill and
# extension are embedded in the binary, so `setup` installs them with zero
# version drift. The binary owns its lifecycle: `webpilot self update`,
# `webpilot uninstall`.
#
# The binary comes from one of two sources, selectable:
#   - prebuilt   download a verified release archive (default; fast).
#   - source     `cargo build` the current checkout (needs Rust + a checkout).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/junyeong-ai/web-pilot/main/scripts/install.sh | bash
#   bash scripts/install.sh                       # from a checkout, prompts source vs prebuilt
#   WEBPILOT_BUILD=source bash scripts/install.sh # force a source build
#
# Environment:
#   WEBPILOT_BUILD         "prebuilt" (default) or "source".
#   WEBPILOT_VERSION       Pin a release tag (prebuilt only). Default: latest.
#   WEBPILOT_INSTALL_DIR   Install path. Default: $HOME/.local/bin.
#   WEBPILOT_REPO          Override repo (e.g. fork). Default: junyeong-ai/web-pilot.
#   WEBPILOT_NO_SETUP=1    Skip the post-install `webpilot setup` walkthrough.
#
set -euo pipefail

REPO="${WEBPILOT_REPO:-junyeong-ai/web-pilot}"
INSTALL_DIR="${WEBPILOT_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${WEBPILOT_VERSION:-}"
BUILD="${WEBPILOT_BUILD:-}"

# --- output ------------------------------------------------------------------

if [ -t 2 ]; then C_DIM=$'\033[2m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'; C_RED=$'\033[31m'; C_RST=$'\033[0m'
else            C_DIM=; C_GRN=; C_YEL=; C_RED=; C_RST=
fi
say()  { printf '  %s→%s %s\n' "$C_DIM" "$C_RST" "$*" >&2; }
ok()   { printf '  %s✓%s %s\n' "$C_GRN" "$C_RST" "$*" >&2; }
warn() { printf '  %s!%s %s\n' "$C_YEL" "$C_RST" "$*" >&2; }
die()  { printf '  %s✗%s %s\n' "$C_RED" "$C_RST" "$*" >&2; exit 1; }

require() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

# Read one line from the controlling terminal even when the script itself is
# piped from curl (stdin is the script). Falls back to the default otherwise.
ask() {
    local prompt="$1" default="$2" reply=""
    if [ -r /dev/tty ]; then
        printf '  %s ' "$prompt" >&2
        read -r reply < /dev/tty || reply=""
    fi
    printf '%s' "${reply:-$default}"
}

# --- locate a checkout (enables source builds) -------------------------------

CHECKOUT=""
src="${BASH_SOURCE[0]:-$0}"
if dir="$(cd "$(dirname "$src")" 2>/dev/null && pwd -P)" && [ -f "$dir/../Cargo.toml" ]; then
    CHECKOUT="$(cd "$dir/.." && pwd -P)"
fi

# --- choose build method -----------------------------------------------------

if [ -z "$BUILD" ]; then
    if [ -n "$CHECKOUT" ] && [ -r /dev/tty ]; then
        printf '\n  Install method:\n    [1] prebuilt binary (fast)\n    [2] build from source\n\n' >&2
        case "$(ask 'Choose [1-2] (default 1):' 1)" in
            2) BUILD="source" ;;
            *) BUILD="prebuilt" ;;
        esac
    else
        BUILD="prebuilt"
    fi
fi

case "$BUILD" in
    prebuilt|source) ;;
    *) die "WEBPILOT_BUILD must be 'prebuilt' or 'source' (got '$BUILD')" ;;
esac
[ "$BUILD" = "source" ] && [ -z "$CHECKOUT" ] && \
    die "source build needs a checkout — clone the repo and run scripts/install.sh from it"

# --- produce a binary at $BIN ------------------------------------------------

BIN=""

build_from_source() {
    require cargo
    say "Building from source ($CHECKOUT)"
    # rust-toolchain.toml pins the compiler; --locked keeps the audited lockfile.
    ( cd "$CHECKOUT" && cargo build --workspace --release --locked ) >&2 \
        || die "cargo build failed"
    BIN="$CHECKOUT/target/release/webpilot"
    [ -x "$BIN" ] || die "build produced no webpilot binary"
    ok "Built webpilot from source"
}

download_prebuilt() {
    require curl
    require tar

    case "$(uname -s)" in
        Linux)  os="unknown-linux-gnu" ;;
        Darwin) os="apple-darwin" ;;
        *)      die "unsupported OS: $(uname -s)" ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64)  arch="x86_64" ;;
        arm64|aarch64) arch="aarch64" ;;
        *)             die "unsupported architecture: $(uname -m)" ;;
    esac
    local target="${arch}-${os}"

    if [ -z "$VERSION" ]; then
        say "Resolving latest release"
        local redirect
        redirect=$(curl -fsSLI --connect-timeout 10 -o /dev/null -w '%{url_effective}' \
            "https://github.com/$REPO/releases/latest") || die "could not reach GitHub releases"
        case "$redirect" in
            */releases/tag/v*)
                VERSION="${redirect##*/releases/tag/v}"; VERSION="${VERSION%%[/?#]*}" ;;
            *)
                die "no published release at github.com/$REPO — pin WEBPILOT_VERSION or use WEBPILOT_BUILD=source" ;;
        esac
    fi
    VERSION="${VERSION#v}"
    [[ "$VERSION" =~ ^[0-9][0-9A-Za-z._+-]*$ ]] || die "invalid version: $VERSION"

    local archive="webpilot-${VERSION}-${target}.tar.gz"
    local base="https://github.com/$REPO/releases/download/v${VERSION}"

    say "Downloading $archive"
    curl -fsSL --retry 3 --connect-timeout 10 -o "$tmp/$archive" "$base/$archive" \
        || die "download failed: $base/$archive"
    curl -fsSL --retry 3 --connect-timeout 10 -o "$tmp/$archive.sha256" "$base/$archive.sha256" \
        || die "checksum download failed: $base/$archive.sha256"

    say "Verifying checksum"
    if   command -v sha256sum >/dev/null 2>&1; then ( cd "$tmp" && sha256sum -c "$archive.sha256" >/dev/null )
    elif command -v shasum    >/dev/null 2>&1; then ( cd "$tmp" && shasum -a 256 -c "$archive.sha256" >/dev/null )
    else die "no sha256 tool found (need sha256sum or shasum)"; fi

    tar -xzf "$tmp/$archive" -C "$tmp"
    BIN="$tmp/webpilot-${VERSION}-${target}/webpilot"
    [ -x "$BIN" ] || die "archive missing webpilot binary"
    ok "Downloaded webpilot v$VERSION"
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/webpilot-install.XXXXXX")
staged=""
cleanup() {
    rm -rf "$tmp"
    [ -n "$staged" ] && [ -f "$staged" ] && rm -f "$staged"
    return 0  # bash propagates the EXIT trap's last $? as the script's exit status
}
trap cleanup EXIT

if [ "$BUILD" = "source" ]; then build_from_source; else download_prebuilt; fi

# --- atomic install ----------------------------------------------------------

mkdir -p "$INSTALL_DIR"
staged="$INSTALL_DIR/.webpilot.install.$$"
cp "$BIN" "$staged"
chmod 0755 "$staged"
mv -f "$staged" "$INSTALL_DIR/webpilot"
staged=""  # mv consumed it; tell trap there's nothing to clean

if [ "$(uname -s)" = "Darwin" ]; then
    codesign --force --sign - "$INSTALL_DIR/webpilot" 2>/dev/null || true
fi

ok "Installed to $INSTALL_DIR/webpilot ($("$INSTALL_DIR/webpilot" --version 2>/dev/null | head -1))"

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

# --- hand off to `webpilot setup` (installs the skill + extension) -----------
#
# `curl ... | bash` consumes stdin with the script content, so route the
# binary's stdin from /dev/tty when available so `webpilot setup` can prompt.

if [ "${WEBPILOT_NO_SETUP:-0}" = "1" ]; then
    exit 0
fi

if [ -r /dev/tty ]; then
    printf '\n' >&2
    "$INSTALL_DIR/webpilot" setup < /dev/tty
else
    # shellcheck disable=SC2016  # literal command to copy
    printf '\n  Next: run `%s` to install the Claude skill and Chrome extension.\n' \
        "webpilot setup" >&2
fi
