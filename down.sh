#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_DIR="${SCRIPT_DIR}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

CLEAN=false
REMOVE_JSON=false
REMOVE_REPOS=false

usage() {
    cat <<'EOF'
Usage: down.sh [OPTIONS]

Stop the knot-server stack and optionally clean up persisted data.

By default, containers are stopped but all data (Docker volumes, repository
registry, cloned repos) is preserved so you can resume where you left off.

Options
  -c, --clean     Delete Docker volumes (Qdrant, Neo4j, workspace, fastembed cache).
                  The databases are wiped — next start will be empty.
  -j, --json      Delete ~/.knot/repos/repos.json and its lock file.
                  This removes the repository registry; the server will forget
                  all registered repos.
  -r, --repos     Delete all cloned repositories under ~/.knot/repos/.
                  The .knot/ metadata directory and fastembed_cache/ are kept.
                  You will need to re-register and re-clone repos on next start.
  -a, --all       Shorthand for -c -j -r. Full factory reset.
  -h, --help      Show this help message.

Examples
  down.sh                  Stop containers, keep everything
  down.sh --clean          Stop containers, wipe databases
  down.sh --json           Stop containers, forget registered repos
  down.sh --all            Stop containers, wipe databases, forget repos, delete clones

EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -c|--clean)
            CLEAN=true
            shift
            ;;
        -j|--json)
            REMOVE_JSON=true
            shift
            ;;
        -r|--repos)
            REMOVE_REPOS=true
            shift
            ;;
        -a|--all)
            CLEAN=true
            REMOVE_JSON=true
            REMOVE_REPOS=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage
            exit 1
            ;;
    esac
done

if ! command -v docker &> /dev/null || ! docker compose version &> /dev/null; then
    echo -e "${RED}Error: Docker Compose is not available${NC}" >&2
    exit 1
fi

echo -e "${YELLOW}Stopping containers...${NC}"
cd "${COMPOSE_DIR}"
docker compose down --remove-orphans

if [ "${CLEAN}" = true ]; then
    echo ""
    echo -e "${YELLOW}Deleting Docker volumes...${NC}"
    docker volume rm \
        knot-server_knot_workspace \
        knot-server_qdrant_data \
        knot-server_neo4j_data \
        knot-server_fastembed_cache \
        downloads_knot_workspace \
        2>/dev/null || true
    echo -e "${GREEN}Volumes deleted.${NC}"
fi

if [ "${REMOVE_JSON}" = true ]; then
    echo ""
    echo -e "${YELLOW}Deleting repository registry...${NC}"
    REPOS_DIR="${HOME}/.knot/repos"
    REPOS_JSON="${REPOS_DIR}/repos.json"

    if [ -f "${REPOS_JSON}" ]; then
        rm -f "${REPOS_JSON}"
        echo -e "${GREEN}Deleted: ${REPOS_JSON}${NC}"
    else
        echo "  (repos.json not found, skipping)"
    fi

    if [ -f "${REPOS_DIR}/repos.json.lock" ]; then
        rm -f "${REPOS_DIR}/repos.json.lock"
        echo -e "${GREEN}Deleted: ${REPOS_DIR}/repos.json.lock${NC}"
    fi
fi

if [ "${REMOVE_REPOS}" = true ]; then
    echo ""
    echo -e "${YELLOW}Deleting cloned repositories...${NC}"
    REPOS_DIR="${HOME}/.knot/repos"

    if [ -d "${REPOS_DIR}" ]; then
        # Keep the directory itself and .knot subdirectory, remove everything else
        find "${REPOS_DIR}" -maxdepth 1 -mindepth 1 ! -name '.knot' ! -name 'fastembed_cache' -exec rm -rf {} +
        echo -e "${GREEN}Deleted all cloned repositories in ${REPOS_DIR}${NC}"
    else
        echo "  (${REPOS_DIR} not found, skipping)"
    fi
fi

echo ""
echo -e "${GREEN}Done.${NC}"
