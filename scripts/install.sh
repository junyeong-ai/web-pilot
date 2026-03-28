#!/usr/bin/env bash
set -e

# ============================================================================
#  WebPilot Installer
#  Installs the webpilot binary and optionally the Claude Code skill.
#
#  Usage:
#    ./scripts/install.sh              # Interactive
#    ./scripts/install.sh --yes        # Accept all defaults (CI)
#    ./scripts/install.sh --source     # Force build from source
#    ./scripts/install.sh --no-skill   # Skip skill installation
#    ./scripts/install.sh --quiet      # Minimal output
# ============================================================================

BINARY_NAME="webpilot"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
REPO="junyeong-ai/web-pilot"
SKILL_NAME="webpilot"

# Flags
YES=false
QUIET=false
NO_SKILL=false
FORCE_SOURCE=false
FORCE_DOWNLOAD=false

# State tracking for summary
SUMMARY_BINARY=""
SUMMARY_SKILL=""
SUMMARY_PATH=""

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

prompt() {
    local message="$1" default="$2" var="$3"
    if $YES; then
        eval "$var=\"$default\""
        return
    fi
    printf "  %b " "$message" >&2
    read -r reply
    eval "$var=\"${reply:-$default}\""
}

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
        --yes|-y)        YES=true ;;
        --quiet|-q)      QUIET=true; YES=true ;;
        --no-skill)      NO_SKILL=true ;;
        --source)        FORCE_SOURCE=true ;;
        --download)      FORCE_DOWNLOAD=true ;;
        --help|-h)
            echo "Usage: install.sh [--yes] [--quiet] [--no-skill] [--source] [--download]"
            exit 0 ;;
        *)               warn "Unknown flag: $arg" ;;
    esac
done

# --- Resolve Project Root ----------------------------------------------------

resolve_project_root() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

    # Running from scripts/ inside the project
    if [ -f "$script_dir/../Cargo.toml" ]; then
        echo "$(cd "$script_dir/.." && pwd)"
        return
    fi
    # Running from project root
    if [ -f "./Cargo.toml" ]; then
        echo "$(pwd)"
        return
    fi
    echo ""
}

PROJECT_ROOT=$(resolve_project_root)
PROJECT_SKILL_DIR="${PROJECT_ROOT:+$PROJECT_ROOT/.claude/skills/$SKILL_NAME}"
USER_SKILL_DIR="$HOME/.claude/skills/$SKILL_NAME"

# --- Toolchain Detection -----------------------------------------------------

CARGO=""

find_cargo() {
    # 1. Already in PATH
    local c
    c=$(command -v cargo 2>/dev/null) && [ -x "$c" ] && { echo "$c"; return 0; }
    # 2. rustup default (~/.cargo/bin)
    [ -x "$HOME/.cargo/bin/cargo" ] && { echo "$HOME/.cargo/bin/cargo"; return 0; }
    # 3. mise shim
    [ -x "$HOME/.local/share/mise/shims/cargo" ] && { echo "$HOME/.local/share/mise/shims/cargo"; return 0; }
    # 4. mise which (activate mode, no shims in PATH)
    c=$(mise which cargo 2>/dev/null) && [ -x "$c" ] && { echo "$c"; return 0; }
    # 5. asdf shim
    [ -x "$HOME/.asdf/shims/cargo" ] && { echo "$HOME/.asdf/shims/cargo"; return 0; }
    # 6. Well-known system paths (brew, system)
    local p
    for p in /opt/homebrew/bin/cargo /usr/local/bin/cargo /usr/bin/cargo; do
        [ -x "$p" ] && { echo "$p"; return 0; }
    done
    return 1
}

# --- Platform Detection ------------------------------------------------------

detect_platform() {
    local os arch
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    arch=$(uname -m)

    case "$os" in
        linux)  os="unknown-linux-gnu" ;;
        darwin) os="apple-darwin" ;;
        *)      error "Unsupported OS: $os"; exit 1 ;;
    esac

    case "$arch" in
        x86_64)        arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)             error "Unsupported architecture: $arch"; exit 1 ;;
    esac

    echo "${arch}-${os}"
}

