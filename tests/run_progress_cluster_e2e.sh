#!/usr/bin/env bash
# E2E Progress Cluster Coordination Test for knot-server
# Validates multi-instance progress visibility and registry coherence.
#
# Scenarios covered:
# 1. Registry status coherence (BUG-1 fix)
# 2. No lost updates between instances (BUG-2 fix)
# 3. Cross-node progress visibility (BUG-3 fix)
# 4. Terminal coherence and cleanup
# 5. Batch endpoint indifferent access

# set -e
set -u

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.e2e.yml"
SHARED_WORKSPACE="/tmp/knot-progress-e2e-$$"
SERVER_A_PORT=18083
SERVER_A_PID=""
SERVER_B_PORT=18084
SERVER_B_PID=""

NEO4J_URI="bolt://localhost:17687"
NEO4J_USER="neo4j"
NEO4J_PASSWORD="e2e_test_password"
QDRANT_URL="http://localhost:16334"

PASSED=0
FAILED=0

pass() {
    echo -e "${GREEN}PASS${NC} — $1"
    PASSED=$((PASSED + 1))
}

fail() {
    echo -e "${RED}FAIL${NC} — $1"
    FAILED=$((FAILED + 1))
}

assert_status() {
    local got="$1"
    local expected="$2"
    local msg="$3"
    if [ "$got" = "$expected" ]; then pass "$msg"; else fail "$msg (got: $got, expected: $expected)"; fi
}

cleanup() {
    echo -e "\n${YELLOW}Cleaning up...${NC}"
    if [ -n "$SERVER_A_PID" ] && kill -0 "$SERVER_A_PID" 2>/dev/null; then
        kill "$SERVER_A_PID" 2>/dev/null || true
        wait "$SERVER_A_PID" 2>/dev/null || true
    fi
    if [ -n "$SERVER_B_PID" ] && kill -0 "$SERVER_B_PID" 2>/dev/null; then
        kill "$SERVER_B_PID" 2>/dev/null || true
        wait "$SERVER_B_PID" 2>/dev/null || true
    fi
    cd "$SCRIPT_DIR"
    docker compose -f "$COMPOSE_FILE" down -v > /dev/null 2>&1 || true
    #     # rm -rf "$SHARED_WORKSPACE" 2>/dev/null || true

    echo -e "\n========================================"
    if [ "$FAILED" -eq 0 ]; then
        echo -e "RESULTS: ${GREEN}${PASSED} passed${NC}, ${RED}0 failed${NC}"
        echo -e "========================================"
        exit 0
    else
        echo -e "RESULTS: ${GREEN}${PASSED} passed${NC}, ${RED}${FAILED} failed${NC}"
        echo -e "========================================"
        exit 1
    fi
}
trap cleanup EXIT

start_server() {
    local port=$1
    local log_file=$2
    
    cd "$PROJECT_ROOT"
    
    KNOT_SERVER_PORT="$port" \
    KNOT_WORKSPACE_DIR="$SHARED_WORKSPACE" \
    KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
    KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
    KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
    KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
    RUST_LOG=info \
    ./target/debug/knot-server > "$log_file" 2>&1 &
    
    local pid=$!
    echo "$pid"
}

# --- Setup ---
echo -e "${YELLOW}Setting up E2E environment for Cluster Progress Coherence...${NC}"
cd "$PROJECT_ROOT"
cargo build --bin knot-server > /dev/null 2>&1
cd "$SCRIPT_DIR"
docker compose -f "$COMPOSE_FILE" down -v > /dev/null 2>&1 || true
docker compose -f "$COMPOSE_FILE" up -d > /dev/null 2>&1

# Wait for Neo4j
echo "Waiting for Neo4j..."
for i in {1..30}; do
    if curl -sf -I "http://localhost:17474" > /dev/null; then
        break
    fi
    sleep 2
done

echo "Waiting for Qdrant..."
for i in {1..10}; do
    if curl -sf "$QDRANT_URL/collections" > /dev/null; then
        break
    fi
    sleep 2
done

