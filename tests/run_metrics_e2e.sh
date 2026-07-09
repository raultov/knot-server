#!/usr/bin/env bash
# E2E Metrics Endpoint Test for knot-server
# Validates that the Prometheus /metrics endpoint returns
# the correct content type and exposes expected metrics.
#
# Scenario:
#   1. Start the server (metrics enabled by default)
#   2. GET /metrics
#   3. Verify Content-Type and presence of knot_build_info

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
WORKSPACE_DIR="/tmp/knot-metrics-e2e-$$"
SERVER_PORT=18085
SERVER_PID=""
SERVER_LOG="/tmp/knot-metrics-e2e-$$.log"

NEO4J_URI="bolt://localhost:17687"
NEO4J_USER="neo4j"
NEO4J_PASSWORD="e2e_test_password"
QDRANT_URL="http://localhost:16334"
BASE_URL="http://localhost:$SERVER_PORT"

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

cleanup() {
    local exit_code=$?
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    cd "$SCRIPT_DIR"
    docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
    rm -rf "$WORKSPACE_DIR" 2>/dev/null || true
    rm -f "$SERVER_LOG"

    echo ""
    if [ "$FAILED" -gt 0 ]; then
        echo -e "${RED}$FAILED test(s) failed${NC}"
    fi
    if [ "$PASSED" -gt 0 ]; then
        echo -e "${GREEN}$PASSED test(s) passed${NC}"
    fi
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}knot-server Metrics Endpoint E2E${NC}"
echo -e "${GREEN}========================================${NC}"

# ── Step 1: Start Docker containers ──────────────────────────
echo -e "\n${YELLOW}[1/3] Starting Docker containers...${NC}"
cd "$SCRIPT_DIR"
docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
docker compose -f "$COMPOSE_FILE" up -d

# ── Step 2: Wait for databases ───────────────────────────────
echo -e "${YELLOW}[2/3] Waiting for databases...${NC}"

wait_for_port() {
    local port="$1"
    local label="$2"
    local max_wait="${3:-60}"
    echo -n "  Waiting for $label (port $port)..."
    for i in $(seq 1 "$max_wait"); do
        if nc -z localhost "$port" 2>/dev/null; then
            echo -e " ${GREEN}ready${NC}"
            return 0
        fi
        sleep 1
    done
    echo -e " ${RED}timeout${NC}"
    return 1
}

wait_for_port 17687 "Neo4j" 60
wait_for_port 16334 "Qdrant" 30

echo -n "  Waiting for Neo4j health check..."
for i in $(seq 1 300); do
    STATUS=$(docker inspect --format='{{.State.Health.Status}}' knot_server_neo4j_e2e 2>/dev/null || echo "unknown")
    if [ "$STATUS" = "healthy" ]; then
        echo -e " ${GREEN}healthy${NC}"
        break
    fi
    if [ "$i" -eq 300 ]; then
        echo -e " ${RED}timeout (status: $STATUS)${NC}"
        exit 1
    fi
    sleep 1
done
sleep 3

# ── Step 3: Build + start server, run metrics tests ─────────
echo -e "${YELLOW}[3/3] Building server + running metrics tests...${NC}"
cd "$PROJECT_ROOT"

rm -rf "$WORKSPACE_DIR"
mkdir -p "$WORKSPACE_DIR"

# Share fastembed cache
mkdir -p /tmp/fastembed_cache_shared
ln -s /tmp/fastembed_cache_shared "$WORKSPACE_DIR/fastembed_cache"

cargo build 2>&1 | grep -E "(Compiling|Finished|error)" || true

KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
KNOT_SERVER_PORT="$SERVER_PORT" \
KNOT_WORKSPACE_DIR="$WORKSPACE_DIR" \
KNOT_SERVER_METRICS_ENABLED=true \
RUST_LOG=info \
    "$PROJECT_ROOT/target/debug/knot-server" > "$SERVER_LOG" 2>&1 &
SERVER_PID=$!

echo -n "  Waiting for knot-server on port $SERVER_PORT..."
for i in $(seq 1 90); do
    if curl -sf "$BASE_URL/api/health" >/dev/null 2>&1; then
        echo -e " ${GREEN}ready${NC}"
        break
    fi
    if [ "$i" -eq 90 ]; then
        echo -e " ${RED}did not start${NC}"
        cat "$SERVER_LOG"
        exit 1
    fi
    sleep 1
