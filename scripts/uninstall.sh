#!/usr/bin/env bash
set -e

# ============================================================================
#  WebPilot Uninstaller
#  Removes the webpilot binary, skill, and temporary files.
#
#  Usage:
#    ./scripts/uninstall.sh            # Interactive
#    ./scripts/uninstall.sh --yes      # Accept all defaults (remove everything)
#    ./scripts/uninstall.sh --quiet    # Minimal output
# ============================================================================

BINARY_NAME="webpilot"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
SKILL_NAME="webpilot"
USER_SKILL_DIR="$HOME/.claude/skills/$SKILL_NAME"
CURRENT_USER="$(whoami)"
PID_FILE="/tmp/webpilot-${CURRENT_USER}-headless.pid"
WS_FILE="/tmp/webpilot-${CURRENT_USER}-headless.ws"
SCREENSHOT_DIR="/tmp/webpilot"

# Flags
YES=false
QUIET=false

# State tracking for summary
SUMMARY_CHROME=""
SUMMARY_BINARY=""
SUMMARY_SKILL=""
SUMMARY_CACHE=""

# --- Colors & Helpers --------------------------------------------------------

GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
RED='\033[0;31m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

info()    { $QUIET || echo -e "  ${CYAN}→${NC} $*" >&2; }
success() { $QUIET || echo -e "  ${GREEN}✓${NC} $*" >&2; }
warn()    { echo -e "  ${YELLOW}!${NC} $*" >&2; }
error()   { echo -e "  ${RED}✗${NC} $*" >&2; }
header()  { $QUIET || { echo "" >&2; echo -e "  ${BOLD}${BLUE}$1${NC}  ${DIM}$2${NC}" >&2; echo -e "  ${DIM}────────────────────────────────────${NC}" >&2; echo "" >&2; }; }
divider() { $QUIET || echo -e "  ${DIM}────────────────────────────────────${NC}" >&2; }

prompt_yn() {
    local message="$1" default="$2"
    if $YES; then
        [ "$default" = "y" ] && return 0 || return 1
    fi
    printf "  %b " "$message" >&2
    read -r reply
    reply="${reply:-$default}"
    [[ "$reply" =~ ^[yY]$ ]]
}

# --- Parse Flags -------------------------------------------------------------

for arg in "$@"; do
    case "$arg" in
        --yes|-y)   YES=true ;;
        --quiet|-q) QUIET=true; YES=true ;;
        --help|-h)
            echo "Usage: uninstall.sh [--yes] [--quiet]"
            exit 0 ;;
        *)          warn "Unknown flag: $arg" ;;
    esac
done

# --- Main --------------------------------------------------------------------