# --- Version Helpers ---------------------------------------------------------

get_latest_version() {
    curl -sf "https://api.github.com/repos/$REPO/releases/latest" \
        | grep '"tag_name"' \
        | sed -E 's/.*"v([^"]+)".*/\1/' \
        || echo ""
}

get_installed_version() {
    if [ -f "$INSTALL_DIR/$BINARY_NAME" ]; then
        "$INSTALL_DIR/$BINARY_NAME" --version 2>/dev/null | sed -E 's/.*[[:space:]]+//' || echo "unknown"
    else
        echo ""
    fi
}

get_skill_version() {
    local skill_md="$1"
    [ -f "$skill_md" ] && grep "^version:" "$skill_md" 2>/dev/null | sed 's/version: *//' || echo "unknown"
}

compare_versions() {
    local v1="$1" v2="$2"
    [ "$v1" = "$v2" ] && { echo "equal"; return; }
    [ "$v1" = "unknown" ] || [ "$v2" = "unknown" ] && { echo "unknown"; return; }
    if [ "$(printf '%s\n' "$v1" "$v2" | sort -V | head -n1)" = "$v1" ]; then
        echo "older"
    else
        echo "newer"
    fi
}

# --- Download Binary ---------------------------------------------------------

download_binary() {
    local version="$1" target="$2"
    local archive="webpilot-v${version}-${target}.tar.gz"
    local url="https://github.com/$REPO/releases/download/v${version}/${archive}"
    local checksum_url="${url}.sha256"
    local tmpdir
    tmpdir=$(mktemp -d)

    info "Downloading ${BOLD}$archive${NC}..."

    if ! curl -fL -o "$tmpdir/$archive" "$url" 2>/dev/null; then
        rm -rf "$tmpdir"
        return 1
    fi

    # Verify checksum
    if curl -fL -o "$tmpdir/${archive}.sha256" "$checksum_url" 2>/dev/null; then
        cd "$tmpdir"
        if command -v sha256sum >/dev/null; then
            sha256sum -c "${archive}.sha256" >/dev/null 2>&1 || { error "Checksum mismatch"; rm -rf "$tmpdir"; return 1; }
        elif command -v shasum >/dev/null; then
            shasum -a 256 -c "${archive}.sha256" >/dev/null 2>&1 || { error "Checksum mismatch"; rm -rf "$tmpdir"; return 1; }
        fi
        success "Checksum verified"
        cd - >/dev/null
    fi

    tar -xzf "$tmpdir/$archive" -C "$tmpdir" 2>/dev/null
    echo "$tmpdir/$BINARY_NAME"
}

# --- Build from Source -------------------------------------------------------

build_from_source() {
    if [ -z "$PROJECT_ROOT" ]; then
        error "Not in project directory — cannot build from source"
        error "Run from the web-pilot repo, or use ${BOLD}--download${NC}"
        exit 1
    fi

    if [ -z "$CARGO" ]; then
        error "cargo not found — install Rust: https://rustup.rs"
        exit 1
    fi

    info "Building from source... ${DIM}(this may take a few minutes)${NC}"
    $QUIET || info "Using ${DIM}$CARGO${NC}"
    echo "" >&2

    local build_log
    build_log=$(mktemp)

    (cd "$PROJECT_ROOT" && "$CARGO" build --release 2>&1) | tee "$build_log" | while IFS= read -r line; do
        if $QUIET; then
            :
        elif echo "$line" | grep -q "^   Compiling"; then
            printf "\r  ${DIM}%s${NC}%40s" "$line" "" >&2
        elif echo "$line" | grep -q "Finished"; then
            printf "\r%80s\r" "" >&2
            success "$line"
        elif echo "$line" | grep -q "^error"; then
            echo "  $line" >&2
        fi
    done

    if ! grep -q "^    Finished" "$build_log"; then
        error "Build failed"
        rm -f "$build_log"
        exit 1
    fi
    rm -f "$build_log"

    echo "$PROJECT_ROOT/target/release/$BINARY_NAME"
}

