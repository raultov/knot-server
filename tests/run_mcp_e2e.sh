#!/usr/bin/env bash
# E2E MCP Endpoint Test for knot-server (MCP_ENDPOINT_PLAN.md §8)
#
# Validates that /mcp serves the knot MCP tool surface as a STATELESS
# JSON-RPC-over-HTTP endpoint across a two-node cluster with no session
# affinity:
#   M1 — handshake: initialize succeeds, tools capability advertised,
#        and NO Mcp-Session-Id header is ever emitted (the cluster invariant)
#   M2 — cross-node conversation: initialize on A, tools/list on B,
#        tool calls on both — no affinity, no session state anywhere
#   M3 — a node that never saw the handshake serves tools (restart B)
#   M4 — real results: search_hybrid_context returns fixture content
#   M5 — protocol errors over the wire, executed against BOTH nodes
#   M6 — parity with the REST surface (MCP on node A vs REST on node B)
#        plus byte-identical find_callers output across nodes
#   M7 — explicit truncation: max_targets is forwarded and both surfaces
#        report the same TRUE total / shown count / truncated flag
#
# The statelessness invariant (D2: no Mcp-Session-Id) is asserted on EVERY
# response captured by this suite — both nodes, including the restarted node
# and the error paths — not just on the handshake.
#
# Scenarios M2 and M3 are the regression test for decision D1: they would
# fail under a session-oriented design (rust-mcp-axum).

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
SHARED_WORKSPACE="/tmp/knot-mcp-e2e-$$"
SERVER_A_PORT=18081
SERVER_B_PORT=18082
SERVER_A_PID=""
SERVER_B_PID=""
LOG_A="/tmp/knot-mcp-e2e-a-$$.log"
LOG_B="/tmp/knot-mcp-e2e-b-$$.log"
TMP=$(mktemp -d)

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

assert_eq() {
    local got="$1"
    local expected="$2"
    local msg="$3"
    if [ "$got" = "$expected" ]; then
        pass "$msg"
    else
        fail "$msg (got: '$got', expected: '$expected')"
    fi
}

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local msg="$3"
    # -e keeps needles like "-32700" from being parsed as grep options.
    if echo "$haystack" | grep -q -e "$needle"; then
        pass "$msg"
    else
        fail "$msg (needle '$needle' not found)"
    fi
}

assert_not_contains() {
    local haystack="$1"
    local needle="$2"
    local msg="$3"
    if echo "$haystack" | grep -q -e "$needle"; then
        fail "$msg (needle '$needle' unexpectedly found)"
    else
        pass "$msg"
    fi
}

# The cluster invariant (MCP_ENDPOINT_PLAN.md D2): a stateless server must
# NEVER emit Mcp-Session-Id. Emitting it tells the client the server is
# session-oriented and silently forces session affinity at the load balancer.
# It must be absent from EVERY response, on EVERY node, at EVERY point of the
# conversation — not just the handshake.
assert_no_session_header() {
    local headers_file="$1"
    local msg="$2"
    assert_not_contains "$(tr '[:upper:]' '[:lower:]' < "$headers_file")" \
        "mcp-session-id" "$msg"
}

cleanup() {
    local exit_code=$?
    for pid in "$SERVER_A_PID" "$SERVER_B_PID"; do
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    cd "$SCRIPT_DIR"
    docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
    rm -rf "$SHARED_WORKSPACE" 2>/dev/null || true
    rm -rf "$TMP" 2>/dev/null || true
    rm -f "$LOG_A" "$LOG_B" 2>/dev/null || true
    exit "$exit_code"
}

trap cleanup EXIT

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}knot-server MCP Endpoint E2E${NC}"
echo -e "${GREEN}========================================${NC}"

# ── Setup: containers, fixture repo, build ───────────────────────
echo -e "\n${YELLOW}[0/6] Setup: containers, fixture repo, build...${NC}"

docker compose -f "$COMPOSE_FILE" down -v > /dev/null 2>&1 || true
docker compose -f "$COMPOSE_FILE" up -d --wait > /dev/null 2>&1
echo "  Docker containers up"

# Fixture repo: a local bare repo with the shared fixture sources.
FIXTURES_ROOT="$SHARED_WORKSPACE/fixtures"
mkdir -p "$FIXTURES_ROOT"
REPO_BARE="$FIXTURES_ROOT/mcp-fixture.git"
REPO_WORK="$FIXTURES_ROOT/mcp-fixture-tmp"

