#!/bin/bash
# ============================================================
#  WebPilot Skill Installer for Claude Code
#  Interactive installer with level selection
# ============================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
SKILL_SOURCE="$PROJECT_DIR/.claude/skills/webpilot/SKILL.md"

# Colors
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
RED='\033[0;31m'
NC='\033[0m'

USER_DIR="$HOME/.claude/skills/webpilot"
PROJECT_SKILL_DIR="$(pwd)/.claude/skills/webpilot"

check_installed() {
  local path="$1"
  [ -f "$path/SKILL.md" ] && echo "installed" || echo ""
}

install_skill() {
  local dest_dir="$1"
  mkdir -p "$dest_dir"
  cp "$SKILL_SOURCE" "$dest_dir/SKILL.md"
}

# --- Main ---

if [ ! -f "$SKILL_SOURCE" ]; then
  echo -e "  ${RED}✗${NC} Skill source not found: $SKILL_SOURCE"
  exit 1
fi

# Check current installation status
user_status=$(check_installed "$USER_DIR")
project_status=$(check_installed "$PROJECT_SKILL_DIR")

echo ""
echo -e "${BOLD}${BLUE}  WebPilot${NC} ${DIM}Skill Installer for Claude Code${NC}"
echo -e "  ${DIM}────────────────────────────────────${NC}"
echo ""
echo -e "  ${BOLD}Current Status:${NC}"

if [ -n "$user_status" ]; then
  echo -e "    ${GREEN}●${NC} User-level    ${DIM}~/.claude/skills/webpilot${NC}  ${GREEN}installed${NC}"
else
  echo -e "    ${DIM}○${NC} User-level    ${DIM}~/.claude/skills/webpilot${NC}  ${DIM}not installed${NC}"
fi

if [ -n "$project_status" ]; then
  echo -e "    ${GREEN}●${NC} Project-level ${DIM}.claude/skills/webpilot${NC}   ${GREEN}installed${NC}"
else
  echo -e "    ${DIM}○${NC} Project-level ${DIM}.claude/skills/webpilot${NC}   ${DIM}not installed${NC}"
fi

# All installed — nothing to do
if [ -n "$user_status" ] && [ -n "$project_status" ]; then
  echo ""
  echo -e "  ${GREEN}✓${NC} All levels installed. Use ${BOLD}/webpilot${NC} in Claude Code."
  echo ""
  exit 0
fi

# Build menu options dynamically based on what's missing
echo ""
options=()
labels=()

if [ -z "$user_status" ]; then
  options+=("user")
  labels+=("User-level     ${DIM}All projects on this machine${NC}")
fi
if [ -z "$project_status" ]; then
  options+=("project")
  labels+=("Project-level  ${DIM}Current project only ($(basename "$(pwd)"))${NC}")
fi
if [ ${#options[@]} -eq 2 ]; then
  options+=("both")
  labels+=("Both")
fi

echo -e "  ${BOLD}Install:${NC}"
echo ""
for i in "${!labels[@]}"; do
  echo -e "    ${CYAN}$((i+1))${NC}  ${labels[$i]}"
done
echo -e "    ${CYAN}q${NC}  Quit"
echo ""
printf "  Choice: "
read -r choice

echo ""

if [ "$choice" = "q" ] || [ "$choice" = "Q" ] || [ -z "$choice" ]; then
  echo -e "  ${DIM}Cancelled.${NC}"
  exit 0
fi

idx=$((choice - 1))
if [ "$idx" -lt 0 ] || [ "$idx" -ge ${#options[@]} ]; then
  echo -e "  ${RED}✗${NC} Invalid choice"
  exit 1
fi

selected="${options[$idx]}"

case "$selected" in
  user)
    install_skill "$USER_DIR"
    echo -e "  ${GREEN}✓${NC} User-level installed"
    ;;
  project)
    install_skill "$PROJECT_SKILL_DIR"
    echo -e "  ${GREEN}✓${NC} Project-level installed"
    ;;
  both)
    install_skill "$USER_DIR"
    install_skill "$PROJECT_SKILL_DIR"
    echo -e "  ${GREEN}✓${NC} User-level installed"
    echo -e "  ${GREEN}✓${NC} Project-level installed"
    ;;
esac

echo ""
echo -e "  ${CYAN}→${NC} Invoke: ${BOLD}/webpilot${NC} in Claude Code"
echo -e "  ${CYAN}→${NC} Auto-activates on browser/web control tasks"
echo ""