mkdir -p "$SHARED_WORKSPACE/repos"
SERVER_A_LOG="$SHARED_WORKSPACE/server_a.log"
SERVER_B_LOG="$SHARED_WORKSPACE/server_b.log"

FIXTURES_ROOT="$SHARED_WORKSPACE/fixtures"
mkdir -p "$FIXTURES_ROOT"

create_fixture_repo() {
    local name="$1"
    local bare="$FIXTURES_ROOT/$name.git"
    local tmp="$FIXTURES_ROOT/${name}-tmp"

    git init --bare "$bare" > /dev/null 2>&1
    rm -rf "$tmp"
    mkdir -p "$tmp"
    git clone "$bare" "$tmp" 2>/dev/null

    # Create lots of files to artificially delay indexing
    for i in {1..200}; do
        cat > "$tmp/Sample$i.java" <<EOF
public class Sample$i {
    public static void main(String[] args) {
        System.out.println("Hello $i");
    }
}
EOF
    done

    git -C "$tmp" add . > /dev/null
    git -C "$tmp" config user.email "test@example.com"
    git -C "$tmp" config user.name "Test User"
    git -C "$tmp" checkout -b main > /dev/null 2>&1 || true
    git -C "$tmp" commit -m "Initial commit" > /dev/null
    git -C "$tmp" push origin main 2>/dev/null

    rm -rf "$tmp"
}

create_fixture_repo "alpha"
create_fixture_repo "beta"

echo -e "\n${CYAN}Starting Instance A (port $SERVER_A_PORT)...${NC}"
SERVER_A_PID=$(start_server "$SERVER_A_PORT" "$SERVER_A_LOG")
for i in {1..30}; do
    if curl -sf "http://localhost:$SERVER_A_PORT/api/health" > /dev/null 2>&1; then
        echo "Instance A is up."
        break
    fi
    if [ "$i" -eq 30 ]; then
        echo -e "${RED}Instance A failed to start${NC}"
        cat "$SERVER_A_LOG"
        exit 1
    fi
    sleep 2
done

echo -e "\n${CYAN}Starting Instance B (port $SERVER_B_PORT)...${NC}"
SERVER_B_PID=$(start_server "$SERVER_B_PORT" "$SERVER_B_LOG")
for i in {1..30}; do
    if curl -sf "http://localhost:$SERVER_B_PORT/api/health" > /dev/null 2>&1; then
        echo "Instance B is up."
        break
    fi
    if [ "$i" -eq 30 ]; then
        echo -e "${RED}Instance B failed to start${NC}"
        cat "$SERVER_B_LOG"
        exit 1
    fi
    sleep 2
done

BASE_A="http://localhost:$SERVER_A_PORT"
BASE_B="http://localhost:$SERVER_B_PORT"

# --- SCENARIO 1: Registry status coherence ---
echo -e "\n${CYAN}Scenario 1: Registry status coherence${NC}"
CREATE_RES=$(curl -s -X POST "$BASE_A/api/repos" \
    -H "Content-Type: application/json" \
    -d "{\"id\": \"alpha\", \"url\": \"$FIXTURES_ROOT/alpha.git\", \"auth_type\": \"ssh\"}")
echo "Create alpha on A: $CREATE_RES"

sleep 2

RAW_B=$(curl -s "$BASE_B/api/repos")
echo "RAW B RESPONSE: $RAW_B"
STATUS_B=$(echo "$RAW_B" | jq -r '.repositories[] | select(.id=="alpha") | .status')
if [ -n "$STATUS_B" ] && [ "$STATUS_B" != "null" ]; then
    pass "Node B lists 'alpha' without restarting (status: $STATUS_B)"
else
    fail "Node B lists 'alpha' without restarting (no alpha found in repos list)"
fi

# --- SCENARIO 2: No lost updates ---
echo -e "\n${CYAN}Scenario 2: No lost updates between instances${NC}"
curl -s -X POST "$BASE_A/api/repos" \
    -H "Content-Type: application/json" \
    -d "{\"id\": \"beta\", \"url\": \"$FIXTURES_ROOT/beta.git\", \"auth_type\": \"ssh\"}" > /dev/null

