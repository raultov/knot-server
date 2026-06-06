#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
    cat <<'EOF'
Usage: up.sh [OPTIONS]

Start the knot-server stack (knot-server, Qdrant, Neo4j) via Docker Compose.

All arguments are forwarded directly to `docker compose up`, so any standard
Docker Compose flag works (-d, --build, --force-recreate, etc.).

Environment Variables
  KNOT_SERVER_PORT       Port for the REST API (default: 3000)
  KNOT_LOCAL_REPOS_DIR   Host directory of local repos to mount read-only
  KNOT_SSH_KEYS_DIR      SSH key directory to copy into the container

Examples
  up.sh                          Start in foreground
  up.sh -d                       Start in detached (background) mode
  KNOT_SERVER_PORT=6060 up.sh -d Start on port 6060, detached
  up.sh --build                  Rebuild images before starting
  up.sh --force-recreate         Recreate containers even if unchanged

EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

if ! command -v docker &> /dev/null || ! docker compose version &> /dev/null; then
    echo "Error: Docker Compose is not available" >&2
    exit 1
fi

cd "${SCRIPT_DIR}"
exec docker compose up "$@"