# --- Install Binary ----------------------------------------------------------

install_binary() {
    local binary_path="$1"

    mkdir -p "$INSTALL_DIR"
    cp "$binary_path" "$INSTALL_DIR/$BINARY_NAME"
    chmod +x "$INSTALL_DIR/$BINARY_NAME"

    if [[ "$OSTYPE" == "darwin"* ]]; then
        codesign --force --deep --sign - "$INSTALL_DIR/$BINARY_NAME" 2>/dev/null || true
    fi

    # Clean up temp dir if download was used
    local tmpdir
    tmpdir=$(dirname "$binary_path")
    if [ "$tmpdir" != "$PROJECT_ROOT/target/release" ]; then
        rm -rf "$tmpdir"
    fi
}

# --- Skill Installation ------------------------------------------------------

install_skill_files() {
    local dest="$1" source="$2"
    mkdir -p "$dest"
    cp "$source/SKILL.md" "$dest/SKILL.md"
}

do_skill_installation() {
    if $NO_SKILL; then return 0; fi
    if [ -z "$PROJECT_SKILL_DIR" ] || [ ! -f "$PROJECT_SKILL_DIR/SKILL.md" ]; then
        $QUIET || warn "Skill source not found — skipping"
        SUMMARY_SKILL="skipped ${DIM}(source not found)${NC}"
        return 0
    fi

    local project_ver
    project_ver=$(get_skill_version "$PROJECT_SKILL_DIR/SKILL.md")

    header "Claude Code Skill" "v$project_ver"

    if [ -d "$USER_SKILL_DIR" ] && [ -f "$USER_SKILL_DIR/SKILL.md" ]; then
        local existing_ver comparison
        existing_ver=$(get_skill_version "$USER_SKILL_DIR/SKILL.md")
        comparison=$(compare_versions "$existing_ver" "$project_ver")

        echo -e "  ${BOLD}Current:${NC}" >&2
        echo -e "    ${GREEN}●${NC} User-level  ${DIM}$USER_SKILL_DIR${NC}  v$existing_ver" >&2
        echo "" >&2

        case "$comparison" in
            equal)
                success "Already up to date"
                SUMMARY_SKILL="${GREEN}●${NC} up to date  ${DIM}v$existing_ver${NC}"
                ;;
            older)
                info "Update available: v$existing_ver ${DIM}→${NC} v$project_ver"
                if prompt_yn "Update? ${DIM}[Y/n]${NC}" "y"; then
                    local ts; ts=$(date +%Y%m%d_%H%M%S)
                    cp -r "$USER_SKILL_DIR" "$USER_SKILL_DIR.backup_$ts"
                    install_skill_files "$USER_SKILL_DIR" "$PROJECT_SKILL_DIR"
                    success "Updated to v$project_ver  ${DIM}(backup: .backup_$ts)${NC}"
                    SUMMARY_SKILL="${GREEN}●${NC} updated     ${DIM}v$existing_ver → v$project_ver${NC}"
                else
                    SUMMARY_SKILL="${GREEN}●${NC} kept        ${DIM}v$existing_ver${NC}"
                fi
                ;;
            newer)
                warn "Installed v$existing_ver is newer than source v$project_ver"
                SUMMARY_SKILL="${GREEN}●${NC} kept        ${DIM}v$existing_ver (newer)${NC}"
                ;;
            *)
                if prompt_yn "Reinstall? ${DIM}[y/N]${NC}" "n"; then
                    install_skill_files "$USER_SKILL_DIR" "$PROJECT_SKILL_DIR"
                    success "Reinstalled"
                    SUMMARY_SKILL="${GREEN}●${NC} reinstalled ${DIM}v$project_ver${NC}"
                else
                    SUMMARY_SKILL="${GREEN}●${NC} kept        ${DIM}v$existing_ver${NC}"
                fi
                ;;
        esac
    else
        echo -e "  ${BOLD}Install scope:${NC}" >&2
        echo "" >&2
        echo -e "    ${CYAN}1${NC}  User-level   ${DIM}All projects on this machine${NC}" >&2
        echo -e "    ${CYAN}2${NC}  Skip" >&2
        echo "" >&2

        local choice
        prompt "Choice ${DIM}[1]${NC}:" "1" choice

        case "$choice" in
            1)
                install_skill_files "$USER_SKILL_DIR" "$PROJECT_SKILL_DIR"
                success "Skill installed to ${DIM}$USER_SKILL_DIR${NC}"
                SUMMARY_SKILL="${GREEN}●${NC} installed   ${DIM}v$project_ver${NC}"
                ;;
            *)
                SUMMARY_SKILL="${DIM}○${NC} skipped"
                ;;
        esac
    fi
}

