#!/usr/bin/env bash
# E2E Cluster Coordination Test for knot-server
# Validates multi-instance coordination: stale lock detection, cleanup,
# and automatic re-enqueue by the surviving node when a peer crashes.
#
# Scenario:
#   1. Start Instance A, register + index repos
#   2. Start Instance B (shared workspace)
#   3. Force A to acquire a lock, then kill -9 A
#   4. Verify B detects the stale lock, cleans it, and re-processes the repo

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
SHARED_WORKSPACE="/tmp/knot-cluster-e2e-$$"
SERVER_A_PORT=18081
SERVER_A_PID=""
SERVER_B_PORT=18082
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

assert_file_exists() {
    if [ -f "$1" ]; then pass "$2"; else fail "$2 (file not found: $1)"; fi
}

assert_file_not_exists() {
    if [ ! -f "$1" ]; then pass "$2"; else fail "$2 (file still exists: $1)"; fi
}

assert_status() {
    local got="$1"
    local expected="$2"
    local msg="$3"
    if [ "$got" = "$expected" ]; then pass "$msg"; else fail "$msg (got: $got, expected: $expected)"; fi
}

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local msg="$3"
    if echo "$haystack" | grep -q "$needle"; then pass "$msg"; else fail "$msg (not found)"; fi
}

cleanup() {
    local exit_code=$?
    if [ -n "$SERVER_A_PID" ] && kill -0 "$SERVER_A_PID" 2>/dev/null; then
        kill "$SERVER_A_PID" 2>/dev/null || true
        wait "$SERVER_A_PID" 2>/dev/null || true
    fi
    if [ -n "$SERVER_B_PID" ] && kill -0 "$SERVER_B_PID" 2>/dev/null; then
        kill "$SERVER_B_PID" 2>/dev/null || true
        wait "$SERVER_B_PID" 2>/dev/null || true
    fi
    cd "$SCRIPT_DIR"
    docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
    rm -rf "$SHARED_WORKSPACE" 2>/dev/null || true
    exit "$exit_code"
}

trap cleanup EXIT

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}knot-server Cluster Coordination E2E${NC}"
echo -e "${GREEN}========================================${NC}"

# ── Step 0: Setup ────────────────────────────────────────────────
echo -e "\n${YELLOW}[0/5] Setup: containers, fixtures, build...${NC}"

# Start Docker containers
docker compose -f "$COMPOSE_FILE" up -d --wait > /dev/null 2>&1
echo "  Docker containers up"

# Create fixture bare repos (3 independent repos with at least 1 commit each)
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

    # Create a unique file so repos are distinguishable
    cat > "$tmp/sample.cs" <<'JAVA'
public class Sample {
    public static void main(String[] args) {
        System.out.println("Hello from sample");
    }
}
JAVA
    echo "# $name fixture" > "$tmp/README.md"

    git -C "$tmp" add . > /dev/null 2>&1
    git -C "$tmp" -c user.email=test@test.com -c user.name=Test \
        commit -m "initial commit for $name" > /dev/null 2>&1
    git -C "$tmp" push origin main 2>/dev/null || { git -C "$tmp" branch -M main && git -C "$tmp" push origin main 2>/dev/null; }
    rm -rf "$tmp"

    echo "$bare"
}

REPO_ALPHA=$(create_fixture_repo "repo-alpha")
REPO_BETA=$(create_fixture_repo "repo-beta")
REPO_GAMMA=$(create_fixture_repo "repo-gamma")

echo "  Fixture repos: alpha, beta, gamma created"

# Create shared workspace dir
mkdir -p "$SHARED_WORKSPACE/repos"

# Share fastembed cache across tests and CI runs to avoid HF 429 rate limits
mkdir -p /tmp/fastembed_cache_shared
ln -s /tmp/fastembed_cache_shared "$SHARED_WORKSPACE/fastembed_cache"

# Build the server binary
cargo build 2>&1 | grep -E "(Compiling|Finished|error)" || true
BINARY="$PROJECT_ROOT/target/debug/knot-server"

