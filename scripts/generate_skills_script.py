#!/usr/bin/env python3
"""
Generate .knot-server-agent-skills.sh from skills/*.md.

Each markdown file is base64-encoded and embedded in the shell script as a
single-quoted string. The shell script decodes them on install.
"""
from __future__ import annotations

import base64
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SKILL_FILES = [
    "preflight.md",
    "search.md",
    "callers.md",
    "explore.md",
    "deps.md",
    "graph.md",
    "repos.md",
    "workflows.md",
    "index.md",
]

COMMAND_FILES = [
    "commands/index.toml",
    "skills/claude-index.md",
]

SCRIPT_HEADER = r"""#!/usr/bin/env bash
#
# Knot-Server Agent-Skills Documentation Installer
#
# Installs the eight skill documentation files used by LLM agents to learn
# the knot-server REST API toolchain. The markdown payloads are base64-encoded
# inside this script and decoded on install.
#
# Requires: bash >= 3.2 (uses arrays, `wait`, `printf %b`, `set -o pipefail`).
#           base64  — standard on Linux, macOS, and the Windows Git Bash MSYS
#                     environment that OpenCode users typically run.
#
# Usage:
#   ./.knot-server-agent-skills.sh                       # install into ./.knot-server-agent-skills
#   ./.knot-server-agent-skills.sh /path/to/install/dir  # install elsewhere
#   ./.knot-server-agent-skills.sh --no-register         # skip the AI Agent prompt
#   curl -fsSL https://.../.knot-server-agent-skills.sh | bash -s -- --no-register
#
# After the files are extracted, the script prompts to register the skills
# with your AI Agent (global or project-local config). Use --no-register to skip
# the prompt (useful for non-interactive runs).

# Self-detect bash. The shebang above is the canonical entry point, but a
# user might still run `sh .knot-server-agent-skills.sh` (dash on Debian, ash on
# Alpine, …) which breaks the `set -o pipefail` and the `printf %b` output
# formatting below. Re-exec under bash so the rest of the script can rely
# on bash features without surprising the user.
if [ -z "${BASH_VERSION:-}" ]; then
  if command -v bash >/dev/null 2>&1; then
    exec bash "$0" "$@"
  else
    printf 'bash is required to run this installer.\n' >&2
    exit 1
  fi
fi

set -e
set -u
set -o pipefail

# --- Argument parsing ---------------------------------------------------------

NO_REGISTER=0
POSITIONAL=()
for arg in "$@"; do
  case "$arg" in
    --no-register)
      NO_REGISTER=1
      ;;
    -h|--help)
      cat <<'USAGE'
Usage: .knot-server-agent-skills.sh [TARGET_DIR] [--no-register]

Arguments:
  TARGET_DIR    Directory to extract the .md files into (default: .knot-server-agent-skills)
  --no-register Skip the AI Agent registration prompt
  -h, --help    Show this help message
USAGE
      exit 0
      ;;
    --)
      shift
      while [ $# -gt 0 ]; do
        POSITIONAL+=("$1")
        shift
      done
      ;;
    -*)
      printf 'Unknown option: %s\n' "$arg" >&2
      exit 2
      ;;
    *)
      POSITIONAL+=("$arg")
      ;;
  esac
done

TARGET_DIR="${POSITIONAL[0]:-.knot-server-agent-skills}"

# --- Colours (printf %b instead of echo -e) ----------------------------------

if [ -t 1 ] && command -v tput >/dev/null 2>&1 && [ -n "${TERM:-}" ] && [ "${TERM:-}" != "dumb" ]; then
  RED=$(printf '\033[0;31m')
  GREEN=$(printf '\033[0;32m')
  YELLOW=$(printf '\033[1;33m')
  BLUE=$(printf '\033[0;34m')
  CYAN=$(printf '\033[0;36m')
  BOLD=$(printf '\033[1m')
  NC=$(printf '\033[0m')
else
  RED=""
  GREEN=""
  YELLOW=""
  BLUE=""
  CYAN=""
  BOLD=""
  NC=""
fi

# --- Extraction --------------------------------------------------------------

if ! command -v base64 >/dev/null 2>&1; then
  printf '%b!%b base64 is not available on this system. Aborting.\n' "$YELLOW" "$NC" >&2
  exit 1
fi

mkdir -p "$TARGET_DIR"

printf '%b%b📦 Installing knot-server agent-skills documentation...%b\n' "$BLUE" "$BOLD" "$NC"
printf '   Destination: %b%s%b\n' "$GREEN" "$TARGET_DIR" "$NC"
printf '\n'

# Function: decode a base64 blob and write it to $TARGET_DIR/$filename.
# `printf %s` preserves the payload byte-for-byte (no shell interpretation).
write_file() {
  local filename="$1"
  local payload="$2"
  local outpath="$TARGET_DIR/$filename"
  local tmpfile
  tmpfile=$(mktemp)
  printf '%s' "$payload" | base64 -d > "$tmpfile"
  # Restore normal umask so the resulting file is readable. `mktemp` defaults
  # to 0600 which would make the installed skill invisible to other tools.
  chmod 0644 "$tmpfile"
  mv "$tmpfile" "$outpath"
  printf '   %b✓%b %s\n' "$GREEN" "$NC" "$filename"
}

"""

