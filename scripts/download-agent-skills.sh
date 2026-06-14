#!/bin/bash
#
# Download and extract knot-server agent-skills documentation
#
# Usage: curl -fsSL https://raw.githubusercontent.com/raultov/knot-server/master/scripts/download-agent-skills.sh | bash
#

set -e

# Color output (printf interprets \033 directly — no shell-specific extensions).
RED=$(printf '\033[0;31m')
GREEN=$(printf '\033[0;32m')
YELLOW=$(printf '\033[1;33m')
BLUE=$(printf '\033[0;34m')
NC=$(printf '\033[0m') # No Color

# Config
TARGET_DIR="${1:-.knot-server-agent-skills}"
GITHUB_REPO="${2:-https://raw.githubusercontent.com/raultov/knot-server/master}"

printf '%b📦 Downloading knot-server agent-skills documentation...%b\n' "$BLUE" "$NC"

# Create target directory
mkdir -p "$TARGET_DIR"

# Define files to download
files=(
  "preflight.md"
  "search.md"
  "callers.md"
  "explore.md"
  "deps.md"
  "graph.md"
  "repos.md"
  "workflows.md"
  "index.md"
)

# Base URL for documentation
BASE_URL="${GITHUB_REPO}/skills"

printf '%bDestination: %b%s%b\n\n' "$BLUE" "$GREEN" "$TARGET_DIR" "$NC"

# Download each file
downloaded=0
for file in "${files[@]}"; do
  printf '%bDownloading%b %s ... ' "$YELLOW" "$NC" "$file"

  if curl -fsSL "${BASE_URL}/${file}" -o "${TARGET_DIR}/${file}"; then
    printf '%b✓%b\n' "$GREEN" "$NC"
    downloaded=$((downloaded + 1))
  else
    printf '%b✗%b\n' "$RED" "$NC"
  fi
done

printf '\n'
printf '%b✅ Downloaded %d/%d files%b\n' "$GREEN" "$downloaded" "${#files[@]}" "$NC"
printf '\n'
printf '%b📖 Documentation files:%b\n' "$BLUE" "$NC"
printf '   - %s/preflight.md    (Server health and index status)\n' "$TARGET_DIR"
printf '   - %s/search.md       (Semantic code discovery)\n' "$TARGET_DIR"
printf '   - %s/callers.md      (Reverse dependency lookup)\n' "$TARGET_DIR"
printf '   - %s/explore.md      (File anatomy discovery)\n' "$TARGET_DIR"
printf '   - %s/deps.md         (Repository dependency graph)\n' "$TARGET_DIR"
printf '   - %s/graph.md        (Entity relationship subgraphs)\n' "$TARGET_DIR"
printf '   - %s/repos.md        (Indexed repository inventory)\n' "$TARGET_DIR"
printf '   - %s/workflows.md    (Common patterns & best practices)\n' "$TARGET_DIR"
printf '   - %s/index.md        (Register and index the current repository)\n' "$TARGET_DIR"
printf '\n'
printf '%b🚀 Quick start:%b\n' "$BLUE" "$NC"
printf '   less %s/preflight.md\n' "$TARGET_DIR"
printf '   less %s/search.md\n' "$TARGET_DIR"
printf '   less %s/workflows.md\n' "$TARGET_DIR"