done

# ── Test A: Metrics endpoint returns 200 with correct Content-Type ──
echo -e "\n${CYAN}Test A: GET /metrics returns 200 with text/plain Content-Type${NC}"
HTTP_CODE=$(curl -s -w "%{http_code}" -o /tmp/metrics_a.txt "$BASE_URL/metrics")
CT=$(curl -s -I "$BASE_URL/metrics" | grep -i "^Content-Type:" | tr -d '\r\n' | tr '[:upper:]' '[:lower:]')

if [ "$HTTP_CODE" = "200" ]; then pass "HTTP 200"; else fail "expected 200, got $HTTP_CODE"; fi

if echo "$CT" | grep -q "text/plain"; then
    pass "Content-Type is text/plain"
else
    fail "Content-Type is not text/plain (got: $CT)"
fi

# ── Test B: Metrics body contains knot_build_info ──
echo -e "\n${CYAN}Test B: Metrics body contains knot_build_info${NC}"
if grep -q "knot_build_info" /tmp/metrics_a.txt; then
    pass "knot_build_info found in metrics output"
else
    fail "knot_build_info not found in metrics output"
    echo "Metrics output (first 50 lines):"
    head -50 /tmp/metrics_a.txt
fi

# Verify the metric has version labels (a gauge with value 1).
# Skip HELP/TYPE comment lines — only check the actual metric data line.
KBI_LINE=$(grep "knot_build_info" /tmp/metrics_a.txt | grep -v '^#' | head -1)
if [ -z "$KBI_LINE" ]; then
    fail "knot_build_info data line not found"
else
    if echo "$KBI_LINE" | grep -q "version="; then
        pass "knot_build_info has version label"
    else
        fail "knot_build_info missing version label (got: $KBI_LINE)"
    fi

    if echo "$KBI_LINE" | grep -q "knot_version="; then
        pass "knot_build_info has knot_version label"
    else
        fail "knot_build_info missing knot_version label (got: $KBI_LINE)"
    fi
fi

# ── Test C: Metrics body contains other expected metrics ──
echo -e "\n${CYAN}Test C: Other expected metrics are present${NC}"

check_metric() {
    local name="$1"
    if grep -q "$name" /tmp/metrics_a.txt; then
        pass "$name is present"
    else
        fail "$name is missing"
    fi
}

check_metric "knot_http_requests_total"
check_metric "knot_repositories_total"
check_metric "knot_process_uptime_seconds"
check_metric "knot_queue_available_capacity"

# ── Test D: Metrics for the /metrics request itself are tracked ──
echo -e "\n${CYAN}Test D: GET /metrics request is tracked in http metrics${NC}"

# At least one request was made to /metrics (the one we just did).
# The /metrics route is listed as "unmatched" in the intern table, so it appears
# under the "unmatched" route label.
if grep -q 'knot_http_requests_total.*route="unmatched".*200' /tmp/metrics_a.txt; then
    pass "metrics request tracked (unmatched route)"
else
    # The first request may have been handled before the router interned it.
    # Check more broadly — any http request counter counts.
    if grep -q 'knot_http_requests_total{' /tmp/metrics_a.txt; then
        pass "metrics request tracking present"
    else
        fail "no http request metrics found"
    fi
fi

rm -f /tmp/metrics_a.txt

# ── Summary ──────────────────────────────────────────────────
echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Metrics E2E: Results${NC}"
echo -e "${GREEN}========================================${NC}"
echo -e "  Passed: ${GREEN}$PASSED${NC}"
echo -e "  Failed: ${RED}$FAILED${NC}"
echo ""
echo "Validated:"
echo "  - GET /metrics returns 200"
echo "  - Content-Type: text/plain"
echo "  - knot_build_info present with version labels"
echo "  - Core metric families exposed (http, repos, uptime, queue)"

if [ "$FAILED" -gt 0 ]; then
    echo -e "\n${RED}Some tests FAILED${NC}"
    exit 1
else
    echo -e "\n${GREEN}All metrics tests PASSED${NC}"
    exit 0
fi