# Build the file-write block: one `write_file "name" "BASE64_BLOB"` per file.
# We use double-quoted strings because the base64 alphabet has no special
# characters for bash inside double quotes.
write_lines: list[str] = []
for fname in SKILL_FILES:
    src = REPO_ROOT / "skills" / fname
    if not src.exists():
        print(f"missing: {src}", file=sys.stderr)
        sys.exit(1)
    encoded = base64.b64encode(src.read_bytes()).decode("ascii")
    write_lines.append(f'write_file "{fname}" "{encoded}"')

# Also encode command files (TOML, etc.)
for cmd_relpath in COMMAND_FILES:
    src = REPO_ROOT / cmd_relpath
    if not src.exists():
        print(f"missing: {src}", file=sys.stderr)
        sys.exit(1)
    encoded = base64.b64encode(src.read_bytes()).decode("ascii")
    # Write to a commands/ subdirectory inside TARGET_DIR
    basename = cmd_relpath.split("/",1)[1]
    if cmd_relpath.startswith("commands/"):
        write_lines.append(f'mkdir -p "$TARGET_DIR/commands"')
        write_lines.append(f'write_file "commands/{basename}" "{encoded}"')
    else:
        # Claude skills go to a separate directory
        write_lines.append(f'mkdir -p "$TARGET_DIR/claude-skills"')
        write_lines.append(f'write_file "claude-skills/{basename}" "{encoded}"')

write_block = "\n".join(write_lines) + "\n\n"

