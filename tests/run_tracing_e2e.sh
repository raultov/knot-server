#!/usr/bin/env bash
# E2E Distributed Tracing Test for knot-server
# Validates the full OpenTelemetry pipeline end to end:
#   span creation -> tracing-opentelemetry bridge -> batch processor ->
#   OTLP gRPC export -> Jaeger ingestion -> Jaeger query API.
#
# Assertions go through Jaeger's HTTP query API (no jq dependency, grep-based
# like run_metrics_e2e.sh). The batch span processor exports asynchronously
# (~5s), so every Jaeger assertion polls with a deadline instead of asserting
# immediately after the request — this is the main source of tracing-e2e
# flakiness, hence the generous poll windows.

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
WORKSPACE_DIR="/tmp/knot-tracing-e2e-$$"
SERVER_PORT=18086
SERVER_PID=""
SERVER_LOG="/tmp/knot-tracing-e2e-$$.log"

NEO4J_URI="bolt://localhost:17687"
NEO4J_USER="neo4j"
NEO4J_PASSWORD="e2e_test_password"
QDRANT_URL="http://localhost:16334"
OTLP_ENDPOINT="http://localhost:24317"
JAEGER_QUERY="http://localhost:26686"
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

stop_server() {
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    SERVER_PID=""
}

cleanup() {
    local exit_code=$?
    stop_server
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

wait_for_port() {
    local port="$1"
    local label="$2"
    local max_wait="${3:-60}"
    echo -n "  Waiting for $label (port $port)..."
    for _ in $(seq 1 "$max_wait"); do
        if nc -z localhost "$port" 2>/dev/null; then
            echo -e " ${GREEN}ready${NC}"
            return 0
        fi
        sleep 1
    done
    echo -e " ${RED}timeout${NC}"
    return 1
}

# Poll a URL until its body contains all given needles, or a deadline elapses.
# Usage: poll_until <url> <deadline_secs> <needle> [needle...]
poll_until() {
    local url="$1"
    local deadline="$2"
    shift 2
    local body ok
    for _ in $(seq 1 "$deadline"); do
        body="$(curl -s "$url" 2>/dev/null || true)"
        ok=1
        for needle in "$@"; do
            if ! echo "$body" | grep -q "$needle"; then
                ok=0
                break
            fi
        done
        if [ "$ok" -eq 1 ]; then
            return 0
        fi
        sleep 1
    done
    # Leave the last body on stdout for debugging by the caller.
    echo "$body"
    return 1
}

start_server() {
    local tracing_enabled="$1"
    rm -f "$SERVER_LOG"
    KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
    KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
    KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
    KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
    KNOT_SERVER_PORT="$SERVER_PORT" \
    KNOT_WORKSPACE_DIR="$WORKSPACE_DIR" \
    KNOT_SERVER_METRICS_ENABLED=false \
    KNOT_SERVER_TRACING_ENABLED="$tracing_enabled" \
    KNOT_SERVER_OTLP_ENDPOINT="$OTLP_ENDPOINT" \
    RUST_LOG=info \
        "$PROJECT_ROOT/target/debug/knot-server" > "$SERVER_LOG" 2>&1 &
    SERVER_PID=$!

    echo -n "  Waiting for knot-server on port $SERVER_PORT (tracing=$tracing_enabled)..."
    for i in $(seq 1 90); do
        if curl -sf "$BASE_URL/api/health" >/dev/null 2>&1; then
            echo -e " ${GREEN}ready${NC}"
            return 0
        fi
        if [ "$i" -eq 90 ]; then
            echo -e " ${RED}did not start${NC}"
            cat "$SERVER_LOG"
            return 1
        fi
        sleep 1
    done
}

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}knot-server Distributed Tracing E2E${NC}"
echo -e "${GREEN}========================================${NC}"

# ── Step 1: Start Docker containers ──────────────────────────
echo -e "\n${YELLOW}[1/3] Starting Docker containers (neo4j, qdrant, jaeger)...${NC}"
cd "$SCRIPT_DIR"
docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
docker compose -f "$COMPOSE_FILE" up -d

# ── Step 2: Wait for backends ────────────────────────────────
echo -e "${YELLOW}[2/3] Waiting for backends...${NC}"
wait_for_port 17687 "Neo4j" 60
wait_for_port 16334 "Qdrant" 30
wait_for_port 24317 "Jaeger OTLP gRPC" 60
wait_for_port 26686 "Jaeger query API" 60

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

# ── Step 3: Build + start server, run tracing tests ─────────
echo -e "${YELLOW}[3/3] Building server + running tracing tests...${NC}"
cd "$PROJECT_ROOT"

