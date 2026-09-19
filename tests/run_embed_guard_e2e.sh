#!/usr/bin/env bash
# E2E regression test for the marker-aware startup guard (PLAN §S7).
#
# The failure this prevents is *silent*: a server rebuilt against another
# embedding model starts happily against a stale same-named collection and
# only fails later with an opaque "wrong vector size" error — or, with a
# same-dimension model, never at all (only degraded recall). The guard must
# abort at startup with the collection name, both dimensions and both models
# named in the message.
#
# Scenarios:
#   1. Backward-compat pin: a fresh 384-dim `knot_entities` collection with
#      NO embedding env vars starts ready and /api/health reports the derived
#      MiniLM/384 model and collection — a v0.7.0 operator's upgrade path.
#   2. BGE-base opted in (`KNOT_EMBED_MODEL=BGEBaseENV15`) against the
#      explicit 384-dim collection must ABORT with an actionable message.
#   3. BGE-base with NO explicit collection must derive `knot_entities_bge768`
#      (C2 regression): the guard's abort must name the derived collection, not
#      the base one.

set -e
set -u

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.e2e.yml"
WORKSPACE_DIR="/tmp/knot-guard-e2e-workspace-$$"
SERVER_PORT=18081
BASE_URL="http://localhost:$SERVER_PORT"
SERVER_LOG="/tmp/knot-server-guard-e2e.log"
SERVER_PID=""

# Pre-flight: a leaked knot-server on the port would make every check pass
# against the WRONG process (its config happens to be compatible).
if nc -z localhost "$SERVER_PORT" 2>/dev/null; then
    echo -e "${RED}Port $SERVER_PORT already in use (leaked knot-server? kill it first)${NC}" >&2
    exit 1
fi

NEO4J_URI="bolt://localhost:17687"
NEO4J_USER="neo4j"
NEO4J_PASSWORD="e2e_test_password"
QDRANT_URL="http://localhost:16334"
QDRANT_HTTP="http://localhost:16333"

fail() {
    echo -e "${RED}FAIL: $1${NC}" >&2
    exit 1
}

