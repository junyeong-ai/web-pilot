#!/usr/bin/env bash
# WebPilot installer.
#
# Lands a `webpilot` binary on PATH and hands off to `webpilot setup` for the
# Claude skill, Chrome extension, and Native Messaging host. The skill and
# extension are embedded in the binary, so `setup` installs them with zero
# version drift. The binary owns its lifecycle: `webpilot self update`,
# `webpilot self uninstall`.
#
# The binary comes from one of two sources, selectable:
#   - prebuilt   download a verified release archive (default; fast).
#   - source     `cargo build` the current checkout (needs Rust + a checkout).
#
# A default run asks nothing: it takes the prebuilt binary and completes setup
# unattended, where every prompt resolves to its safe answer — so a skill the
# user has edited is kept, never overwritten. Each decision has a flag, and
# --interactive restores the guided prompts (read from /dev/tty, so they work
# even under `curl | bash`).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/junyeong-ai/web-pilot/main/scripts/install.sh | bash
#   curl -fsSL .../install.sh | bash -s -- --source --no-setup
#   bash scripts/install.sh --interactive         # from a checkout, guided
#
# Uninstall (symmetric one-shot — delegates to `webpilot self uninstall`):
#   curl -fsSL https://raw.githubusercontent.com/junyeong-ai/web-pilot/main/scripts/uninstall.sh | bash
#
# Run with --help for the flag inventory. Flags win over the environment:
#   WEBPILOT_BUILD         "prebuilt" (default) or "source".
#   WEBPILOT_VERSION       Pin a release tag (prebuilt only). Default: latest.
#   WEBPILOT_INSTALL_DIR   Install path. Default: $HOME/.local/bin.
#   WEBPILOT_REPO          Override repo (e.g. fork). Default: junyeong-ai/web-pilot.
#   WEBPILOT_NO_SETUP=1    Skip the post-install `webpilot setup`.
#
set -euo pipefail

# --- output ------------------------------------------------------------------

if [ -t 2 ]; then C_DIM=$'\033[2m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'; C_RED=$'\033[31m'; C_RST=$'\033[0m'
else            C_DIM=; C_GRN=; C_YEL=; C_RED=; C_RST=
fi
say()  { printf '  %s→%s %s\n' "$C_DIM" "$C_RST" "$*" >&2; }
ok()   { printf '  %s✓%s %s\n' "$C_GRN" "$C_RST" "$*" >&2; }
warn() { printf '  %s!%s %s\n' "$C_YEL" "$C_RST" "$*" >&2; }
die()  { printf '  %s✗%s %s\n' "$C_RED" "$C_RST" "$*" >&2; exit 1; }

require() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

usage() {
    cat >&2 <<'EOF'
  WebPilot installer

  Usage: install.sh [options]

    --prebuilt              Download a verified release archive (default)
    --source                Build the current checkout with cargo
    --version <VER>         Pin a release version (prebuilt only)
    --install-dir <DIR>     Install path (default: $HOME/.local/bin)
    --verify-attestations   Also check GitHub build provenance (needs the gh CLI)
    --no-setup              Stop after installing the binary
    --interactive           Ask before each decision instead of taking defaults
    -h, --help              Show this message
EOF
}

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --prebuilt)            BUILD="prebuilt" ;;
            --source)              BUILD="source" ;;
            --version)             shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1" ;;
            --version=*)           VERSION="${1#*=}" ;;
            --install-dir)         shift; [ $# -gt 0 ] || die "--install-dir needs a value"; INSTALL_DIR="$1" ;;
            --install-dir=*)       INSTALL_DIR="${1#*=}" ;;
            --verify-attestations) VERIFY_ATTESTATIONS=1 ;;
            --no-setup)            NO_SETUP=1 ;;
            --interactive)         INTERACTIVE=1 ;;
            -h|--help)             usage; exit 0 ;;
            *)                     die "unknown option: $1 (try --help)" ;;
        esac
        shift
    done
}

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