# ── Step 1: Start Instance A ─────────────────────────────────────
echo -e "\n${YELLOW}[1/5] Starting Instance A (port $SERVER_A_PORT)...${NC}"

start_server() {
    local port="$1"
    local logfile="$2"

    KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
    KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
    KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
    KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
    KNOT_SERVER_PORT="$port" \
    KNOT_WORKSPACE_DIR="$SHARED_WORKSPACE" \
    KNOT_SERVER_POLL_INTERVAL_SECS=2 \
    KNOT_SERVER_STALE_LOCK_TIMEOUT_SECS=3 \
    KNOT_SERVER_QUEUE_CAPACITY=16 \
    RUST_LOG=info \
        "$BINARY" > "$logfile" 2>&1 &
    echo $!
}

SERVER_A_LOG="/tmp/knot-cluster-e2e-a-$$.log"
SERVER_A_PID=$(start_server "$SERVER_A_PORT" "$SERVER_A_LOG")

# Wait for Instance A to be ready
for i in $(seq 1 90); do
    if curl -sf "http://localhost:$SERVER_A_PORT/api/health" > /dev/null 2>&1; then
        echo "  Instance A ready"
        break
    fi
    if [ "$i" -eq 90 ]; then
        echo -e "${RED}Instance A failed to start${NC}"
        cat "$SERVER_A_LOG"
        exit 1
    fi
    sleep 1
done

# ── Step 2: Register repos via Instance A ────────────────────────
echo -e "\n${YELLOW}[2/5] Registering repos via Instance A...${NC}"

BASE_A="http://localhost:$SERVER_A_PORT"

register_repo() {
    local repo_name="$1"
    local repo_url="$2"
    curl -s -X POST "$BASE_A/api/repos" \
        -H "Content-Type: application/json" \
        -d "{\"url\": \"$repo_url\", \"name\": \"$repo_name\", \"auth\": {\"type\": \"none\"}}"
}

# Register repos
register_repo "repo-alpha" "$REPO_ALPHA" > /dev/null
register_repo "repo-beta" "$REPO_BETA" > /dev/null
register_repo "repo-gamma" "$REPO_GAMMA" > /dev/null
echo "  Repos registered: alpha, beta, gamma"

# Wait for them to be indexed (status indexed)
wait_for_indexed() {
    local repo_name="$1"
    local port="$2"
    local base="http://localhost:$port"
    for i in $(seq 1 60); do
        local status
        status=$(curl -sf "$base/api/repos/$repo_name" | jq -r '.status')
        if [ "$status" = "indexed" ]; then
            echo "  $repo_name → indexed"
            return 0
        elif [ "$status" = "error" ]; then
            echo -e "${RED}$repo_name → error (check server logs)${NC}"
            return 1
        fi
        sleep 1
    done
    echo -e "${YELLOW}$repo_name → still not indexed after 60s${NC}"
    return 0
}

wait_for_indexed "repo-alpha" "$SERVER_A_PORT" || true
wait_for_indexed "repo-beta" "$SERVER_A_PORT" || true
wait_for_indexed "repo-gamma" "$SERVER_A_PORT" || true

# Verify all 3 are in the registry with indexed status
LIST_A=$(curl -sf "$BASE_A/api/repos")
assert_contains "$LIST_A" '"repo-alpha"' "repo-alpha registered on A"
assert_contains "$LIST_A" '"repo-beta"' "repo-beta registered on A"
assert_contains "$LIST_A" '"repo-gamma"' "repo-gamma registered on A"

STATUS_A_ALPHA=$(echo "$LIST_A" | jq -r '.repositories[] | select(.id=="repo-alpha") | .status')
STATUS_A_BETA=$(echo "$LIST_A" | jq -r '.repositories[] | select(.id=="repo-beta") | .status')
STATUS_A_GAMMA=$(echo "$LIST_A" | jq -r '.repositories[] | select(.id=="repo-gamma") | .status')
assert_status "$STATUS_A_ALPHA" "indexed" "repo-alpha is indexed on A"
assert_status "$STATUS_A_BETA" "indexed" "repo-beta is indexed on A"
assert_status "$STATUS_A_GAMMA" "indexed" "repo-gamma is indexed on A"