rm -rf "$WORKSPACE_DIR"
mkdir -p "$WORKSPACE_DIR"
mkdir -p /tmp/fastembed_cache_shared
ln -s /tmp/fastembed_cache_shared "$WORKSPACE_DIR/fastembed_cache"

cargo build 2>&1 | grep -E "(Compiling|Finished|error)" || true

start_server "true"

# ── Test A: service registers in Jaeger ──────────────────────
echo -e "\n${CYAN}Test A: knot-server appears in Jaeger's service list${NC}"
for _ in $(seq 1 5); do
    curl -sf "$BASE_URL/api/health" >/dev/null 2>&1 || true
done
if poll_until "$JAEGER_QUERY/api/services" 40 '"knot-server"' >/dev/null; then
    pass "knot-server registered as a Jaeger service"
else
    fail "knot-server did not appear in $JAEGER_QUERY/api/services within deadline"
    curl -s "$JAEGER_QUERY/api/services" || true
fi

# ── Test B: root span shape (operation, route, status) ───────
echo -e "\n${CYAN}Test B: GET /api/repos root span has http.route + status code${NC}"
curl -sf "$BASE_URL/api/repos" >/dev/null 2>&1 || true
# Jaeger query: filter by service + operation (our otel span name "GET /api/repos").
TRACES_B_URL="$(curl -Gs -o /dev/null -w '%{url_effective}' \
    "$JAEGER_QUERY/api/traces" \
    --data-urlencode "service=knot-server" \
    --data-urlencode "operation=GET /api/repos")"
if poll_until "$TRACES_B_URL" 40 'http.route' >/dev/null; then
    pass "root span exposes http.route attribute"
else
    fail "no http.route attribute found for operation 'GET /api/repos'"
    curl -s "$TRACES_B_URL" | head -c 2000 || true
fi
if poll_until "$TRACES_B_URL" 20 'http.response.status_code' >/dev/null; then
    pass "root span exposes http.response.status_code attribute"
else
    fail "no http.response.status_code attribute found"
fi

# ── Test C: W3C traceparent propagation over the wire ────────
echo -e "\n${CYAN}Test C: inbound W3C traceparent becomes the span's parent trace${NC}"
TRACE_ID="0af7651916cd43dd8448eb211c80319c"
curl -sf -H "traceparent: 00-${TRACE_ID}-b7ad6b7169203331-01" \
    "$BASE_URL/api/repos" >/dev/null 2>&1 || true
if poll_until "$JAEGER_QUERY/api/traces/${TRACE_ID}" 40 "\"traceID\"" "knot-server" >/dev/null; then
    pass "remote trace-id $TRACE_ID contains a knot-server span (propagation works)"
else
    fail "trace $TRACE_ID not found in Jaeger or missing knot-server span"
    curl -s "$JAEGER_QUERY/api/traces/${TRACE_ID}" | head -c 2000 || true
fi

# ── Test E: kill switch — tracing disabled produces no OTLP errors ──
echo -e "\n${CYAN}Test E: KNOT_SERVER_TRACING_ENABLED=false disables the exporter cleanly${NC}"
stop_server
start_server "false"
if curl -sf "$BASE_URL/api/health" >/dev/null 2>&1; then
    pass "server healthy with tracing disabled"
else
    fail "server unhealthy with tracing disabled"
fi
# Exercise a few requests; none should trigger any OTLP/OTel export machinery.
for _ in $(seq 1 3); do curl -sf "$BASE_URL/api/repos" >/dev/null 2>&1 || true; done
sleep 2
if grep -iE "otlp|opentelemetry|failed to export|tonic.*error" "$SERVER_LOG" >/dev/null 2>&1; then
    fail "OTLP/OTel error surfaced in logs while tracing was disabled"
    grep -iE "otlp|opentelemetry|failed to export|tonic.*error" "$SERVER_LOG" | head -10
else
    pass "no OTLP/OTel errors logged with tracing disabled"
fi

# ── Summary ──────────────────────────────────────────────────
echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Tracing E2E: Results${NC}"
echo -e "${GREEN}========================================${NC}"
echo -e "  Passed: ${GREEN}$PASSED${NC}"
echo -e "  Failed: ${RED}$FAILED${NC}"
echo ""
echo "Validated:"
echo "  - Service registers in Jaeger (span export pipeline works end to end)"
echo "  - HTTP root span carries http.route + status code attributes"
echo "  - Inbound W3C traceparent is honored as the parent trace"
echo "  - Kill switch: tracing disabled → no exporter, no OTLP errors"

if [ "$FAILED" -gt 0 ]; then
    echo -e "\n${RED}Some tracing tests FAILED${NC}"
    exit 1
else
    echo -e "\n${GREEN}All tracing tests PASSED${NC}"
    exit 0
fi