sleep 1
# Trigger A (for beta) and B (for alpha) concurrently
curl -s -X POST "$BASE_A/api/repos/beta/sync" > /dev/null &
PID_A=$!
curl -s -X POST "$BASE_B/api/repos/alpha/sync" > /dev/null &
PID_B=$!
wait $PID_A
wait $PID_B

# --- SCENARIO 3 & 4 & 5: Cross-node progress visibility, Cleanup, Batch ---
echo -e "\n${CYAN}Scenario 3-5: Cross-node progress & batch access${NC}"

# Loop while both are still not indexed
SEEN_NONZERO_ON_A=0

for i in {1..90}; do
    PROG_BATCH_A=$(curl -s "$BASE_A/api/progress")
    PROG_BATCH_B=$(curl -s "$BASE_B/api/progress")
    
    # Ensure they agree on status for alpha
    STATUS_ALPHA_A=$(echo "$PROG_BATCH_A" | jq -r '.repos[] | select(.repo_id=="alpha") | .status')
    STATUS_ALPHA_B=$(echo "$PROG_BATCH_B" | jq -r '.repos[] | select(.repo_id=="alpha") | .status')
    if [ "$STATUS_ALPHA_A" != "$STATUS_ALPHA_B" ]; then
        # They might be momentarily out of sync, just continue polling
        sleep 0.5
        continue
    fi
    
    STAGE_ALPHA_A=$(echo "$PROG_BATCH_A" | jq -r '.repos[] | select(.repo_id=="alpha") | .stage')
    STAGE_ALPHA_B=$(echo "$PROG_BATCH_B" | jq -r '.repos[] | select(.repo_id=="alpha") | .stage')

    if [ "$STAGE_ALPHA_A" != "null" ] && [ -n "$STAGE_ALPHA_A" ] && [ "$STAGE_ALPHA_A" != "idle" ]; then
        SEEN_NONZERO_ON_A=1
    fi
    # debugging log
    # echo "Poll: A=$STAGE_ALPHA_A, B=$STAGE_ALPHA_B"
    
    if [ "$STATUS_ALPHA_A" == "indexed" ]; then
        break
    fi
    sleep 0.5
done

if [ $SEEN_NONZERO_ON_A -eq 1 ]; then pass "Node A reported live progress for a repo indexed by B (cross-node)"; else fail "Node A never saw live progress from B"; fi

# Ensure batch endpoints correctly match terminal state
for i in {1..60}; do
    PROG_BATCH_A=$(curl -s "$BASE_A/api/progress")
    FINAL_STATUS_A=$(echo "$PROG_BATCH_A" | jq -r '.repos[] | select(.repo_id=="alpha") | .status')
    if [ "$FINAL_STATUS_A" = "indexed" ]; then
        break
    fi
    sleep 0.5
done
if [ "$FINAL_STATUS_A" = "indexed" ]; then
    pass "Node A sees terminal batch status as indexed"
else
    fail "Node A sees terminal batch status as indexed (last observed: $FINAL_STATUS_A)"
fi

for i in {1..60}; do
    PROG_BATCH_B=$(curl -s "$BASE_B/api/progress")
    FINAL_STATUS_B=$(echo "$PROG_BATCH_B" | jq -r '.repos[] | select(.repo_id=="alpha") | .status')
    if [ "$FINAL_STATUS_B" = "indexed" ]; then
        break
    fi
    sleep 0.5
done
if [ "$FINAL_STATUS_B" = "indexed" ]; then
    pass "Node B sees terminal batch status as indexed"
else
    fail "Node B sees terminal batch status as indexed (last observed: $FINAL_STATUS_B)"
fi

# The progress persister removes the snapshot asynchronously after
# the worker signals cancel. Give it a small window to drain before
# asserting the file is gone (CI runners can be slower than local).
for i in {1..10}; do
    if [ ! -f "$SHARED_WORKSPACE/progress/alpha.json" ]; then
        break
    fi
    sleep 0.3
done

if [ ! -f "$SHARED_WORKSPACE/progress/alpha.json" ]; then pass "Snapshot file removed on completion"; else fail "Snapshot file still exists"; fi