# ── Step 3: Start Instance B ─────────────────────────────────────
echo -e "\n${YELLOW}[3/5] Starting Instance B (port $SERVER_B_PORT)...${NC}"

SERVER_B_LOG="/tmp/knot-cluster-e2e-b-$$.log"
SERVER_B_PID=$(start_server "$SERVER_B_PORT" "$SERVER_B_LOG")

# Wait for Instance B to be ready
for i in $(seq 1 90); do
    if curl -sf "http://localhost:$SERVER_B_PORT/api/health" > /dev/null 2>&1; then
        echo "  Instance B ready"
        break
    fi
    if [ "$i" -eq 90 ]; then
        echo -e "${RED}Instance B failed to start${NC}"
        cat "$SERVER_B_LOG"
        exit 1
    fi
    sleep 1
done

BASE_B="http://localhost:$SERVER_B_PORT"

# Verify B sees the same 3 repos (shared repos.json)
LIST_B=$(curl -sf "$BASE_B/api/repos")
assert_contains "$LIST_B" '"repo-alpha"' "repo-alpha visible on B"
assert_contains "$LIST_B" '"repo-beta"' "repo-beta visible on B"
assert_contains "$LIST_B" '"repo-gamma"' "repo-gamma visible on B"

# ── Step 4: Create orphaned lock file and kill Instance A ─────────
echo -e "\n${YELLOW}[4/5] Simulating orphaned lock after crash of A...${NC}"

LOCK_PATH="$SHARED_WORKSPACE/repo-alpha/.knot.lock"

# Clean up any stale lock files left from the initial indexing
rm -f "$LOCK_PATH" 2>/dev/null || true

# Use flock(1) to acquire an exclusive advisory lock, then kill it.
# The OS releases the lock but the file stays on disk — exactly the
# orphaned lock scenario after a node crash.
mkdir -p "$(dirname "$LOCK_PATH")"
touch "$LOCK_PATH"
flock --exclusive "$LOCK_PATH" sleep 30 &
FLOCK_PID=$!
sleep 0.5

echo "  flock process holding lock (PID: $FLOCK_PID)"
assert_file_exists "$LOCK_PATH" ".knot.lock exists (flock holding it)"

# Kill the flock process — OS releases the advisory lock, file stays.
kill -9 "$FLOCK_PID"
wait "$FLOCK_PID" 2>/dev/null || true
echo "  flock killed (simulating worker crash)"

# Actually, on Linux, kill -9 on flock doesn't orphan the file immediately
# if another process had the fd open. Let's also kill Instance A now.
kill -9 "$SERVER_A_PID"
wait "$SERVER_A_PID" 2>/dev/null || true
SERVER_A_PID=""
echo "  Instance A killed (SIGKILL)"

# Verify A is dead
if curl -sf "http://localhost:$SERVER_A_PORT/api/health" > /dev/null 2>&1; then
    fail "Instance A still responds after SIGKILL"
    exit 1
else
    pass "Instance A no longer responds after SIGKILL"
fi

# Artificially age the lock file so the scheduler detects it as stale
# (the stale_lock_timeout is 3 seconds, so we set mtime to 10 seconds ago)
touch -d "10 seconds ago" "$LOCK_PATH" 2>/dev/null || touch -t "$(date -d '10 seconds ago' +%Y%m%d%H%M.%S 2>/dev/null || date -d '-10 sec' +%Y%m%d%H%M.%S 2>/dev/null)" "$LOCK_PATH"
echo "  .knot.lock artificially aged (mtime set to 10s ago)"

assert_file_exists "$LOCK_PATH" ".knot.lock orphaned on disk after crash"

# ── Step 5: Verify B detects stale lock and recovers ─────────────
echo -e "\n${YELLOW}[5/5] Waiting for B to detect stale lock and recover...${NC}"