git init --bare "$REPO_BARE" > /dev/null 2>&1
git clone "$REPO_BARE" "$REPO_WORK" 2>/dev/null
cp "$SCRIPT_DIR/fixtures"/*.java "$REPO_WORK/"
echo "# MCP E2E fixture" > "$REPO_WORK/README.md"
git -C "$REPO_WORK" add . > /dev/null 2>&1
git -C "$REPO_WORK" -c user.email=e2e@test -c user.name=e2e \
    commit -m "initial commit with fixture sources" > /dev/null 2>&1
git -C "$REPO_WORK" branch -M main > /dev/null 2>&1
git -C "$REPO_WORK" push origin main > /dev/null 2>&1 || \
    git -C "$REPO_WORK" push origin main > /dev/null 2>&1
rm -rf "$REPO_WORK"
echo "  Fixture repo: $REPO_BARE"

mkdir -p "$SHARED_WORKSPACE/repos"

# Share fastembed cache across tests and CI runs to avoid HF 429 rate limits
mkdir -p /tmp/fastembed_cache_shared
ln -s /tmp/fastembed_cache_shared "$SHARED_WORKSPACE/fastembed_cache"

cargo build 2>&1 | grep -E "(Compiling|Finished|error)" || true
BINARY="$PROJECT_ROOT/target/debug/knot-server"

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
    KNOT_SERVER_QUEUE_CAPACITY=16 \
    RUST_LOG=info \
        "$BINARY" > "$logfile" 2>&1 &
    echo $!
}

wait_ready() {
    local port="$1"
    local name="$2"
    for i in $(seq 1 90); do
        if curl -sf "http://localhost:$port/api/health" > /dev/null 2>&1; then
            echo "  $name ready"
            return 0
        fi
        if [ "$i" -eq 90 ]; then
            echo -e "${RED}$name failed to start${NC}"
            return 1
        fi
        sleep 1
    done
}

SERVER_A_PID=$(start_server "$SERVER_A_PORT" "$LOG_A")
wait_ready "$SERVER_A_PORT" "Instance A" || exit 1

BASE_A="http://localhost:$SERVER_A_PORT"
BASE_B="http://localhost:$SERVER_B_PORT"

# Register the fixture repo on A and wait for indexing.
curl -sf -X POST "$BASE_A/api/repos" \
    -H "Content-Type: application/json" \
    -d "{\"url\": \"$REPO_BARE\", \"auth_type\": \"ssh\"}" > /dev/null
for i in $(seq 1 90); do
    status=$(curl -sf "$BASE_A/api/repos/mcp-fixture" | jq -r '.status' 2>/dev/null || echo "")
    if [ "$status" = "indexed" ]; then
        echo "  mcp-fixture indexed"
        break
    fi
    if [ "$status" = "error" ]; then
        echo -e "${RED}mcp-fixture indexing failed${NC}"
        tail -30 "$LOG_A"
        exit 1
    fi
    if [ "$i" -eq 90 ]; then
        echo -e "${YELLOW}mcp-fixture not indexed after 90s (continuing)${NC}"
    fi
    sleep 1
done

SERVER_B_PID=$(start_server "$SERVER_B_PORT" "$LOG_B")
wait_ready "$SERVER_B_PORT" "Instance B" || exit 1

# mcp_post <base> <body-file> <name> [extra curl args...]
#
# POSTs a JSON-RPC body to <base>/mcp, writes the response body to
# $TMP/<name>-resp.json and the response headers to $TMP/<name>-headers.txt,
# and prints the HTTP status code to stdout. Capturing headers on *every*
# call is what lets assert_no_session_header cover the whole conversation.
#
# Note: extra curl args are appended AFTER the fixed -H headers, so this
# helper cannot be used for cases needing a different Content-Type/Accept
# (a duplicate header would be sent). Those cases use raw curl instead.
mcp_post() {
    local base="$1"
    local body_file="$2"
    local name="$3"
    shift 3
    curl -s -D "$TMP/$name-headers.txt" -o "$TMP/$name-resp.json" -w "%{http_code}" \
        -X POST "$base/mcp" \
        -H "Content-Type: application/json" \
        -H "Accept: application/json" \
        --data-binary "@$body_file" "$@"
}

# ── M1: handshake ────────────────────────────────────────────────
echo -e "\n${YELLOW}[M1] Handshake: initialize + statelessness invariant${NC}"

cat > "$TMP/init.json" <<'JSON'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0.0.1"}}}
JSON

M1_STATUS=$(mcp_post "$BASE_A" "$TMP/init.json" "init")
M1_CT=$(grep -i "^content-type:" "$TMP/init-headers.txt" | tr -d '\r' | awk '{print $2}')
assert_eq "$M1_STATUS" "200" "M1: initialize returns 200"
assert_contains "$M1_CT" "application/json" "M1: response is application/json"
assert_contains "$(cat "$TMP/init-resp.json")" '"tools"' "M1: tools capability advertised"
assert_contains "$(cat "$TMP/init-resp.json")" '"knot-server"' "M1: serverInfo identifies knot-server"
assert_no_session_header "$TMP/init-headers.txt" \
    "M1: no Mcp-Session-Id on initialize (node A)"

# ── M2: cross-node conversation ──────────────────────────────────
echo -e "\n${YELLOW}[M2] Cross-node conversation without session affinity${NC}"

cat > "$TMP/tools-list.json" <<'JSON'
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
JSON

# initialize already went to node A above; now tools/list on node B —
# a session-oriented server would answer 404 session_not_found here.
M2_TOOLS_STATUS=$(mcp_post "$BASE_B" "$TMP/tools-list.json" "tools-list")
M2_TOOLS=$(cat "$TMP/tools-list-resp.json")
assert_eq "$M2_TOOLS_STATUS" "200" "M2: tools/list on node B returns 200"
assert_contains "$M2_TOOLS" "search_hybrid_context" "M2: tools/list on node B works"
assert_contains "$M2_TOOLS" "find_callers" "M2: find_callers listed"
assert_contains "$M2_TOOLS" "explore_file" "M2: explore_file listed"
assert_contains "$M2_TOOLS" "list_repo_dependencies" "M2: list_repo_dependencies listed"
assert_contains "$M2_TOOLS" "list_repositories" "M2: list_repositories listed"
assert_no_session_header "$TMP/tools-list-headers.txt" \
    "M2: no Mcp-Session-Id on tools/list (node B)"

cat > "$TMP/search.json" <<'JSON'
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_hybrid_context","arguments":{"query":"greeting","repo_name":"mcp-fixture"}}}
JSON
M2_SEARCH_STATUS=$(mcp_post "$BASE_A" "$TMP/search.json" "search")
assert_eq "$M2_SEARCH_STATUS" "200" "M2: tools/call search on node A returns 200"
M2_ISERROR=$(jq -r '.result.isError // false' "$TMP/search-resp.json" 2>/dev/null || echo "parse-error")
assert_eq "$M2_ISERROR" "false" "M2: search_hybrid_context succeeds on node A"
assert_no_session_header "$TMP/search-headers.txt" \
    "M2: no Mcp-Session-Id on tools/call search (node A)"

cat > "$TMP/explore.json" <<'JSON'
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"explore_file","arguments":{"path":"Greeter.java","repo_name":"mcp-fixture"}}}
JSON
M2_EXPLORE_STATUS=$(mcp_post "$BASE_B" "$TMP/explore.json" "explore")
assert_eq "$M2_EXPLORE_STATUS" "200" "M2: tools/call explore on node B returns 200"
assert_no_session_header "$TMP/explore-headers.txt" \
    "M2: no Mcp-Session-Id on tools/call explore (node B)"

# ── M3: a node that never saw the handshake serves tools ─────────
echo -e "\n${YELLOW}[M3] Restart node B (fresh process, zero in-memory state)${NC}"

kill "$SERVER_B_PID" 2>/dev/null || true
wait "$SERVER_B_PID" 2>/dev/null || true
SERVER_B_PID=""
sleep 2
SERVER_B_PID=$(start_server "$SERVER_B_PORT" "$LOG_B")
wait_ready "$SERVER_B_PORT" "Instance B (restarted)" || exit 1

cat > "$TMP/callers.json" <<'JSON'
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"find_callers","arguments":{"entity_name":"greet","repo_name":"mcp-fixture"}}}
JSON
M3_STATUS=$(mcp_post "$BASE_B" "$TMP/callers.json" "callers")
assert_eq "$M3_STATUS" "200" "M3: find_callers on restarted node B returns 200"
M3_ISERROR=$(jq -r '.result.isError // false' "$TMP/callers-resp.json" 2>/dev/null || echo "parse-error")
assert_eq "$M3_ISERROR" "false" "M3: find_callers succeeds on a node that never saw the handshake"
assert_no_session_header "$TMP/callers-headers.txt" \
    "M3: no Mcp-Session-Id from a node that never saw the handshake"

# ── M4: real results, not just 200s ──────────────────────────────
echo -e "\n${YELLOW}[M4] search_hybrid_context returns fixture content${NC}"

M4_TEXT=$(jq -r '[.result.content[]? | select(.type=="text") | .text] | join("\n")' "$TMP/search-resp.json" 2>/dev/null || echo "")
assert_contains "$M4_TEXT" "Greeter" "M4: search result references the fixture file"

# ── M5: protocol errors over the wire ────────────────────────────
echo -e "\n${YELLOW}[M5] Protocol errors (both nodes)${NC}"

# Protocol errors are node-independent by design; running the whole block
# against both nodes proves neither treats them specially and that the
# restarted node B still answers them identically.
run_protocol_error_checks() {
    local base="$1"
    local label="$2"

    local parse_status
    parse_status=$(curl -s -D "$TMP/parse-$label-headers.txt" \
        -o "$TMP/parse-$label-resp.json" -w "%{http_code}" -X POST "$base/mcp" \
        -H "Content-Type: application/json" -H "Accept: application/json" \
        --data-binary "{not valid json")
    assert_eq "$parse_status" "400" "M5[$label]: malformed JSON is 400"
    assert_contains "$(jq -r '.error.code' "$TMP/parse-$label-resp.json")" "-32700" \
        "M5[$label]: parse error code -32700"
    assert_no_session_header "$TMP/parse-$label-headers.txt" \
        "M5[$label]: no Mcp-Session-Id on parse error"

    local batch_status
    batch_status=$(mcp_post "$base" "$TMP/batch.json" "batch-$label")
    assert_eq "$batch_status" "400" "M5[$label]: batch request is 400"
    assert_contains "$(jq -r '.error.code' "$TMP/batch-$label-resp.json")" "-32600" \
        "M5[$label]: batch error code -32600"
    assert_no_session_header "$TMP/batch-$label-headers.txt" \
        "M5[$label]: no Mcp-Session-Id on batch rejection"

    local unknown_status
    unknown_status=$(mcp_post "$base" "$TMP/unknown-method.json" "unknown-$label")
    assert_eq "$unknown_status" "200" "M5[$label]: unknown method is 200"
    assert_contains "$(jq -r '.error.code' "$TMP/unknown-$label-resp.json")" "-32601" \
        "M5[$label]: unknown method code -32601"
    assert_eq "$(jq -r '.id' "$TMP/unknown-$label-resp.json")" "9" \
        "M5[$label]: error id matches request id"
    assert_no_session_header "$TMP/unknown-$label-headers.txt" \
        "M5[$label]: no Mcp-Session-Id on method-not-found"

    local get_status allow
    get_status=$(curl -s -D "$TMP/get-$label-headers.txt" -o /dev/null -w "%{http_code}" "$base/mcp")
    assert_eq "$get_status" "405" "M5[$label]: GET /mcp is 405"
    allow=$(grep -i '^allow:' "$TMP/get-$label-headers.txt" | tr -d '\r')
    assert_contains "$allow" "POST" "M5[$label]: Allow header lists POST"
    assert_contains "$allow" "DELETE" "M5[$label]: Allow header lists DELETE"

    local delete_status
    delete_status=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE "$base/mcp")
    assert_eq "$delete_status" "200" "M5[$label]: DELETE /mcp is a 200 no-op"

    local ct_status
    ct_status=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$base/mcp" \
        -H "Content-Type: text/plain" -H "Accept: application/json" --data-binary "{}")
    assert_eq "$ct_status" "415" "M5[$label]: text/plain Content-Type is 415"

    local accept_status
    accept_status=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$base/mcp" \
        -H "Content-Type: application/json" -H "Accept: text/plain" --data-binary "{}")
    assert_eq "$accept_status" "406" "M5[$label]: Accept without JSON is 406"
}

cat > "$TMP/batch.json" <<'JSON'
[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"method":"ping"}]
JSON

cat > "$TMP/unknown-method.json" <<'JSON'
{"jsonrpc":"2.0","id":9,"method":"resources/list","params":{}}
JSON

run_protocol_error_checks "$BASE_A" "A"
run_protocol_error_checks "$BASE_B" "B"

# ── M6: parity with the REST surface, cross-node ─────────────────
echo -e "\n${YELLOW}[M6] MCP and REST report the same callers (cross-node)${NC}"

# M6 is deliberately cross-node AND cross-transport: the MCP call goes to
# node A, the REST call to node B. A divergence means either the knot bypass
# leaked, or the two nodes are not seeing the same corpus.
M6_MCP_STATUS=$(mcp_post "$BASE_A" "$TMP/callers.json" "callers-a")
assert_eq "$M6_MCP_STATUS" "200" "M6: find_callers via /mcp on node A returns 200"
assert_no_session_header "$TMP/callers-a-headers.txt" \
    "M6: no Mcp-Session-Id on node A tool call"

# The MCP tool returns the references formatted as Markdown (the same text
# knot-mcp serves over stdio); the REST endpoint returns raw JSON. Parity
# means both report the same calls count. The Markdown renders a non-empty
# calls bucket as "## Calls (function/method invocations) (N)"; an empty one
# as "No references found".
M6_MCP_TEXT=$(jq -r '.result.content[0].text' "$TMP/callers-a-resp.json" 2>/dev/null || echo "")
assert_contains "$M6_MCP_TEXT" 'References to .greet.' \
    "M6: MCP find_callers output references the queried entity"

M6_REST_JSON="$TMP/callers-rest.json"
curl -sf "$BASE_B/api/repos/mcp-fixture/callers?entity=greet" -o "$M6_REST_JSON"

M6_MCP_CALLS=$(echo "$M6_MCP_TEXT" \
    | grep -oE 'Calls \(function/method invocations\) \([0-9]+\)' \
    | grep -oE '[0-9]+' || echo "0")
M6_REST_CALLS=$(jq -r '.calls | length' "$M6_REST_JSON" 2>/dev/null || echo "rest-error")
assert_eq "$M6_MCP_CALLS" "$M6_REST_CALLS" \
    "M6: /mcp on node A and /api/callers on node B agree (calls: $M6_MCP_CALLS vs $M6_REST_CALLS)"

# The TOTAL must agree too, not just the shown bucket. knot emits the true
# pre-truncation target count as `resolution.total_targets`; the Markdown
# states it as "Resolved to N target(s)" (when complete) or in the truncation
# notice (when partial). Default max_targets (25) covers the tiny fixture, so
# here both must report the same total with no truncation.
M6_REST_TOTAL=$(jq -r '.resolution.total_targets' "$M6_REST_JSON")
M6_REST_TARGETS=$(jq -r '.resolution.targets | length' "$M6_REST_JSON")
M6_REST_TRUNC=$(jq -r '.resolution.truncated' "$M6_REST_JSON")
M6_MCP_RESOLVED=$(echo "$M6_MCP_TEXT" \
    | grep -oE 'Resolved to [0-9]+ target' | grep -oE '[0-9]+' | head -1)
assert_eq "$M6_MCP_RESOLVED" "$M6_REST_TOTAL" \
    "M6: /mcp and REST report the same total (resolved: $M6_MCP_RESOLVED vs total_targets: $M6_REST_TOTAL)"
assert_eq "$M6_REST_TARGETS" "$M6_REST_TOTAL" \
    "M6: an untruncated response shows every resolved target"
assert_eq "$M6_REST_TRUNC" "false" \
    "M6: default max_targets leaves the tiny fixture untruncated"
assert_not_contains "$M6_MCP_TEXT" "**Truncated**" \
    "M6: MCP marks the complete result as untruncated, same as REST"

# Same tool, same arguments, two different nodes: the output must be
# byte-identical. knot >= 1.9.4 renders the `### Target:` sections in a stable
# BTreeMap order (1.9.3 used a process-random HashMap, which made the same
# query render targets in a different order across processes), so this is
# deterministic. $TMP/callers-resp.json is the response M3 obtained from the
# restarted node B.
M6_MCP_TEXT_B=$(jq -r '.result.content[0].text' "$TMP/callers-resp.json" 2>/dev/null || echo "")
assert_eq "$M6_MCP_TEXT" "$M6_MCP_TEXT_B" \
    "M6: identical find_callers output from node A and node B"

# ── M7: explicit truncation, quantified the same on both surfaces ─
echo -e "\n${YELLOW}[M7] max_targets is forwarded; truncation is explicit and equal on /mcp and REST${NC}"

# `greet` resolves to multiple targets (interface + overrides). Capping the
# resolution at 1 forces a real truncation, exercising knot's new semantics:
# the response must carry the true pre-truncation total, the shown count, and
# an explicit truncated flag — and /mcp (a faithful passthrough) must agree.
cat > "$TMP/callers-truncated.json" <<'JSON'
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"find_callers","arguments":{"entity_name":"greet","repo_name":"mcp-fixture","max_targets":1}}}
JSON
M7_MCP_STATUS=$(mcp_post "$BASE_A" "$TMP/callers-truncated.json" "callers-truncated")
assert_eq "$M7_MCP_STATUS" "200" "M7: find_callers with max_targets via /mcp returns 200"
assert_no_session_header "$TMP/callers-truncated-headers.txt" \
    "M7: no Mcp-Session-Id on a max_targets tool call"

M7_MCP_TEXT=$(jq -r '.result.content[0].text' "$TMP/callers-truncated-resp.json" 2>/dev/null || echo "")
assert_contains "$M7_MCP_TEXT" "**Truncated**" \
    "M7: /mcp discloses the truncated target resolution"
assert_contains "$M7_MCP_TEXT" "Counts below are partial" \
    "M7: /mcp quantifies the partial bucket counts"

# Pull "shown of total" out of the machine-visible caveat:
# "Counts below are partial — they cover only the 1 of 3 targets shown."
M7_MCP_SHOWN=$(echo "$M7_MCP_TEXT" \
    | grep -oE 'cover only the [0-9]+ of [0-9]+ targets shown' \
    | grep -oE '[0-9]+' | head -1)
M7_MCP_TOTAL=$(echo "$M7_MCP_TEXT" \
    | grep -oE 'cover only the [0-9]+ of [0-9]+ targets shown' \
    | grep -oE '[0-9]+' | tail -1)

M7_REST_JSON="$TMP/callers-truncated-rest.json"
curl -sf "$BASE_B/api/repos/mcp-fixture/callers?entity=greet&max_targets=1" -o "$M7_REST_JSON"
M7_REST_TOTAL=$(jq -r '.resolution.total_targets' "$M7_REST_JSON")
M7_REST_SHOWN=$(jq -r '.resolution.targets | length' "$M7_REST_JSON")
M7_REST_TRUNC=$(jq -r '.resolution.truncated' "$M7_REST_JSON")

assert_eq "$M7_REST_TRUNC" "true" \
    "M7: REST marks the max_targets=1 resolution as truncated"
assert_eq "$M7_MCP_TOTAL" "$M7_REST_TOTAL" \
    "M7: /mcp and REST agree on the TRUE total (mcp: $M7_MCP_TOTAL vs rest: $M7_REST_TOTAL)"
assert_eq "$M7_MCP_SHOWN" "$M7_REST_SHOWN" \
    "M7: /mcp and REST agree on the shown target count (mcp: $M7_MCP_SHOWN vs rest: $M7_REST_SHOWN)"

# ── Summary ──────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}MCP Endpoint E2E: Results${NC}"
echo -e "${GREEN}========================================${NC}"
echo -e "  Passed: ${GREEN}$PASSED${NC}"
echo -e "  Failed: ${RED}$FAILED${NC}"
echo ""
echo "Validated:"
echo "  - Stateless initialize (no Mcp-Session-Id, ever)"
echo "  - No Mcp-Session-Id on ANY response of BOTH nodes (M1/M2/M3/M5/M6)"
echo "  - Cross-node conversation without session affinity (M2)"
echo "  - Fresh node serves tools with zero handshake state (M3)"
echo "  - Real indexed content served through the MCP surface (M4)"
echo "  - Protocol-level error contract, on both nodes (M5)"
echo "  - MCP/REST parity across nodes + identical output per node (M6)"
echo "  - max_targets forwarding + same true total/truncation on both surfaces (M7)"

if [ "$FAILED" -gt 0 ]; then
    echo -e "\n${RED}Some tests FAILED${NC}"
    exit 1
else
    echo -e "\n${GREEN}All MCP endpoint tests PASSED${NC}"
    exit 0
fi