# --- Summary -----------------------------------------------------------------

print_summary() {
    $QUIET && return

    echo "" >&2
    echo -e "  ${BOLD}Summary${NC}" >&2
    divider
    echo -e "    Binary   $SUMMARY_BINARY" >&2
    echo -e "    Skill    $SUMMARY_SKILL" >&2
    echo -e "    PATH     $SUMMARY_PATH" >&2
    divider
    echo "" >&2

    echo -e "  ${BOLD}Next steps:${NC}" >&2
    echo -e "    ${CYAN}→${NC} ${BOLD}$BINARY_NAME status${NC}                              Check connection" >&2
    echo -e "    ${CYAN}→${NC} ${BOLD}$BINARY_NAME capture --dom --url${NC} \"https://...\"   Capture a page" >&2
    echo -e "    ${CYAN}→${NC} ${BOLD}/webpilot${NC} in Claude Code                        Invoke skill" >&2
    echo "" >&2
}

# --- Main --------------------------------------------------------------------

main() {
    header "WebPilot" "Installer"

    # --- Prerequisites ---
    local target version installed_ver
    target=$(detect_platform)
    version=$(get_latest_version)
    installed_ver=$(get_installed_version)
    CARGO=$(find_cargo 2>/dev/null || true)

    # Show current status
    echo -e "  ${BOLD}Status:${NC}" >&2
    if [ -n "$installed_ver" ]; then
        echo -e "    ${GREEN}●${NC} Binary   ${DIM}$INSTALL_DIR/$BINARY_NAME${NC}  v$installed_ver" >&2
    else
        echo -e "    ${DIM}○${NC} Binary   ${DIM}$INSTALL_DIR/$BINARY_NAME${NC}  ${DIM}not installed${NC}" >&2
    fi
    if [ -d "$USER_SKILL_DIR" ] && [ -f "$USER_SKILL_DIR/SKILL.md" ]; then
        local skv; skv=$(get_skill_version "$USER_SKILL_DIR/SKILL.md")
        echo -e "    ${GREEN}●${NC} Skill    ${DIM}$USER_SKILL_DIR${NC}  v$skv" >&2
    else
        echo -e "    ${DIM}○${NC} Skill    ${DIM}$USER_SKILL_DIR${NC}  ${DIM}not installed${NC}" >&2
    fi
    if [ -n "$PROJECT_ROOT" ]; then
        echo -e "    ${GREEN}●${NC} Source   ${DIM}$PROJECT_ROOT${NC}" >&2
    else
        echo -e "    ${DIM}○${NC} Source   ${DIM}not in project directory${NC}" >&2
    fi
    if [ -n "$CARGO" ]; then
        local cargo_ver; cargo_ver=$("$CARGO" --version 2>/dev/null | head -1 || echo "unknown")
        echo -e "    ${GREEN}●${NC} Cargo    ${DIM}$CARGO${NC}  ${DIM}($cargo_ver)${NC}" >&2
    else
        echo -e "    ${DIM}○${NC} Cargo    ${DIM}not found${NC}" >&2
    fi
    echo "" >&2

    # --- Choose install method ---
    local binary_path=""
    local can_download=false can_build=false

    [ -n "$version" ] && command -v curl >/dev/null && can_download=true
    [ -n "$PROJECT_ROOT" ] && [ -n "$CARGO" ] && can_build=true

    if $FORCE_SOURCE; then
        binary_path=$(build_from_source)
    elif $FORCE_DOWNLOAD; then
        if ! $can_download; then
            error "Cannot download — no release found or curl missing"
            exit 1
        fi
        binary_path=$(download_binary "$version" "$target") || { error "Download failed"; exit 1; }
    elif $can_download && $can_build; then
        if [ -n "$version" ]; then
            info "Latest release: ${BOLD}v$version${NC}  ${DIM}($target)${NC}"
        fi
        echo "" >&2
        echo -e "  ${BOLD}Install method:${NC}" >&2
        echo "" >&2
        echo -e "    ${CYAN}1${NC}  Download prebuilt binary  ${DIM}(fast)${NC}" >&2
        echo -e "    ${CYAN}2${NC}  Build from source         ${DIM}(requires Rust)${NC}" >&2
        echo "" >&2

        local method
        prompt "Choice ${DIM}[1]${NC}:" "1" method

        case "$method" in
            2)  binary_path=$(build_from_source) ;;
            *)
                binary_path=$(download_binary "$version" "$target") || {
                    warn "Download failed, falling back to source build"
                    binary_path=$(build_from_source)
                }
                ;;
        esac
    elif $can_download; then
        info "Downloading v$version..."
        binary_path=$(download_binary "$version" "$target") || { error "Download failed"; exit 1; }
    elif $can_build; then
        binary_path=$(build_from_source)
    else
        error "Cannot install: no release available and cargo not found"
        error "Install Rust (https://rustup.rs) or download a release from GitHub"
        exit 1
    fi

    # --- Install binary ---
    install_binary "$binary_path"

    local new_ver
    new_ver=$("$INSTALL_DIR/$BINARY_NAME" --version 2>/dev/null | sed -E 's/.*[[:space:]]+//' || echo "installed")

    if [ -n "$installed_ver" ] && [ "$installed_ver" != "$new_ver" ]; then
        success "Binary updated  ${DIM}v$installed_ver → v$new_ver${NC}"
        SUMMARY_BINARY="${GREEN}●${NC} $INSTALL_DIR/$BINARY_NAME  ${DIM}v$installed_ver → v$new_ver${NC}"
    elif [ -n "$installed_ver" ]; then
        success "Binary reinstalled  ${DIM}v$new_ver${NC}"
        SUMMARY_BINARY="${GREEN}●${NC} $INSTALL_DIR/$BINARY_NAME  ${DIM}v$new_ver (reinstalled)${NC}"
    else
        success "Binary installed  ${DIM}v$new_ver${NC}"
        SUMMARY_BINARY="${GREEN}●${NC} $INSTALL_DIR/$BINARY_NAME  ${DIM}v$new_ver${NC}"
    fi

    # --- PATH check ---
    if echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
        SUMMARY_PATH="${GREEN}●${NC} $INSTALL_DIR ${DIM}in PATH${NC}"
    else
        warn "$INSTALL_DIR is not in PATH"
        echo -e "    ${DIM}Add to ~/.zshrc or ~/.bashrc:${NC}" >&2
        echo -e "    ${BOLD}export PATH=\"\$HOME/.local/bin:\$PATH\"${NC}" >&2
        SUMMARY_PATH="${YELLOW}!${NC} $INSTALL_DIR ${DIM}not in PATH${NC}"
    fi

    # --- Skill installation ---
    do_skill_installation

    # --- Summary ---
    print_summary
}

main "$@"