# We need to be careful with the heredoc syntax for the skill JSON below.
# Use placeholders that do not collide with the f-string interpolator.
SCRIPT_FOOTER = r"""
# --- Done --------------------------------------------------------------------

printf '\n'
printf '%b%b✅ Installation complete!%b\n' "$GREEN" "$BOLD" "$NC"
printf '\n'
printf '📖 %bDocumentation files:%b\n' "$BLUE" "$NC"
ls -1 "$TARGET_DIR" | sed 's/^/   - /'
printf '\n'
printf '📚 %bGet started:%b\n' "$BLUE" "$NC"
printf '   cat %s/preflight.md\n' "$TARGET_DIR"
printf '   cat %s/workflows.md\n' "$TARGET_DIR"

# --- OpenCode registration prompt --------------------------------------------

if [ "$NO_REGISTER" = "1" ]; then
  exit 0
fi

# Skip the prompt if no terminal is available (e.g. CI/CD)
if [ ! -t 0 ] && [ ! -c /dev/tty ]; then
  exit 0
fi

printf '\n'
printf '%b%b🤖 AI Agent skill registration%b\n' "$CYAN" "$BOLD" "$NC"
printf 'The installed skill files can be registered to auto-load in your AI agent.\n'
printf 'Choose where to register them:\n'
printf '\n'
printf '   %b1)%b Universal   — copy to ~/.agents/skills/ (auto-loads in compatible agents)\n' "$BOLD" "$NC"
printf '   %b2)%b OpenCode    — global: ~/.config/opencode/ (skills + /index command)\n' "$BOLD" "$NC"
printf '   %b3)%b OpenCode    — project: ./.opencode/ (skills + /index command)\n' "$BOLD" "$NC"
printf '   %b4)%b Claude Code — global: ~/.claude/ (/index skill)\n' "$BOLD" "$NC"
printf '   %b5)%b Gemini CLI  — global: ~/.gemini/ (/index command)\n' "$BOLD" "$NC"
printf '   %b6)%b Skip        — for Cursor, Copilot, Codex (see README for setup)\n'
printf '\n'

# Read a single character without waiting for Enter.
# ESC key sends ^[ which is not a valid choice; treat it as skip.
choice=""
if [ -t 0 ]; then
  read -r -n 1 -p "Select [1/2/3/4/5/6]: " choice
else
  read -r -n 1 -p "Select [1/2/3/4/5/6]: " choice < /dev/tty
fi
printf '\n'
# Normalize ESC and other control characters to empty (skip)
if [[ "$choice" == $'\e' || "$choice" == $'\x1b' ]]; then
  choice=""
fi

# Resolve the absolute install directory once so that the resulting
# file:// URIs in opencode.json work no matter where the user runs the
# script from later.
ABS_SKILL_DIR=$(cd "$TARGET_DIR" && pwd)

register_with_python() {
  local target="$1"
  SKILL_DIR="$ABS_SKILL_DIR" TARGET_FILE="$target" python3 - <<'PYEOF' || return 1
import json, os, re, sys

target = os.environ["TARGET_FILE"]
base = os.environ["SKILL_DIR"]

mapping = [
    ("preflight.md", "knot-server-preflight", "MANDATORY STEP 0: Server health and index status check"),
    ("search.md",    "knot-server-search",    "Use knot-server for semantic code discovery across indexed repositories"),
    ("callers.md",   "knot-server-callers",   "Use knot-server to find reverse dependencies and perform impact analysis"),
    ("explore.md",   "knot-server-explore",   "Use knot-server to get a structural overview of a source file"),
    ("deps.md",      "knot-server-deps",      "Use knot-server to traverse the repository dependency graph"),
    ("graph.md",     "knot-server-graph",     "Use knot-server to query raw entity relationship subgraphs"),
    ("repos.md",     "knot-server-repos",     "Use knot-server to list, register, sync, and delete repositories"),
    ("workflows.md", "knot-server-workflows", "Multi-step knot-server workflows: impact analysis, cross-repo exploration, refactoring patterns"),
]

new_skills = {
    name: {"description": desc, "location": f"file://{base}/{fname}"}
    for fname, name, desc in mapping
}

# opencode.json files in the wild frequently contain `//` and `#` comments
# (the user's config is hand-edited) plus occasional trailing commas. Strip
# those before parsing so we do not refuse to edit otherwise-valid configs.
def _strip_jsonc(text: str) -> str:
    text = re.sub(r"^\s*//.*$", "", text, flags=re.MULTILINE)
    text = re.sub(r"\s+//[^\n]*", "", text)
    text = re.sub(r"^\s*#.*$", "", text, flags=re.MULTILINE)
    text = re.sub(r",(\s*[\]}])", r"\1", text)
    return text

try:
    with open(target, "r", encoding="utf-8") as f:
        raw = f.read()
    if not raw.strip():
        cfg = {"$schema": "https://opencode.ai/config.json"}
    else:
        cfg = json.loads(_strip_jsonc(raw))
except FileNotFoundError:
    cfg = {"$schema": "https://opencode.ai/config.json"}
except json.JSONDecodeError as exc:
    print(f"opencode.json is not valid JSON/JSONC: {exc}", file=sys.stderr)
    sys.exit(2)

if not isinstance(cfg, dict):
    cfg = {}

existing = cfg.get("skills", {})
if not isinstance(existing, dict):
    existing = {}

for name, val in new_skills.items():
    existing[name] = val

cfg["skills"] = existing

with open(target, "w", encoding="utf-8") as f:
    json.dump(cfg, f, indent=2)
    f.write("\n")
PYEOF
}

register_with_jq() {
  local target="$1"
  local tmp_skills tmp_cfg
  tmp_skills=$(mktemp)
  tmp_cfg=$(mktemp)

  cat > "$tmp_skills" <<EOFJSON
{
  "knot-server-preflight": { "description": "MANDATORY STEP 0: Server health and index status check", "location": "file://${ABS_SKILL_DIR}/preflight.md" },
  "knot-server-search": { "description": "Use knot-server for semantic code discovery across indexed repositories", "location": "file://${ABS_SKILL_DIR}/search.md" },
  "knot-server-callers": { "description": "Use knot-server to find reverse dependencies and perform impact analysis", "location": "file://${ABS_SKILL_DIR}/callers.md" },
  "knot-server-explore": { "description": "Use knot-server to get a structural overview of a source file", "location": "file://${ABS_SKILL_DIR}/explore.md" },
  "knot-server-deps": { "description": "Use knot-server to traverse the repository dependency graph", "location": "file://${ABS_SKILL_DIR}/deps.md" },
  "knot-server-graph": { "description": "Use knot-server to query raw entity relationship subgraphs", "location": "file://${ABS_SKILL_DIR}/graph.md" },
  "knot-server-repos": { "description": "Use knot-server to list, register, sync, and delete repositories", "location": "file://${ABS_SKILL_DIR}/repos.md" },
  "knot-server-workflows": { "description": "Multi-step knot-server workflows: impact analysis, cross-repo exploration, refactoring patterns", "location": "file://${ABS_SKILL_DIR}/workflows.md" }
}
EOFJSON

  if [ ! -f "$target" ]; then
    printf '{\n  "$schema": "https://opencode.ai/config.json",\n  "skills": {}\n}\n' > "$target"
  fi

  # Merge the new skills object into the existing skills object
  jq --slurpfile new "$tmp_skills" '
    .skills = ((.skills // {}) + $new[0])
  ' "$target" > "$tmp_cfg"
  mv "$tmp_cfg" "$target"
  rm -f "$tmp_skills"
}

register_to() {
  local target="$1"
  if [ -z "$target" ]; then
    return 1
  fi

  mkdir -p "$(dirname "$target")"

  if command -v python3 >/dev/null 2>&1; then
    if register_with_python "$target"; then
      printf '   %b✓%b Registered in %s\n' "$GREEN" "$NC" "$target"
      return 0
    fi
  fi

  if command -v jq >/dev/null 2>&1; then
    register_with_jq "$target"
    printf '   %b✓%b Registered in %s\n' "$GREEN" "$NC" "$target"
    return 0
  fi

  printf '   %b!%b Could not register automatically: neither python3 nor jq is installed.\n' "$YELLOW" "$NC"
  printf '   Please add the following to %s:\n' "$target"
  printf '\n'
  cat <<MANUAL
  "skills": {
    "knot-server-preflight": { "description": "MANDATORY STEP 0: Server health and index status check", "location": "file://${ABS_SKILL_DIR}/preflight.md" },
    "knot-server-search": { "description": "Use knot-server for semantic code discovery across indexed repositories", "location": "file://${ABS_SKILL_DIR}/search.md" },
    "knot-server-callers": { "description": "Use knot-server to find reverse dependencies and perform impact analysis", "location": "file://${ABS_SKILL_DIR}/callers.md" },
    "knot-server-explore": { "description": "Use knot-server to get a structural overview of a source file", "location": "file://${ABS_SKILL_DIR}/explore.md" },
    "knot-server-deps": { "description": "Use knot-server to traverse the repository dependency graph", "location": "file://${ABS_SKILL_DIR}/deps.md" },
    "knot-server-graph": { "description": "Use knot-server to query raw entity relationship subgraphs", "location": "file://${ABS_SKILL_DIR}/graph.md" },
    "knot-server-repos": { "description": "Use knot-server to list, register, sync, and delete repositories", "location": "file://${ABS_SKILL_DIR}/repos.md" },
    "knot-server-workflows": { "description": "Multi-step knot-server workflows: impact analysis, cross-repo exploration, refactoring patterns", "location": "file://${ABS_SKILL_DIR}/workflows.md" }
  }
MANUAL
}

# Install OpenCode directory-based skills (SKILL.md format).
# This enables the skill tool to discover and load skills on demand.
install_opencode_skills() {
  local skills_dir="$1"
  local skill_names=(
    "knot-server-preflight"
    "knot-server-search"
    "knot-server-callers"
    "knot-server-explore"
    "knot-server-deps"
    "knot-server-graph"
    "knot-server-repos"
    "knot-server-workflows"
  )
  local skill_files=(
    "preflight.md"
    "search.md"
    "callers.md"
    "explore.md"
    "deps.md"
    "graph.md"
    "repos.md"
    "workflows.md"
  )
  local skill_descriptions=(
    "MANDATORY STEP 0: Server health and index status check"
    "Use knot-server for semantic code discovery across indexed repositories"
    "Use knot-server to find reverse dependencies and perform impact analysis"
    "Use knot-server to get a structural overview of a source file"
    "Use knot-server to traverse the repository dependency graph"
    "Use knot-server to query raw entity relationship subgraphs"
    "Use knot-server to list, register, sync, and delete repositories"
    "Multi-step knot-server workflows: impact analysis, cross-repo exploration, refactoring patterns"
  )

  mkdir -p "$skills_dir"
  for i in "${!skill_names[@]}"; do
    local name="${skill_names[$i]}"
    local src_file="${skill_files[$i]}"
    local desc="${skill_descriptions[$i]}"
    local skill_dir="$skills_dir/$name"
    local skill_md="$skill_dir/SKILL.md"

    mkdir -p "$skill_dir"
    # Write YAML frontmatter + content from the extracted .md file
    {
      printf -- "---\n"
      printf "name: %s\n" "$name"
      printf "description: %s\n" "$desc"
      printf -- "---\n\n"
      cat "$TARGET_DIR/$src_file"
    } > "$skill_md"
    printf '   %b✓%b %s/SKILL.md\n' "$GREEN" "$NC" "$name"
  done
}

# Install OpenCode command: /index
# Commands go in ~/.config/opencode/commands/ (global) or .opencode/commands/ (project)
install_opencode_command() {
  local commands_dir="$1"
  mkdir -p "$commands_dir"
  cp "$TARGET_DIR/index.md" "$commands_dir/index.md"
  printf '   %b✓%b %s/index.md (command /index)\n' "$GREEN" "$NC" "$commands_dir"
}

# Install Gemini CLI command: /index
# Commands go in ~/.gemini/commands/ (global) or .gemini/commands/ (project)
install_gemini_command() {
  local commands_dir="$1"
  mkdir -p "$commands_dir"
  cp "$TARGET_DIR/commands/index.toml" "$commands_dir/index.toml"
  printf '   %b✓%b %s/index.toml (command /index)\n' "$GREEN" "$NC" "$commands_dir"
}

# Install Claude Code skill: /index
# Skills go in ~/.claude/skills/<name>/SKILL.md (global) or .claude/skills/<name>/SKILL.md (project)
install_claude_skill() {
  local skills_dir="$1"
  local skill_dir="$skills_dir/knot-server-index"
  mkdir -p "$skill_dir"
  cp "$TARGET_DIR/claude-skills/claude-index.md" "$skill_dir/SKILL.md"
  printf '   %b✓%b %s/SKILL.md (command /index)\n' "$GREEN" "$NC" "$skill_dir"
}

case "$choice" in
  1)
    mkdir -p "$HOME/.agents/skills"
    cp "$TARGET_DIR"/*.md "$HOME/.agents/skills/"
    printf '   %b✓%b Copied to %s\n' "$GREEN" "$NC" "$HOME/.agents/skills/"
    ;;
  2)
    register_to "$HOME/.config/opencode/opencode.json"
    printf '\n   %bInstalling OpenCode directory-based skills...%b\n' "$YELLOW" "$NC"
    install_opencode_skills "$HOME/.config/opencode/skills"
    printf '\n   %bInstalling OpenCode commands...%b\n' "$YELLOW" "$NC"
    install_opencode_command "$HOME/.config/opencode/commands"
    ;;
  3)
    register_to "./opencode.json"
    printf '\n   %bInstalling OpenCode directory-based skills...%b\n' "$YELLOW" "$NC"
    install_opencode_skills "./.opencode/skills"
    printf '\n   %bInstalling OpenCode commands...%b\n' "$YELLOW" "$NC"
    install_opencode_command "./.opencode/commands"
    ;;
  4)
    printf '\n   %bInstalling Claude Code skill...%b\n' "$YELLOW" "$NC"
    install_claude_skill "$HOME/.claude/skills"
    ;;
  5)
    printf '\n   %bInstalling Gemini CLI command...%b\n' "$YELLOW" "$NC"
    install_gemini_command "$HOME/.gemini/commands"
    ;;
  6|"")
    printf '   Skipped.\n'
    ;;
  *)
    printf '   %b!%b Unknown choice; skipping.\n' "$YELLOW" "$NC" >&2
    ;;
esac

printf '\n'
"""


def main() -> None:
    out = REPO_ROOT / ".knot-server-agent-skills.sh"
    parts = [SCRIPT_HEADER, write_block, SCRIPT_FOOTER]
    out.write_text("".join(parts))
    out.chmod(0o755)
    print(f"wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