cleanup() {
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    cd "$SCRIPT_DIR"
    docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
    rm -rf "$WORKSPACE_DIR" 2>/dev/null || true
    cp "$SERVER_LOG" "$SCRIPT_DIR/.last-guard-e2e-server.log" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

wait_for_port() {
    local port="$1"
    local label="$2"
    local max_wait="${3:-60}"
    for i in $(seq 1 "$max_wait"); do
        if nc -z localhost "$port" 2>/dev/null; then
            echo -e "  ${GREEN}$label ready${NC}"
            return 0
        fi
        sleep 1
    done
    echo -e "  ${RED}$label timeout${NC}" >&2
    return 1
}

start_server() {
    KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
    KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
    KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
    KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
    KNOT_SERVER_PORT="$SERVER_PORT" \
    KNOT_WORKSPACE_DIR="$WORKSPACE_DIR" \
    RUST_LOG="${RUST_LOG:-info}" \
    "$PROJECT_ROOT/target/debug/knot-server" >"$SERVER_LOG" 2>&1 &
    SERVER_PID=$!
}

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Start-up Embed Guard E2E Test${NC}"
echo -e "${GREEN}========================================${NC}"

echo -e "${YELLOW}[1/6] Starting Docker containers...${NC}"
cd "$SCRIPT_DIR"
docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
docker compose -f "$COMPOSE_FILE" up -d
wait_for_port 17687 "Neo4j" 60 || fail "neo4j"
wait_for_port 16334 "Qdrant (gRPC)" 30 || fail "qdrant"
wait_for_port 16333 "Qdrant (REST)" 30 || fail "qdrant"
echo -n "  Waiting for Neo4j health check..."
for i in $(seq 1 60); do
    STATUS=$(docker inspect --format='{{.State.Health.Status}}' knot_server_neo4j_e2e 2>/dev/null || echo "unknown")
    if [ "$STATUS" = "healthy" ]; then
        echo -e "  ${GREEN}healthy${NC}"
        break
    fi
    [ "$i" -eq 60 ] && { echo -e "  ${RED}timeout (status: $STATUS)${NC}"; exit 1; }
    sleep 1
done
sleep 3

echo -e "${YELLOW}[2/6] Building server + creating a 384-dim collection...${NC}"
cd "$PROJECT_ROOT"
cargo build 2>&1 | grep -E "(Compiling|Finished|error)" || true

rm -rf "$WORKSPACE_DIR"
mkdir -p "$WORKSPACE_DIR"
mkdir -p /tmp/fastembed_cache_shared
ln -s /tmp/fastembed_cache_shared "$WORKSPACE_DIR/fastembed_cache"

curl -sf -X PUT "$QDRANT_HTTP/collections/knot_entities" \
    -H 'content-type: application/json' \
    -d '{"vectors": {"size": 384, "distance": "Cosine"}}' >/dev/null \
    || fail "could not create the 384-dim knot_entities collection"

echo -e "${YELLOW}[3/6] Scenario 1: MiniLM default against the 384-dim collection${NC}"
start_server
OK=1
for i in $(seq 1 120); do
    if curl -sf "$BASE_URL/api/health" >/dev/null 2>&1; then
        OK=0
        break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        wait "$SERVER_PID" 2>/dev/null; SERVER_PID=""
        fail "server exited while loading the default model: $(cat "$SERVER_LOG")"
    fi
    sleep 1
done
[ "$OK" -eq 0 ] || fail "server did not start with the default model: $(cat "$SERVER_LOG")"
HEALTH=$(curl -sf "$BASE_URL/api/health" || true)
kill "$SERVER_PID"; wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
echo "$HEALTH" | grep -q '"embed_model":"AllMiniLML6V2"' || fail "health must report the model: $HEALTH"
echo "$HEALTH" | grep -q '"embed_dim":384' || fail "health must report the derived dimension: $HEALTH"
echo "$HEALTH" | grep -q '"qdrant_collection":"knot_entities"' || fail "health must report the collection: $HEALTH"
echo -e "  ${GREEN}Scenario 1 PASSED: zero re-index, zero config change${NC}"

echo -e "${YELLOW}[4/6] Scenario 2: opt-in BGE model against the 384-dim explicit collection${NC}"
KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
KNOT_SERVER_PORT="$SERVER_PORT" \
KNOT_SERVER_QDRANT_COLLECTION="knot_entities" \
KNOT_WORKSPACE_DIR="$WORKSPACE_DIR" \
KNOT_EMBED_MODEL="BGEBaseENV15" \
RUST_LOG="${RUST_LOG:-info}" \
    "$PROJECT_ROOT/target/debug/knot-server" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

set +e
wait "$SERVER_PID"
EXIT_CODE=$?
set -e
SERVER_PID=""

if [ "$EXIT_CODE" -eq 0 ]; then
    fail "server must abort when the configured model does not match the collection's dimension"
fi

# The abort message must name the collection, both dimensions and both models.
for token in "knot_entities" "384" "768" "BGEBaseENV15"; do
    grep -q "$token" "$SERVER_LOG" || fail "abort message must name '$token': $(cat "$SERVER_LOG")"
done
# H1: the remediation must speak knot-server's language, not knot's. The server
# reads KNOT_SERVER_QDRANT_COLLECTION and never runs knot-indexer.
grep -q "KNOT_SERVER_QDRANT_COLLECTION" "$SERVER_LOG" \
    || fail "abort must name the server's collection variable: $(cat "$SERVER_LOG")"
if grep -q "knot-indexer" "$SERVER_LOG"; then
    fail "abort must not tell a server operator to run knot-indexer: $(cat "$SERVER_LOG")"
fi
echo -e "  ${GREEN}Scenario 2 PASSED: abort names collection, dimensions and model${NC}"

echo -e "${YELLOW}[5/6] Scenario 3: BGE with no explicit collection must derive knot_entities_bge768${NC}"
# C2 regression. The derived collection is the one the model's suffix is
# applied to; a dead derivation would instead probe `knot_entities` (the
# 384-dim collection from scenario 1) and the abort would name that.
#
# To observe the probed name without loading BGE (a ~400 MB download), the
# derived collection is pre-created at a deliberately wrong dimension: the
# guard aborts before the embedder is initialised, and its message names the
# collection it probed.
curl -sf -X PUT "$QDRANT_HTTP/collections/knot_entities_bge768" \
    -H 'content-type: application/json' \
    -d '{"vectors": {"size": 384, "distance": "Cosine"}}' >/dev/null \
    || fail "could not create the 384-dim knot_entities_bge768 collection"

# An explicit collection from the parent environment would defeat the test.
unset KNOT_SERVER_QDRANT_COLLECTION
KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
KNOT_SERVER_PORT="$SERVER_PORT" \
KNOT_WORKSPACE_DIR="$WORKSPACE_DIR" \
KNOT_EMBED_MODEL="BGEBaseENV15" \
RUST_LOG="${RUST_LOG:-info}" \
    "$PROJECT_ROOT/target/debug/knot-server" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

set +e
wait "$SERVER_PID"
EXIT_CODE=$?
set -e
SERVER_PID=""

if [ "$EXIT_CODE" -eq 0 ]; then
    fail "server must abort against the deliberately mismatched derived collection"
fi

# Assert the *probed* collection name (quoted in the guard message), not just
# the suggested default — the message mentions the derived name either way.
grep -q "collection 'knot_entities_bge768'" "$SERVER_LOG" \
    || fail "collection derivation is dead — abort probed the wrong collection: $(cat "$SERVER_LOG")"
echo -e "  ${GREEN}Scenario 3 PASSED: BGE derives knot_entities_bge768${NC}"

echo -e "${YELLOW}[6/6] Done${NC}"
echo -e "${GREEN}All guard E2E tests passed!${NC}"
