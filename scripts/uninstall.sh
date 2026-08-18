#!/usr/bin/env bash
# WebPilot uninstaller — a curl-able front-end over `webpilot self uninstall`.
#
# Every WebPilot artefact belongs to the binary: the embedded Claude skill, the
# extracted Chrome extension, the Native Messaging host manifest, and the
# per-user cache/runtime tree are installed by `webpilot setup` and removed by
# `webpilot self uninstall` in one typed pass — using the binary's OWN path
# resolution (`webpilot::dirs`, the NM host locator) as the single source of
# truth, removing only what it created. This script's only job is to find that
# binary and hand off to it; it never re-derives those paths in shell, which
# would silently drift from the Rust definitions.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/junyeong-ai/web-pilot/main/scripts/uninstall.sh | bash
#   bash scripts/uninstall.sh          # the binary prompts before removing
#   bash scripts/uninstall.sh --yes    # remove without prompting
#
# Options:
#   -y, --yes    Remove every artefact without prompting.
#   -h, --help   Show this help.
#
# Environment:
#   WEBPILOT_INSTALL_DIR   Where the binary lives. Default: $HOME/.local/bin.
#
set -euo pipefail

if [ -t 2 ]; then C_DIM=$'\033[2m'; C_YEL=$'\033[33m'; C_RED=$'\033[31m'; C_RST=$'\033[0m'
else            C_DIM=; C_YEL=; C_RED=; C_RST=
fi
say()  { printf '  %s→%s %s\n' "$C_DIM" "$C_RST" "$*" >&2; }
warn() { printf '  %s!%s %s\n' "$C_YEL" "$C_RST" "$*" >&2; }
die()  { printf '  %s✗%s %s\n' "$C_RED" "$C_RST" "$*" >&2; exit 1; }

usage() { sed -n '2,24p' "${BASH_SOURCE[0]:-$0}" | sed 's/^#\{0,1\} \{0,1\}//'; }

# Every side-effecting step lives in main, invoked only by the last line of the
# file — so a partially-downloaded `curl | bash` stream defines functions and
# then ends, instead of executing half an uninstall.
main() {
    local install_dir="${WEBPILOT_INSTALL_DIR:-$HOME/.local/bin}"
    local assume_yes=false

    while [ "$#" -gt 0 ]; do
        case "$1" in
            -y|--yes)  assume_yes=true ;;
            -h|--help) usage; exit 0 ;;
            *)         warn "unknown option: $1"; usage >&2; exit 1 ;;
        esac
        shift
    done

    # Locate the binary: the install dir first (what install.sh writes), then PATH.
    local bin=""
    if [ -x "$install_dir/webpilot" ]; then
        bin="$install_dir/webpilot"
    elif command -v webpilot >/dev/null 2>&1; then
        bin="$(command -v webpilot)"
    fi

    # No binary → nothing to delegate to. The artefact paths are the binary's to
    # know; guessing them in shell is exactly the drift-prone duplication this
    # script exists to avoid. Point the user at the one clean recovery instead.
    if [ -z "$bin" ]; then
        warn "no webpilot binary on PATH or in $install_dir — nothing to uninstall"
        say  "If artefacts remain after the binary was removed by hand, restore it"
        say  "and let it clean up after itself:"
        say  "  curl -fsSL https://raw.githubusercontent.com/junyeong-ai/web-pilot/main/scripts/install.sh | bash -s -- --no-setup"
        say  "  webpilot self uninstall"
        exit 0
    fi

    # This script is fetched fresh while the binary is whatever the user
    # installed, so the spelling is the BINARY's to decide: `self uninstall` only
    # exists from 0.8.1. Ask it rather than assume — `--help` succeeds exactly
    # when the subcommand is there, so the answer comes from the binary itself
    # instead of a version string parsed here.
    local -a cmd=(uninstall)
    if "$bin" self uninstall --help >/dev/null 2>&1; then
        cmd=(self uninstall)
    fi

    # Hand off to the single source of truth. It removes the skill, extension,
    # NM host, cache/runtime tree, and finally the binary itself — in dependency
    # order, only what it created. `--yes` passes through; without it the binary
    # prompts, so route its stdin from the terminal when we were piped from curl.
    say "Removing via $bin ${cmd[*]}"
    if [ "$assume_yes" = true ]; then
        exec "$bin" "${cmd[@]}" --yes
    elif [ -r /dev/tty ]; then
        exec "$bin" "${cmd[@]}" < /dev/tty
    else
        die "non-interactive with no terminal to confirm at — re-run with --yes"
    fi
}

main "$@"