main() {
    header "WebPilot" "Uninstaller"

    # --- Current status ---
    local has_chrome=false has_binary=false has_skill=false has_cache=false

    echo -e "  ${BOLD}Current status:${NC}" >&2

    # Headless Chrome
    if [ -f "$PID_FILE" ]; then
        local pid
        pid=$(cat "$PID_FILE" 2>/dev/null)
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            echo -e "    ${GREEN}●${NC} Chrome   ${DIM}headless running (PID $pid)${NC}" >&2
            has_chrome=true
        else
            echo -e "    ${DIM}○${NC} Chrome   ${DIM}stale PID file${NC}" >&2
        fi
    else
        echo -e "    ${DIM}○${NC} Chrome   ${DIM}not running${NC}" >&2
    fi

    # Binary
    if [ -f "$INSTALL_DIR/$BINARY_NAME" ]; then
        local ver
        ver=$("$INSTALL_DIR/$BINARY_NAME" --version 2>/dev/null | sed -E 's/.*[[:space:]]+//' || echo "unknown")
        echo -e "    ${GREEN}●${NC} Binary   ${DIM}$INSTALL_DIR/$BINARY_NAME${NC}  v$ver" >&2
        has_binary=true
    else
        echo -e "    ${DIM}○${NC} Binary   ${DIM}not installed${NC}" >&2
    fi

    # Skill
    if [ -d "$USER_SKILL_DIR" ] && [ -f "$USER_SKILL_DIR/SKILL.md" ]; then
        local skv
        skv=$(grep "^version:" "$USER_SKILL_DIR/SKILL.md" 2>/dev/null | sed 's/version: *//' || echo "unknown")
        echo -e "    ${GREEN}●${NC} Skill    ${DIM}$USER_SKILL_DIR${NC}  v$skv" >&2
        has_skill=true
    else
        echo -e "    ${DIM}○${NC} Skill    ${DIM}not installed${NC}" >&2
    fi

    # Screenshot cache
    if [ -d "$SCREENSHOT_DIR" ]; then
        local count
        count=$(find "$SCREENSHOT_DIR" -type f 2>/dev/null | wc -l | tr -d ' ')
        echo -e "    ${GREEN}●${NC} Cache    ${DIM}$SCREENSHOT_DIR${NC}  ${DIM}($count files)${NC}" >&2
        has_cache=true
    else
        echo -e "    ${DIM}○${NC} Cache    ${DIM}$SCREENSHOT_DIR${NC}  ${DIM}empty${NC}" >&2
    fi

    echo "" >&2

    # Nothing to remove
    if ! $has_chrome && ! $has_binary && ! $has_skill && ! $has_cache; then
        success "Nothing to uninstall"
        return 0
    fi

    # --- Stop headless Chrome ---
    if $has_chrome; then
        local pid
        pid=$(cat "$PID_FILE" 2>/dev/null)
        if prompt_yn "Stop headless Chrome (PID $pid)? ${DIM}[Y/n]${NC}" "y"; then
            if kill "$pid" 2>/dev/null; then
                # Wait briefly for clean shutdown
                for _ in 1 2 3; do
                    kill -0 "$pid" 2>/dev/null || break
                    sleep 0.5
                done
                if kill -0 "$pid" 2>/dev/null; then
                    warn "Chrome did not exit cleanly, sending SIGKILL"
                    kill -9 "$pid" 2>/dev/null || true
                fi
                success "Stopped headless Chrome"
                SUMMARY_CHROME="${GREEN}✓${NC} stopped  ${DIM}(PID $pid)${NC}"
            else
                warn "Could not stop process (may have already exited)"
                SUMMARY_CHROME="${YELLOW}!${NC} already exited"
            fi
            rm -f "$PID_FILE" "$WS_FILE"
        else
            info "Left Chrome running"
            SUMMARY_CHROME="${DIM}○${NC} kept running"
        fi
    else
        # Clean up stale PID/WS files
        rm -f "$PID_FILE" "$WS_FILE"
        SUMMARY_CHROME="${DIM}○${NC} not running"
    fi

    # --- Remove binary ---
    if $has_binary; then
        if prompt_yn "Remove binary? ${DIM}[Y/n]${NC}" "y"; then
            rm "$INSTALL_DIR/$BINARY_NAME"
            success "Removed ${DIM}$INSTALL_DIR/$BINARY_NAME${NC}"
            SUMMARY_BINARY="${GREEN}✓${NC} removed"
        else
            info "Kept binary"
            SUMMARY_BINARY="${DIM}○${NC} kept"
        fi
    else
        SUMMARY_BINARY="${DIM}○${NC} not installed"
    fi

    # --- Remove skill ---
    if $has_skill; then
        if prompt_yn "Remove Claude Code skill? ${DIM}[y/N]${NC}" "n"; then
            if prompt_yn "Create backup first? ${DIM}[Y/n]${NC}" "y"; then
                local ts; ts=$(date +%Y%m%d_%H%M%S)
                cp -r "$USER_SKILL_DIR" "$USER_SKILL_DIR.backup_$ts"
                info "Backup: ${DIM}$USER_SKILL_DIR.backup_$ts${NC}"
            fi

            rm -rf "$USER_SKILL_DIR"
            success "Removed skill"
            SUMMARY_SKILL="${GREEN}✓${NC} removed"

            # Cleanup empty parent
            if [ -d "$HOME/.claude/skills" ] && [ -z "$(ls -A "$HOME/.claude/skills" 2>/dev/null)" ]; then
                rmdir "$HOME/.claude/skills" 2>/dev/null || true
            fi
        else
            info "Kept skill"
            SUMMARY_SKILL="${DIM}○${NC} kept"
        fi
    else
        SUMMARY_SKILL="${DIM}○${NC} not installed"
    fi

    # --- Remove screenshot cache ---
    if $has_cache; then
        if prompt_yn "Remove screenshot cache? ${DIM}[Y/n]${NC}" "y"; then
            rm -rf "$SCREENSHOT_DIR"
            success "Removed ${DIM}$SCREENSHOT_DIR${NC}"
            SUMMARY_CACHE="${GREEN}✓${NC} removed"
        else
            info "Kept screenshot cache"
            SUMMARY_CACHE="${DIM}○${NC} kept"
        fi
    else
        SUMMARY_CACHE="${DIM}○${NC} empty"
    fi

    # --- Summary ---
    $QUIET && return 0

    echo "" >&2
    echo -e "  ${BOLD}Summary${NC}" >&2
    divider
    echo -e "    Chrome   $SUMMARY_CHROME" >&2
    echo -e "    Binary   $SUMMARY_BINARY" >&2
    echo -e "    Skill    $SUMMARY_SKILL" >&2
    echo -e "    Cache    $SUMMARY_CACHE" >&2
    divider
    echo "" >&2

    echo -e "  ${DIM}Notes:${NC}" >&2
    echo -e "    ${DIM}● Project-level skill (.claude/skills/$SKILL_NAME) is not touched${NC}" >&2
    echo -e "    ${DIM}● Chrome for Testing is not removed (may be shared)${NC}" >&2
    echo -e "    ${DIM}● To reinstall: ./scripts/install.sh${NC}" >&2
    echo "" >&2
}

main "$@"