# --- binary production -------------------------------------------------------

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

    local os arch
    case "$(uname -s)" in
        Linux)  os="unknown-linux-gnu" ;;
        Darwin) os="apple-darwin" ;;
        *)      die "unsupported OS: $(uname -s)" ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64)
            arch="x86_64"
            # A shell under Rosetta reports x86_64 on Apple silicon — prefer
            # the native binary the hardware actually runs best.
            if [ "$os" = "apple-darwin" ] && [ "$(sysctl -in hw.optional.arm64 2>/dev/null)" = "1" ]; then
                arch="aarch64"
            fi
            ;;
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

    if [ "$VERIFY_ATTESTATIONS" = "1" ]; then
        # The sidecar travels the same channel as the archive, so it proves the
        # transfer, not the origin. The attestation ties these bytes to a run of
        # the project's release workflow — and having been asked for it, a
        # failure here aborts rather than warns.
        require gh
        say "Verifying build provenance"
        gh attestation verify "$tmp/$archive" --repo "$REPO" >&2 \
            || die "attestation verification failed for $archive"
    fi

    tar -xzf "$tmp/$archive" -C "$tmp"
    BIN="$tmp/webpilot-${VERSION}-${target}/webpilot"
    [ -x "$BIN" ] || die "archive missing webpilot binary"
    ok "Downloaded webpilot v$VERSION"
}

cleanup() {
    rm -rf "$tmp"
    [ -n "$staged" ] && [ -f "$staged" ] && rm -f "$staged"
    return 0  # bash propagates the EXIT trap's last $? as the script's exit status
}

# Every side-effecting step lives in main, invoked only by the last line of the
# file — so a partially-downloaded `curl | bash` stream defines functions and
# then ends, instead of executing half an install.
main() {
    REPO="${WEBPILOT_REPO:-junyeong-ai/web-pilot}"
    INSTALL_DIR="${WEBPILOT_INSTALL_DIR:-$HOME/.local/bin}"
    VERSION="${WEBPILOT_VERSION:-}"
    BUILD="${WEBPILOT_BUILD:-}"
    NO_SETUP="${WEBPILOT_NO_SETUP:-0}"
    VERIFY_ATTESTATIONS=0
    INTERACTIVE=0
    parse_args "$@"

    # --- locate a checkout (enables source builds) ---------------------------

    CHECKOUT=""
    local src dir
    src="${BASH_SOURCE[0]:-$0}"
    if dir="$(cd "$(dirname "$src")" 2>/dev/null && pwd -P)" && [ -f "$dir/../Cargo.toml" ]; then
        CHECKOUT="$(cd "$dir/.." && pwd -P)"
    fi

    # --- choose build method --------------------------------------------------

    if [ -z "$BUILD" ]; then
        if [ "$INTERACTIVE" = "1" ] && [ -n "$CHECKOUT" ] && [ -r /dev/tty ]; then
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

    # --- produce a binary at $BIN ---------------------------------------------

    BIN=""
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/webpilot-install.XXXXXX")
    staged=""
    trap cleanup EXIT

    if [ "$BUILD" = "source" ]; then build_from_source; else download_prebuilt; fi

    # --- atomic install --------------------------------------------------------

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

    # --- PATH guidance ----------------------------------------------------------

    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            warn "$INSTALL_DIR is not on PATH"
            printf '\n    Add to your shell profile (~/.zshrc, ~/.bashrc):\n' >&2
            # shellcheck disable=SC2016  # literal $PATH for the user to copy
            printf '      export PATH="%s:$PATH"\n\n' "$INSTALL_DIR" >&2
            ;;
    esac

    # --- hand off to `webpilot setup` (installs the skill + extension) ---------

    if [ "$NO_SETUP" = "1" ]; then
        exit 0
    fi

    printf '\n' >&2
    if [ "$INTERACTIVE" = "1" ] && [ -r /dev/tty ]; then
        # `curl ... | bash` consumes stdin with the script content, so route the
        # binary's stdin from the terminal for it to prompt on.
        "$INSTALL_DIR/webpilot" setup < /dev/tty
    else
        # Unattended: with no terminal on stdin every prompt resolves to its safe
        # answer, so the skill, extension and NM host are deployed while a skill
        # the user has edited is kept — and `setup` reports which.
        "$INSTALL_DIR/webpilot" setup < /dev/null
    fi
}

main "$@"
