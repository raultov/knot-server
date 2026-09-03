#!/usr/bin/env bash
# E2E Cross-Repo Scope Test for knot-server
# Validates GET /api/search and GET /api/callers with the `repo` scope
# parameter (all / single / comma-list) against TWO indexed fixture
# repositories that share a homonym entity (SharedUtil.work) and each
# carry one unique entity.
#
# Group S — cross-repo search scenarios
# Group C — cross-repo callers scenarios
# Group G — repo=all registry-confinement scenarios (ghost repository)
# Regression guards pin the per-repo routes as unchanged.

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
CROSS_FIXTURE_DIR="$SCRIPT_DIR/fixtures/cross_repo"
WORKSPACE_DIR="/tmp/knot-crossrepo-e2e-$$"
SERVER_PORT=18087
SERVER_PID=""
SERVER_LOG="/tmp/knot-crossrepo-e2e-$$.log"

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
echo -e "${GREEN}knot-server Cross-Repo Scope E2E${NC}"
echo -e "${GREEN}========================================${NC}"

# ── Step 1: Start Docker containers ──────────────────────────
echo -e "\n${YELLOW}[1/5] Starting Docker containers...${NC}"
cd "$SCRIPT_DIR"
docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
docker compose -f "$COMPOSE_FILE" up -d

# ── Step 2: Wait for databases ───────────────────────────────
echo -e "${YELLOW}[2/5] Waiting for databases...${NC}"

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

# ── Step 3: Build fixture repos + start server ───────────────
echo -e "${YELLOW}[3/5] Building fixture repos + server...${NC}"
cd "$PROJECT_ROOT"

rm -rf "$WORKSPACE_DIR"
mkdir -p "$WORKSPACE_DIR"

# Share fastembed cache
mkdir -p /tmp/fastembed_cache_shared
ln -s /tmp/fastembed_cache_shared "$WORKSPACE_DIR/fastembed_cache"