# With poll_interval=2s and stale_lock_timeout=3s, B's scheduler
# should detect the orphaned lock within ~6 seconds. When it does:
#   1. It logs "Stale lock detected" and removes the lock file
#   2. It enqueues a Pull job
#   3. The worker processes it (pull + index) and sets status=indexed
#
# We verify by checking B's server log for the recovery messages.
# Note: after processing, the worker creates a fresh .knot.lock
# (which it releases), so we can't rely on file absence alone.

for i in $(seq 1 20); do
    sleep 1

    if grep -q "Removed stale lock: .*repo-alpha" "$SERVER_B_LOG" 2>/dev/null; then
        echo "  Stale lock detected and removed by B's scheduler after ${i}s"
        break
    fi

    if [ "$i" -eq 20 ]; then
        echo -e "${RED}Recovery timeout: stale lock not detected after 20s${NC}"
        echo "  Lock file: $(test -f "$LOCK_PATH" && echo 'exists' || echo 'removed')"
        echo "  Instance B log scheduler messages:"
        grep -i "scheduler\|stale" "$SERVER_B_LOG" || echo "  (none)"
        exit 1
    fi
done

# Removing the stale lock only *enqueues* the recovery Pull job; the actual
# pull + parse + embed + ingest takes several seconds. Wait for the worker to
# finish before asserting the terminal state, otherwise we race and observe a
# transient 'indexing' status.
echo "  Waiting for B to complete the recovery job for repo-alpha..."
for i in $(seq 1 90); do
    if grep -q "job completed for 'repo-alpha'" "$SERVER_B_LOG" 2>/dev/null; then
        echo "  B worker completed repo-alpha recovery after ${i}s"
        break
    fi
    if [ "$(curl -sf "$BASE_B/api/repos/repo-alpha" | jq -r '.status')" = "indexed" ]; then
        echo "  repo-alpha reported 'indexed' on B after ${i}s"
        break
    fi
    if [ "$i" -eq 90 ]; then
        echo -e "${YELLOW}  Recovery job did not complete within 90s${NC}"
    fi
    sleep 1
done

# Verify B's log shows the full recovery sequence
assert_contains "$(tail -500 "$SERVER_B_LOG")" "Removed stale lock" "B scheduler removed stale .knot.lock"
assert_contains "$(tail -500 "$SERVER_B_LOG")" "job completed for 'repo-alpha'" "B worker processed repo-alpha after recovery"

# Verify repos are still in good shape via B
STATUS_B1=$(curl -sf "$BASE_B/api/repos/repo-alpha" | jq -r '.status')
STATUS_B2=$(curl -sf "$BASE_B/api/repos/repo-beta" | jq -r '.status')
STATUS_B3=$(curl -sf "$BASE_B/api/repos/repo-gamma" | jq -r '.status')
assert_status "$STATUS_B1" "indexed" "repo-alpha is indexed on B after recovery"
assert_status "$STATUS_B2" "indexed" "repo-beta still indexed (unaffected)"
assert_status "$STATUS_B3" "indexed" "repo-gamma still indexed (unaffected)"

# ── Summary ──────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Cluster Coordination E2E: Results${NC}"
echo -e "${GREEN}========================================${NC}"
echo -e "  Passed: ${GREEN}$PASSED${NC}"
echo -e "  Failed: ${RED}$FAILED${NC}"
echo ""
echo "Validated:"
echo "  - Shared workspace (repos.json, .knot.lock)"
echo "  - Multi-instance registry visibility"
echo "  - Orphaned lock after crash (SIGKILL)"
echo "  - Scheduler stale lock detection and cleanup"
echo "  - Automatic re-enqueue and recovery by surviving node"

if [ "$FAILED" -gt 0 ]; then
    echo -e "\n${RED}Some tests FAILED${NC}"
    exit 1
else
    echo -e "\n${GREEN}All cluster coordination tests PASSED${NC}"
    exit 0
fi