# Create a bare git repo from a source directory.
# Usage: create_fixture_bare_repo <source_dir> <bare_name>
create_fixture_bare_repo() {
    local src_dir="$1"
    local bare_name="$2"
    local bare_path="$WORKSPACE_DIR/fixtures/$bare_name.git"
    local work_path="$WORKSPACE_DIR/fixtures-tmp-$bare_name"

    rm -rf "$bare_path" "$work_path"
    mkdir -p "$(dirname "$bare_path")"

    git init --bare -q "$bare_path"
    git clone -q "$bare_path" "$work_path"

    cp "$src_dir"/*.java "$work_path/"
    echo "# Cross-Repo E2E Fixture ($bare_name)" > "$work_path/README.md"

    git -C "$work_path" add .
    git -C "$work_path" -c user.email=e2e@test -c user.name=e2e \
        commit -q -m "initial commit with fixture sources"
    git -C "$work_path" push origin main 2>/dev/null \
        || { git -C "$work_path" branch -M main && git -C "$work_path" push -q origin main; }

    rm -rf "$work_path"
    echo "$bare_path"
}

FIXTURE_A=$(create_fixture_bare_repo "$CROSS_FIXTURE_DIR/repo_a" "cross-repo-a")
FIXTURE_B=$(create_fixture_bare_repo "$CROSS_FIXTURE_DIR/repo_b" "cross-repo-b")
echo "  Fixture repo A: $FIXTURE_A"
echo "  Fixture repo B: $FIXTURE_B"

cargo build 2>&1 | grep -E "(Compiling|Finished|error)" || true

# Start knot-server with the canonical environment. Used for the initial
# start and for the Group G restart so both runs share identical settings.
start_knot_server() {
    KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
    KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
    KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
    KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
    KNOT_SERVER_PORT="$SERVER_PORT" \
    KNOT_WORKSPACE_DIR="$WORKSPACE_DIR" \
    KNOT_SERVER_QUEUE_CAPACITY=4 \
    RUST_LOG="${RUST_LOG:-info}" \
        "$PROJECT_ROOT/target/debug/knot-server" >> "$SERVER_LOG" 2>&1 &
    SERVER_PID=$!
}

wait_server_up() {
    local max_wait="${1:-90}"
    for i in $(seq 1 "$max_wait"); do
        if curl -sf "$BASE_URL/api/health" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    return 1
}

start_knot_server

echo -n "  Waiting for knot-server on port $SERVER_PORT..."
if wait_server_up 90; then
    echo -e " ${GREEN}ready${NC}"
else
    echo -e " ${RED}did not start${NC}"
    cat "$SERVER_LOG"
    exit 1
fi

# ── Step 4: Register both repos + wait for indexing ──────────
echo -e "${YELLOW}[4/5] Registering fixture repos + waiting for indexing...${NC}"

wait_indexed() {
    local repo_id="$1"
    local max_wait="${2:-120}"
    local i status last
    for i in $(seq 1 "$max_wait"); do
        status=$(curl -sf "$BASE_URL/api/repos/$repo_id" 2>/dev/null | jq -r '.status' 2>/dev/null || echo "")
        last=$(curl -sf "$BASE_URL/api/repos/$repo_id" 2>/dev/null | jq -r '.last_indexed // ""' 2>/dev/null || echo "")
        if [ "$status" = "indexed" ] && [ -n "$last" ] && [ "$last" != "null" ]; then
            return 0
        elif [ "$status" = "error" ]; then
            echo "  indexing failed for $repo_id"
            tail -30 "$SERVER_LOG" 2>/dev/null || true
            return 1
        fi
        sleep 1
    done
    echo "  indexing did not complete for $repo_id within ${max_wait}s (status: $status)"
    return 1
}

REG_A_BODY=$(mktemp)
REG_A_CODE=$(curl -sf -w "%{http_code}" -o "$REG_A_BODY" \
    -X POST "$BASE_URL/api/repos" \
    -H "Content-Type: application/json" \
    -d "{\"url\": \"$FIXTURE_A\", \"auth_type\": \"ssh\"}")
if [ "$REG_A_CODE" = "202" ]; then
    pass "Register repo A (202)"
else
    echo -e "${RED}SETUP FAILED${NC} — register repo A returned $REG_A_CODE"
    cat "$REG_A_BODY"; exit 1
fi
REPO_A_ID=$(jq -r '.id' "$REG_A_BODY")
rm -f "$REG_A_BODY"
echo "  Repo A ID: $REPO_A_ID"

REG_B_BODY=$(mktemp)
REG_B_CODE=$(curl -sf -w "%{http_code}" -o "$REG_B_BODY" \
    -X POST "$BASE_URL/api/repos" \
    -H "Content-Type: application/json" \
    -d "{\"url\": \"$FIXTURE_B\", \"auth_type\": \"ssh\"}")
if [ "$REG_B_CODE" = "202" ]; then
    pass "Register repo B (202)"
else
    echo -e "${RED}SETUP FAILED${NC} — register repo B returned $REG_B_CODE"
    cat "$REG_B_BODY"; exit 1
fi
REPO_B_ID=$(jq -r '.id' "$REG_B_BODY")
rm -f "$REG_B_BODY"
echo "  Repo B ID: $REPO_B_ID"

if wait_indexed "$REPO_A_ID"; then
    pass "Repo A indexed"
else
    echo -e "${RED}SETUP FAILED${NC}"; exit 1
fi
if wait_indexed "$REPO_B_ID"; then
    pass "Repo B indexed"
else
    echo -e "${RED}SETUP FAILED${NC}"; exit 1
fi

# ── Step 5: Scenario tests ───────────────────────────────────
echo -e "${YELLOW}[5/5] Scenario tests...${NC}"

# jq filter: repo_name values of a search response's entity array.
# The search endpoint returns null when there are no hits — normalise
# with `// []` so count/unique expressions behave.
SEARCH_REPO_NAMES='[.[]? | .repo_name? // empty] | unique'

# ═════════════════════════════════════════════════════════════
echo -e "\n${CYAN}═══ Group S — /api/search ═══${NC}"

# ── S1: default scope spans every indexed repository ──
echo -e "\n${CYAN}S1: default scope finds AlphaService in repo A${NC}"
S1_CODE=$(curl -s -w "%{http_code}" -o /tmp/s1.json "$BASE_URL/api/search?q=AlphaService")
S1_A_HITS=$(jq '[.[]? | select(.repo_name == "'"$REPO_A_ID"'")] | length' /tmp/s1.json 2>/dev/null || echo "0")
if [ "$S1_CODE" = "200" ] && [ "$S1_A_HITS" -ge 1 ]; then
    pass "S1 — status=200, AlphaService found with repo_name=REPO_A"
else
    fail "S1 — status=$S1_CODE, repo_A hits=$S1_A_HITS"
    cat /tmp/s1.json
fi

# ── S2: the union really is a union ──
echo -e "\n${CYAN}S2: SharedUtil found in both repos (default scope)${NC}"
S2_CODE=$(curl -s -w "%{http_code}" -o /tmp/s2.json "$BASE_URL/api/search?q=SharedUtil")
S2_NAMES=$(jq -r "$SEARCH_REPO_NAMES | join(\",\")" /tmp/s2.json 2>/dev/null || echo "")
if [ "$S2_CODE" = "200" ] \
   && echo "$S2_NAMES" | grep -q "$REPO_A_ID" \
   && echo "$S2_NAMES" | grep -q "$REPO_B_ID"; then
    pass "S2 — both repo_name values present: $S2_NAMES"
else
    fail "S2 — status=$S2_CODE, repo_name set: $S2_NAMES"
    cat /tmp/s2.json
fi

# ── S3: explicit sentinel behaves like the default ──
echo -e "\n${CYAN}S3: repo=all behaves like the default${NC}"
S3_CODE=$(curl -s -w "%{http_code}" -o /tmp/s3.json "$BASE_URL/api/search?q=SharedUtil&repo=all")
S3_NAMES=$(jq -r "$SEARCH_REPO_NAMES | join(\",\")" /tmp/s3.json 2>/dev/null || echo "")
if [ "$S3_CODE" = "200" ] && [ "$S3_NAMES" = "$S2_NAMES" ]; then
    pass "S3 — repo=all yields the same repo_name set as the default"
else
    fail "S3 — status=$S3_CODE, default set='$S2_NAMES', sentinel set='$S3_NAMES'"
fi

# ── S4: comma list restricts to the listed repos ──
echo -e "\n${CYAN}S4: repo=<B> restricts results to repo B (homonym must not leak)${NC}"
S4_CODE=$(curl -s -w "%{http_code}" -o /tmp/s4.json "$BASE_URL/api/search?q=SharedUtil&repo=$REPO_B_ID")
S4_NAMES=$(jq -r "$SEARCH_REPO_NAMES | join(\",\")" /tmp/s4.json 2>/dev/null || echo "")
if [ "$S4_CODE" = "200" ] \
   && [ "$S4_NAMES" = "$REPO_B_ID" ]; then
    pass "S4 — all entities confined to repo B"
else
    fail "S4 — status=$S4_CODE, repo_name set: '$S4_NAMES' (expected only '$REPO_B_ID')"
    cat /tmp/s4.json
fi

# ── S5: two-element list returns both ──
echo -e "\n${CYAN}S5: repo=<A>,<B> returns both repos${NC}"
S5_CODE=$(curl -s -w "%{http_code}" -o /tmp/s5.json "$BASE_URL/api/search?q=SharedUtil&repo=$REPO_A_ID,$REPO_B_ID")
S5_NAMES=$(jq -r "$SEARCH_REPO_NAMES | join(\",\")" /tmp/s5.json 2>/dev/null || echo "")
if [ "$S5_CODE" = "200" ] \
   && echo "$S5_NAMES" | grep -q "$REPO_A_ID" \
   && echo "$S5_NAMES" | grep -q "$REPO_B_ID"; then
    pass "S5 — both repos present: $S5_NAMES"
else
    fail "S5 — status=$S5_CODE, repo_name set: $S5_NAMES"
fi

# ── S6: whitespace and duplicates are tolerated ──
echo -e "\n${CYAN}S6: whitespace/duplicates in repo list are normalised${NC}"
S6_CODE=$(curl -s -w "%{http_code}" -o /tmp/s6.json --get --data-urlencode "q=SharedUtil" \
    --data-urlencode "repo= $REPO_A_ID , $REPO_A_ID , $REPO_B_ID " "$BASE_URL/api/search")
S6_NAMES=$(jq -r "$SEARCH_REPO_NAMES | join(\",\")" /tmp/s6.json 2>/dev/null || echo "")
if [ "$S6_CODE" = "200" ] \
   && echo "$S6_NAMES" | grep -q "$REPO_A_ID" \
   && echo "$S6_NAMES" | grep -q "$REPO_B_ID"; then
    pass "S6 — trimmed/deduped scope still spans both repos"
else
    fail "S6 — status=$S6_CODE, repo_name set: $S6_NAMES"
fi

# ── S7: unknown repository names are rejected loudly ──
echo -e "\n${CYAN}S7: unknown repo name rejected with 400${NC}"
S7_CODE=$(curl -s -w "%{http_code}" -o /tmp/s7.json "$BASE_URL/api/search?q=SharedUtil&repo=$REPO_A_ID,ghost")
S7_ERR=$(jq -r '.error // ""' /tmp/s7.json 2>/dev/null || echo "")
if [ "$S7_CODE" = "400" ] && echo "$S7_ERR" | grep -q "ghost"; then
    pass "S7 — 400 mentioning 'ghost': $S7_ERR"
else
    fail "S7 — status=$S7_CODE, error: $S7_ERR"
fi

# ── S8: missing query ──
echo -e "\n${CYAN}S8: missing q rejected with 400${NC}"
S8_CODE=$(curl -s -w "%{http_code}" -o /dev/null "$BASE_URL/api/search")
if [ "$S8_CODE" = "400" ]; then
    pass "S8 — 400 without q"
else
    fail "S8 — expected 400, got $S8_CODE"
fi

# ── S9: max_results is clamped, not honored blindly ──
echo -e "\n${CYAN}S9: max_results=99999 clamped to 100${NC}"
S9_CODE=$(curl -s -w "%{http_code}" -o /tmp/s9.json "$BASE_URL/api/search?q=SharedUtil&max_results=99999")
S9_COUNT=$(jq 'if . == null then 0 else length end' /tmp/s9.json 2>/dev/null || echo "999999")
if [ "$S9_CODE" = "200" ] && [ "$S9_COUNT" -le 100 ]; then
    pass "S9 — status=200 with $S9_COUNT entities (<= 100)"
else
    fail "S9 — status=$S9_CODE, entity count=$S9_COUNT"
fi

# ── S10 (regression guard): per-repo route untouched ──
echo -e "\n${CYAN}S10: per-repo search route unchanged${NC}"
S10_CODE=$(curl -s -w "%{http_code}" -o /tmp/s10.json "$BASE_URL/api/repos/$REPO_A_ID/search?q=AlphaService")
S10_NAMES=$(jq -r "$SEARCH_REPO_NAMES | join(\",\")" /tmp/s10.json 2>/dev/null || echo "")
if [ "$S10_CODE" = "200" ] && [ "$S10_NAMES" = "$REPO_A_ID" ]; then
    pass "S10 — per-repo search still works and is confined to repo A"
else
    fail "S10 — status=$S10_CODE, repo_name set: $S10_NAMES"
fi

# ── S11 (regression guard): repo id never parsed as sentinel ──
echo -e "\n${CYAN}S11: per-repo search ignores scope sentinels${NC}"
S11_CODE=$(curl -s -w "%{http_code}" -o /tmp/s11.json "$BASE_URL/api/repos/$REPO_A_ID/search?q=SharedUtil")
S11_NAMES=$(jq -r "$SEARCH_REPO_NAMES | join(\",\")" /tmp/s11.json 2>/dev/null || echo "")
if [ "$S11_CODE" = "200" ] && [ "$S11_NAMES" = "$REPO_A_ID" ]; then
    pass "S11 — per-repo search results confined to repo A"
else
    fail "S11 — status=$S11_CODE, repo_name set: $S11_NAMES"
fi

# ═════════════════════════════════════════════════════════════
echo -e "\n${CYAN}═══ Group C — /api/callers ═══${NC}"

# jq filter: union of all caller-row names across the six buckets.
CALLERS_ROW_NAMES='[.calls[]?, .extends[]?, .implements[]?, .references[]?, .overridden_by[]?, .overrides[]? | .name?] | unique'
CALLERS_ALL_ROWS='[.calls[]?, .extends[]?, .implements[]?, .references[]?, .overridden_by[]?, .overrides[]?]'

# ── C1: default scope finds callers in every repository ──
echo -e "\n${CYAN}C1: default scope finds alphaCaller and betaCaller${NC}"
C1_CODE=$(curl -s -w "%{http_code}" -o /tmp/c1.json "$BASE_URL/api/callers?entity=SharedUtil.work")
C1_NAMES=$(jq -r "$CALLERS_ROW_NAMES | join(\",\")" /tmp/c1.json 2>/dev/null || echo "")
if [ "$C1_CODE" = "200" ] \
   && echo "$C1_NAMES" | grep -q "alphaCaller" \
   && echo "$C1_NAMES" | grep -q "betaCaller"; then
    pass "C1 — both callers found: $C1_NAMES"
else
    fail "C1 — status=$C1_CODE, row names: $C1_NAMES"
    cat /tmp/c1.json
fi

# ── C2: every returned row is attributable ──
echo -e "\n${CYAN}C2: all rows carry repo_name; alphaCaller->A, betaCaller->B${NC}"
C2_NULL_ROWS=$(jq "$CALLERS_ALL_ROWS | map(select(.repo_name == null)) | length" /tmp/c1.json 2>/dev/null || echo "999")
C2_ALPHA_REPO=$(jq -r "$CALLERS_ALL_ROWS | map(select(.name == \"alphaCaller\") | .repo_name) | unique | join(\",\")" /tmp/c1.json 2>/dev/null || echo "")
C2_BETA_REPO=$(jq -r "$CALLERS_ALL_ROWS | map(select(.name == \"betaCaller\") | .repo_name) | unique | join(\",\")" /tmp/c1.json 2>/dev/null || echo "")
if [ "$C2_NULL_ROWS" = "0" ] \
   && echo "$C2_ALPHA_REPO" | grep -q "^$REPO_A_ID$" \
   && echo "$C2_BETA_REPO" | grep -q "^$REPO_B_ID$"; then
    pass "C2 — every row labeled; alphaCaller→A, betaCaller→B"
else
    fail "C2 — rows without repo_name=$C2_NULL_ROWS, alphaCaller repos='$C2_ALPHA_REPO', betaCaller repos='$C2_BETA_REPO'"
fi

# ── C3: resolution targets are attributable too ──
echo -e "\n${CYAN}C3: resolution.targets has one target per repository${NC}"
C3_COUNT=$(jq '.resolution.targets | length' /tmp/c1.json 2>/dev/null || echo "0")
C3_REPOS=$(jq -r '[.resolution.targets[]? | .repo_name? // empty] | unique | sort | join(",")' /tmp/c1.json 2>/dev/null || echo "")
C3_EXPECTED=$(printf '%s\n%s' "$REPO_A_ID" "$REPO_B_ID" | sort | paste -sd, -)
if [ "$C3_COUNT" = "2" ] && [ "$C3_REPOS" = "$C3_EXPECTED" ]; then
    pass "C3 — 2 targets with repo_name set {$REPO_A_ID, $REPO_B_ID}"
else
    fail "C3 — targets=$C3_COUNT, repo_name set='$C3_REPOS' (expected '$C3_EXPECTED')"
    jq '.resolution' /tmp/c1.json
fi

# ── C4: single-repo scope restricts and still labels ──
echo -e "\n${CYAN}C4: repo=<A> restricts callers to repo A${NC}"
C4_CODE=$(curl -s -w "%{http_code}" -o /tmp/c4.json "$BASE_URL/api/callers?entity=SharedUtil.work&repo=$REPO_A_ID")
C4_NAMES=$(jq -r "$CALLERS_ROW_NAMES | join(\",\")" /tmp/c4.json 2>/dev/null || echo "")
C4_ALPHA_REPO=$(jq -r "$CALLERS_ALL_ROWS | map(select(.name == \"alphaCaller\") | .repo_name) | unique | join(\",\")" /tmp/c4.json 2>/dev/null || echo "")
if [ "$C4_CODE" = "200" ] \
   && echo "$C4_NAMES" | grep -q "alphaCaller" \
   && ! echo "$C4_NAMES" | grep -q "betaCaller" \
   && echo "$C4_ALPHA_REPO" | grep -q "^$REPO_A_ID$"; then
    pass "C4 — alphaCaller (repo A) present, betaCaller absent"
else
    fail "C4 — status=$C4_CODE, names: $C4_NAMES, alphaCaller repo: $C4_ALPHA_REPO"
fi

# ── C5: comma list unions the listed repos ──
echo -e "\n${CYAN}C5: repo=<A>,<B> unions callers from both repos${NC}"
C5_CODE=$(curl -s -w "%{http_code}" -o /tmp/c5.json "$BASE_URL/api/callers?entity=SharedUtil.work&repo=$REPO_A_ID,$REPO_B_ID")
C5_NAMES=$(jq -r "$CALLERS_ROW_NAMES | join(\",\")" /tmp/c5.json 2>/dev/null || echo "")
if [ "$C5_CODE" = "200" ] \
   && echo "$C5_NAMES" | grep -q "alphaCaller" \
   && echo "$C5_NAMES" | grep -q "betaCaller"; then
    pass "C5 — both callers present under the comma-list scope"
else
    fail "C5 — status=$C5_CODE, names: $C5_NAMES"
fi

# ── C6: unknown repository names are rejected loudly ──
echo -e "\n${CYAN}C6: unknown repo name rejected with 400${NC}"
C6_CODE=$(curl -s -w "%{http_code}" -o /tmp/c6.json "$BASE_URL/api/callers?entity=SharedUtil.work&repo=ghost")
C6_ERR=$(jq -r '.error // ""' /tmp/c6.json 2>/dev/null || echo "")
if [ "$C6_CODE" = "400" ] && echo "$C6_ERR" | grep -q "ghost"; then
    pass "C6 — 400 mentioning 'ghost': $C6_ERR"
else
    fail "C6 — status=$C6_CODE, error: $C6_ERR"
fi

# ── C7: missing entity ──
echo -e "\n${CYAN}C7: missing entity rejected with 400${NC}"
C7_CODE=$(curl -s -w "%{http_code}" -o /tmp/c7.json "$BASE_URL/api/callers")
C7_ERR=$(jq -r '.error // ""' /tmp/c7.json 2>/dev/null || echo "")
if [ "$C7_CODE" = "400" ] && echo "$C7_ERR" | grep -q "entity"; then
    pass "C7 — 400 mentioning 'entity': $C7_ERR"
else
    fail "C7 — status=$C7_CODE, error: $C7_ERR"
fi

# ── C8: unreferenced entity is an empty object, never null ──
echo -e "\n${CYAN}C8: BillingService (unreferenced) returns the six buckets${NC}"
C8_CODE=$(curl -s -w "%{http_code}" -o /tmp/c8.json "$BASE_URL/api/callers?entity=BillingService&repo=$REPO_B_ID")
C8_IS_OBJ=$(jq 'type == "object"' /tmp/c8.json 2>/dev/null || echo "false")
C8_BUCKETS=$(jq '[.calls, .extends, .implements, .references, .overridden_by, .overrides] | map(type == "array") | all' /tmp/c8.json 2>/dev/null || echo "false")
C8_EMPTY_OK=$(jq '[.calls[]?, .extends[]?, .implements[]?, .references[]?, .overridden_by[]?, .overrides[]?] | length == 0' /tmp/c8.json 2>/dev/null || echo "false")
if [ "$C8_CODE" = "200" ] && [ "$C8_IS_OBJ" = "true" ] && [ "$C8_BUCKETS" = "true" ] && [ "$C8_EMPTY_OK" = "true" ]; then
    pass "C8 — object with six empty buckets (never null)"
else
    fail "C8 — status=$C8_CODE, is_object=$C8_IS_OBJ, buckets_ok=$C8_BUCKETS, empty_ok=$C8_EMPTY_OK"
    cat /tmp/c8.json
fi

# ── C9 (regression guard): per-repo callers route untouched ──
echo -e "\n${CYAN}C9: per-repo callers route unchanged (rows self-labeled via knot 1.8.1)${NC}"
C9_CODE=$(curl -s -w "%{http_code}" -o /tmp/c9.json "$BASE_URL/api/repos/$REPO_A_ID/callers?entity=SharedUtil.work")
C9_NAMES=$(jq -r "$CALLERS_ROW_NAMES | join(\",\")" /tmp/c9.json 2>/dev/null || echo "")
C9_ALPHA_REPO=$(jq -r "$CALLERS_ALL_ROWS | map(select(.name == \"alphaCaller\") | .repo_name) | unique | join(\",\")" /tmp/c9.json 2>/dev/null || echo "")
if [ "$C9_CODE" = "200" ] \
   && echo "$C9_NAMES" | grep -q "alphaCaller" \
   && ! echo "$C9_NAMES" | grep -q "betaCaller" \
   && echo "$C9_ALPHA_REPO" | grep -q "^$REPO_A_ID$"; then
    pass "C9 — alphaCaller (repo_name=$REPO_A_ID) present, betaCaller absent"
else
    fail "C9 — status=$C9_CODE, names: $C9_NAMES, alphaCaller repo: '$C9_ALPHA_REPO'"
fi

# ═════════════════════════════════════════════════════════════
echo -e "\n${CYAN}═══ Group G — repo=all registry confinement ═══${NC}"

# Setup: index a third fixture repo ("ghost"), then deregister it behind
# the server's back — stop knot-server, drop its entry from the on-disk
# registry, restart with the same environment. The Neo4j/Qdrant rows
# survive, simulating a repo deleted from the registry with orphaned rows.
echo -e "\n${CYAN}G-setup: ghost repository lifecycle${NC}"

FIXTURE_C=$(create_fixture_bare_repo "$CROSS_FIXTURE_DIR/repo_c" "cross-repo-ghost")
echo "  Fixture repo C: $FIXTURE_C"

REG_C_BODY=$(mktemp)
REG_C_CODE=$(curl -sf -w "%{http_code}" -o "$REG_C_BODY" \
    -X POST "$BASE_URL/api/repos" \
    -H "Content-Type: application/json" \
    -d "{\"url\": \"$FIXTURE_C\", \"auth_type\": \"ssh\"}")
if [ "$REG_C_CODE" = "202" ]; then
    pass "Register repo C (202)"
else
    echo -e "${RED}SETUP FAILED${NC} — register repo C returned $REG_C_CODE"
    cat "$REG_C_BODY"; exit 1
fi
REPO_C_ID=$(jq -r '.id' "$REG_C_BODY")
rm -f "$REG_C_BODY"
echo "  Repo C ID: $REPO_C_ID"

if wait_indexed "$REPO_C_ID"; then
    pass "Repo C (ghost) indexed"
else
    echo -e "${RED}SETUP FAILED${NC}"; exit 1
fi

# Precondition: the ghost rows must be retrievable while the repo is
# still registered, so a G1-G3 red state is attributable to the scope
# leak and not to a broken fixture.
GHOST_SANITY_CODE=$(curl -s -w "%{http_code}" -o /tmp/ghost_sanity.json \
    "$BASE_URL/api/search?q=GhostEntity&repo=$REPO_C_ID")
GHOST_SANITY_HITS=$(jq 'if . == null then 0 else length end' /tmp/ghost_sanity.json 2>/dev/null || echo "0")
if [ "$GHOST_SANITY_CODE" = "200" ] && [ "$GHOST_SANITY_HITS" -ge 1 ]; then
    pass "Ghost rows retrievable while registered ($GHOST_SANITY_HITS entities)"
else
    echo -e "${RED}SETUP FAILED${NC} — GhostEntity not retrievable from registered repo C (status=$GHOST_SANITY_CODE, hits=$GHOST_SANITY_HITS)"
    cat /tmp/ghost_sanity.json 2>/dev/null; exit 1
fi

echo -n "  Stopping knot-server..."
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
echo -e " ${GREEN}stopped${NC}"

if [ ! -f "$WORKSPACE_DIR/repos.json" ]; then
    echo -e "${RED}SETUP FAILED${NC} — $WORKSPACE_DIR/repos.json not found"
    exit 1
fi
jq --arg id "$REPO_C_ID" '.repositories |= map(select(.id != $id))' \
    "$WORKSPACE_DIR/repos.json" > "$WORKSPACE_DIR/repos.json.g.tmp"
mv "$WORKSPACE_DIR/repos.json.g.tmp" "$WORKSPACE_DIR/repos.json"
GHOST_LEFT=$(jq --arg id "$REPO_C_ID" '[.repositories[]? | select(.id == $id)] | length' \
    "$WORKSPACE_DIR/repos.json" 2>/dev/null || echo "999")
if [ "$GHOST_LEFT" = "0" ]; then
    pass "Removed '$REPO_C_ID' from repos.json (DB rows left behind)"
else
    echo -e "${RED}SETUP FAILED${NC} — '$REPO_C_ID' still present in repos.json"
    exit 1
fi

echo -n "  Restarting knot-server..."
start_knot_server
if wait_server_up 90; then
    echo -e " ${GREEN}ready${NC}"
else
    echo -e " ${RED}did not restart${NC}"
    tail -50 "$SERVER_LOG"
    exit 1
fi

# ── G1: repo=all is confined to registered repositories ──
echo -e "\n${CYAN}G1: repo=all (and the omitted default) finds no ghost rows${NC}"
G1_CODE=$(curl -s -w "%{http_code}" -o /tmp/g1.json "$BASE_URL/api/search?q=GhostEntity&max_results=100")
G1_GHOST=$(jq --arg id "$REPO_C_ID" '[.[]? | select(.repo_name == $id)] | length' /tmp/g1.json 2>/dev/null || echo "999")
if [ "$G1_CODE" = "200" ] && [ "$G1_GHOST" = "0" ]; then
    pass "G1 — status=200, no entity from '$REPO_C_ID'"
else
    fail "G1 — status=$G1_CODE, ghost entities=$G1_GHOST (scope leaked unregistered '$REPO_C_ID')"
    cat /tmp/g1.json
fi

# ── G2: omitting repo behaves identically to repo=all ──
echo -e "\n${CYAN}G2: omitted repo and repo=all yield the same confined set${NC}"
G2A_CODE=$(curl -s -w "%{http_code}" -o /tmp/g2a.json "$BASE_URL/api/search?q=GhostEntity&max_results=100")
G2B_CODE=$(curl -s -w "%{http_code}" -o /tmp/g2b.json "$BASE_URL/api/search?q=GhostEntity&repo=all&max_results=100")
G2A_SET=$(jq -r "$SEARCH_REPO_NAMES | join(\",\")" /tmp/g2a.json 2>/dev/null || echo "")
G2B_SET=$(jq -r "$SEARCH_REPO_NAMES | join(\",\")" /tmp/g2b.json 2>/dev/null || echo "")
if [ "$G2A_CODE" = "200" ] && [ "$G2B_CODE" = "200" ] \
   && [ "$G2A_SET" = "$G2B_SET" ] \
   && ! echo "$G2A_SET" | grep -q "$REPO_C_ID" \
   && ! echo "$G2B_SET" | grep -q "$REPO_C_ID"; then
    pass "G2 — identical repo_name sets, no ghost rows: '$G2A_SET'"
else
    fail "G2 — omitted: status=$G2A_CODE set='$G2A_SET', repo=all: status=$G2B_CODE set='$G2B_SET'"
fi

# ── G3: callers under repo=all is confined too ──
echo -e "\n${CYAN}G3: callers buckets/targets carry no ghost rows${NC}"
G3_CODE=$(curl -s -w "%{http_code}" -o /tmp/g3.json "$BASE_URL/api/callers?entity=GhostEntity")
G3_ROW_GHOSTS=$(jq --arg id "$REPO_C_ID" \
    '[.calls[]?, .extends[]?, .implements[]?, .references[]?, .overridden_by[]?, .overrides[]?
      | select((.repo_name? // "") == $id or (.target_repo_name? // "") == $id)] | length' \
    /tmp/g3.json 2>/dev/null || echo "999")
G3_TARGET_GHOSTS=$(jq --arg id "$REPO_C_ID" \
    '[.resolution.targets[]? | select((.repo_name? // "") == $id)] | length' \
    /tmp/g3.json 2>/dev/null || echo "999")
if [ "$G3_CODE" = "200" ] && [ "$G3_ROW_GHOSTS" = "0" ] && [ "$G3_TARGET_GHOSTS" = "0" ]; then
    pass "G3 — status=200, no caller row or resolution target from '$REPO_C_ID'"
else
    fail "G3 — status=$G3_CODE, ghost caller rows=$G3_ROW_GHOSTS, ghost targets=$G3_TARGET_GHOSTS"
    jq . /tmp/g3.json
fi

# ── G4: registered repositories are still fully reachable under all ──
echo -e "\n${CYAN}G4: repo=all still reaches repos A and B${NC}"
G4_CODE=$(curl -s -w "%{http_code}" -o /tmp/g4.json "$BASE_URL/api/search?q=SharedUtil&repo=all")
G4_A=$(jq --arg id "$REPO_A_ID" '[.[]? | select(.repo_name == $id)] | length' /tmp/g4.json 2>/dev/null || echo "0")
G4_B=$(jq --arg id "$REPO_B_ID" '[.[]? | select(.repo_name == $id)] | length' /tmp/g4.json 2>/dev/null || echo "0")
if [ "$G4_CODE" = "200" ] && [ "$G4_A" -ge 1 ] && [ "$G4_B" -ge 1 ]; then
    pass "G4 — SharedUtil found in repo A ($G4_A hits) and repo B ($G4_B hits)"
else
    fail "G4 — status=$G4_CODE, repo A hits=$G4_A, repo B hits=$G4_B"
    cat /tmp/g4.json
fi

# ── G5 (regression): named scopes and per-repo routes are unchanged ──
echo -e "\n${CYAN}G5: named scopes and per-repo routes unchanged${NC}"
G5A_CODE=$(curl -s -w "%{http_code}" -o /tmp/g5a.json "$BASE_URL/api/search?q=SharedUtil&repo=$REPO_A_ID")
G5A_OTHERS=$(jq --arg id "$REPO_A_ID" '[.[]? | select(.repo_name != $id)] | length' /tmp/g5a.json 2>/dev/null || echo "999")
if [ "$G5A_CODE" = "200" ] && [ "$G5A_OTHERS" = "0" ]; then
    pass "G5a — repo=$REPO_A_ID confined to repo A"
else
    fail "G5a — status=$G5A_CODE, non-repo-A entities=$G5A_OTHERS"
fi

G5B_CODE=$(curl -s -w "%{http_code}" -o /tmp/g5b.json "$BASE_URL/api/search?q=SharedUtil&repo=$REPO_C_ID")
G5B_ERR=$(jq -r '.error // ""' /tmp/g5b.json 2>/dev/null || echo "")
if [ "$G5B_CODE" = "400" ] && echo "$G5B_ERR" | grep -q "$REPO_C_ID"; then
    pass "G5b — 400 mentioning '$REPO_C_ID': $G5B_ERR"
else
    fail "G5b — status=$G5B_CODE, error: $G5B_ERR"
fi

G5C_CODE=$(curl -s -w "%{http_code}" -o /tmp/g5c.json "$BASE_URL/api/repos/$REPO_A_ID/search?q=SharedUtil")
G5C_OTHERS=$(jq --arg id "$REPO_A_ID" '[.[]? | select(.repo_name != $id)] | length' /tmp/g5c.json 2>/dev/null || echo "999")
if [ "$G5C_CODE" = "200" ] && [ "$G5C_OTHERS" = "0" ]; then
    pass "G5c — per-repo search route confined to repo A"
else
    fail "G5c — status=$G5C_CODE, non-repo-A entities=$G5C_OTHERS"
fi

# ── G6: empty-body drift guard for /api/callers ──
echo -e "\n${CYAN}G6: callers empty-body key sets pinned${NC}"
G6_CODE=$(curl -s -w "%{http_code}" -o /tmp/g6.json "$BASE_URL/api/callers?entity=NoSuchEntityXyz123")
G6_KEYS=$(jq -r 'keys | sort | join(",")' /tmp/g6.json 2>/dev/null || echo "")
G6_RES_KEYS=$(jq -r '.resolution | keys | sort | join(",")' /tmp/g6.json 2>/dev/null || echo "")
G6_EXPECTED_KEYS="calls,extends,implements,overridden_by,overrides,references,resolution"
G6_EXPECTED_RES_KEYS="fuzzy,query,targets,tier,truncated"
if [ "$G6_CODE" = "200" ] \
   && [ "$G6_KEYS" = "$G6_EXPECTED_KEYS" ] \
   && [ "$G6_RES_KEYS" = "$G6_EXPECTED_RES_KEYS" ]; then
    pass "G6 — top-level and resolution key sets match the pinned shape"
else
    fail "G6 — status=$G6_CODE, top-level keys='$G6_KEYS' (expected '$G6_EXPECTED_KEYS'), resolution keys='$G6_RES_KEYS' (expected '$G6_EXPECTED_RES_KEYS')"
    cat /tmp/g6.json
fi

# ── Summary ──────────────────────────────────────────────────
echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Cross-Repo Scope E2E: Results${NC}"
echo -e "${GREEN}========================================${NC}"
echo -e "  Passed: ${GREEN}$PASSED${NC}"
echo -e "  Failed: ${RED}$FAILED${NC}"

if [ "$FAILED" -gt 0 ]; then
    echo -e "\n${RED}Some tests FAILED${NC}"
    exit 1
else
    echo -e "\n${GREEN}All cross-repo scope tests PASSED${NC}"
    exit 0
fi
