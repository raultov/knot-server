#!/usr/bin/env bash
# E2E regression test for Issue #7 — "Remove repo on fail" + re-registration race.
#
# Covers the behaviour implemented in worker.rs / cleanup.rs / handlers/repo.rs:
#   Bug A  Re-registering a healthy, indexed repo re-clones it from scratch
#          under the worker's file lock and never corrupts it (no background
#          cleanup race → never "git fetch failed ... 255").
#   Bug B  A repo whose clone never succeeded (last_indexed == None) is fully
#          wiped on failure: status=error, entry KEPT in the registry, local
#          dir gone, progress snapshot gone, no data in Neo4j.
#   §2.2   A `sync` (Pull) on an errored repo whose local dir is missing falls
#          back to a fresh clone instead of failing with "cannot pull".
#   §2.3   A previously-indexed repo that fails a transient pull KEEPS its
#          local dir and its index (no destructive wipe).
#   Recov. Re-registering an errored repo (same derived id) recovers it to
#          indexed.
#   #11    Re-registering an indexed repo with an unreachable source keeps its
#          index and local dir (a missing local path is rejected with 400).
#   #10    A second knot-server on the same workspace and port exits before
#          touching the registry, and a repo left in an in-progress status
#          with no worker holding its lock is re-enqueued on its own.
#
# Follows the conventions of run_e2e.sh: spins up the shared Qdrant + Neo4j
# e2e containers, runs the server against a local fixture bare repo, and drives
# everything through the public REST API + on-disk workspace inspection.

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
FIXTURE_DIR="$SCRIPT_DIR/fixtures"
WORKSPACE_DIR="/tmp/knot-e2e-recovery-workspace-$$"
SERVER_PORT=18081
SERVER_PID=""
SERVER_LOG="/tmp/knot-server-recovery-e2e-$$.log"
SECOND_LOG="/tmp/knot-server-recovery-e2e-second-$$.log"
SECOND_PID=""

NEO4J_URI="bolt://localhost:17687"
NEO4J_USER="neo4j"
NEO4J_PASSWORD="e2e_test_password"
QDRANT_URL="http://localhost:16334"
BASE_URL="http://localhost:$SERVER_PORT"

echo -e "${GREEN}==================================================${NC}"
echo -e "${GREEN}knot-server E2E — Issue #7 reindex/recovery on fail${NC}"
echo -e "${GREEN}==================================================${NC}"

cleanup() {
    local exit_code=$?
    if [ -n "$SECOND_PID" ] && kill -0 "$SECOND_PID" 2>/dev/null; then
        kill "$SECOND_PID" 2>/dev/null || true
    fi
    rm -f "$SECOND_LOG"
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    cd "$SCRIPT_DIR"
    docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
    rm -rf "$WORKSPACE_DIR" 2>/dev/null || true
    cp "$SERVER_LOG" "$SCRIPT_DIR/.last-recovery-e2e-server.log" 2>/dev/null || true
    rm -f "$SERVER_LOG"
    if [ $exit_code -ne 0 ]; then
        echo -e "\n${RED}Tests failed!${NC}"
    fi
    exit $exit_code
}
trap cleanup EXIT INT TERM

fail() {
    echo -e "${RED}FAIL${NC} — $1"
    echo "Server log tail:"
    tail -40 "$SERVER_LOG" 2>/dev/null || true
    exit 1
}

wait_for_port() {
    local port="$1" label="$2" max_wait="${3:-60}"
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

# Build a bare git repo with real (parseable) fixture sources so the indexing
# pipeline produces entities. Echoes the bare repo path on stdout.
create_bare_with_sources() {
    local bare_path="$1"
    local work_path="${bare_path%.git}-work-$RANDOM"
    rm -rf "$bare_path" "$work_path"
    mkdir -p "$(dirname "$bare_path")"

    git init --bare -b main "$bare_path" >/dev/null 2>&1
    git clone "$bare_path" "$work_path" >/dev/null 2>&1
    cp "$FIXTURE_DIR"/*.java "$work_path/" 2>/dev/null || true
    echo "# recovery e2e" > "$work_path/README.md"
    (
        cd "$work_path"
        git checkout -b main >/dev/null 2>&1 || true
        git add .
        git -c user.email=e2e@test -c user.name=e2e commit -m "seed" >/dev/null 2>&1
        git push origin main >/dev/null 2>&1
    )
    rm -rf "$work_path"
    echo "$bare_path"
}

# Register a URL, assert 202, echo the derived repo id on stdout.
register_repo() {
    local url="$1"
    local body code id
    body="$(mktemp)"
    code=$(curl -s -w "%{http_code}" -o "$body" \
        -X POST "$BASE_URL/api/repos" \
        -H "Content-Type: application/json" \
        -d "{\"url\": \"$url\", \"auth_type\": \"ssh\"}")
    if [ "$code" != "202" ]; then
        echo -e "${RED}register returned $code (expected 202)${NC}" >&2
        cat "$body" >&2
        rm -f "$body"
        return 1
    fi
    id=$(jq -r '.id' "$body")
    rm -f "$body"
    echo "$id"
}

repo_status() {
    curl -sf "$BASE_URL/api/repos/$1" 2>/dev/null | jq -r '.status' 2>/dev/null || echo ""
}

repo_last_indexed() {
    curl -sf "$BASE_URL/api/repos/$1" 2>/dev/null | jq -r '.last_indexed // ""' 2>/dev/null || echo ""
}

repo_exists_in_list() {
    curl -sf "$BASE_URL/api/repos" 2>/dev/null \
        | jq -e ".repositories[] | select(.id == \"$1\")" >/dev/null 2>&1
}

wait_status() {
    local id="$1" want="$2" max="${3:-90}" s
    for _ in $(seq 1 "$max"); do
        s=$(repo_status "$id")
        [ "$s" = "$want" ] && return 0
        sleep 1
    done
    echo -e "${RED}timed out waiting for '$id' to reach status='$want' (last='$s')${NC}" >&2
    return 1
}

# Wait until last_indexed CHANGES from a captured baseline and status=indexed.
# Robust against the "already indexed" race right after a re-register/sync.
wait_reindexed() {
    local id="$1" before="$2" max="${3:-90}" s last
    for _ in $(seq 1 "$max"); do
        s=$(repo_status "$id")
        last=$(repo_last_indexed "$id")
        if [ "$s" = "indexed" ] && [ -n "$last" ] && [ "$last" != "$before" ]; then
            return 0
        fi
        if [ "$s" = "error" ]; then
            echo -e "${RED}repo '$id' went to error while waiting for re-index${NC}" >&2
            return 1
        fi
        sleep 1
    done
    echo -e "${RED}timed out waiting for '$id' to re-index (status='$s', last='$last', before='$before')${NC}" >&2
    return 1
}

# Rewrite one repo's status in repos.json behind the server's back, as a
# process that dies mid-job leaves it. The server reloads the changed file.
set_repo_status() {
    local id="$1" status="$2" tmp
    tmp=$(mktemp "$WORKSPACE_DIR/repos.json.e2e.XXXXXX")
    jq --arg id "$id" --arg st "$status" \
        '(.repositories[] | select(.id == $id) | .status) = $st' \
        "$WORKSPACE_DIR/repos.json" >"$tmp"
    mv "$tmp" "$WORKSPACE_DIR/repos.json"
}

neo4j_entity_count() {
    local id="$1"
    docker exec knot_server_neo4j_e2e cypher-shell -u neo4j -p "$NEO4J_PASSWORD" \
        "MATCH (e:Entity {repo_name: '$id'}) RETURN count(e) AS cnt" 2>/dev/null \
        | grep -oE '[0-9]+' | head -1
}

# -------------------------------------------------------
# Step 1: containers
# -------------------------------------------------------
echo -e "${YELLOW}[1/5] Starting Docker containers...${NC}"
cd "$SCRIPT_DIR"
docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
docker compose -f "$COMPOSE_FILE" up -d

echo -e "${YELLOW}[2/5] Waiting for databases...${NC}"
wait_for_port 17687 "Neo4j" 60
wait_for_port 16334 "Qdrant" 30
echo -n "  Waiting for Neo4j health check..."
for i in $(seq 1 60); do
    STATUS=$(docker inspect --format='{{.State.Health.Status}}' knot_server_neo4j_e2e 2>/dev/null || echo "unknown")
    if [ "$STATUS" = "healthy" ]; then
        echo -e " ${GREEN}healthy${NC}"
        break
    fi
    [ "$i" -eq 60 ] && { echo -e " ${RED}timeout (status: $STATUS)${NC}"; exit 1; }
    sleep 1
done
sleep 3

# -------------------------------------------------------
# Step 2: fixtures + server
# -------------------------------------------------------
echo -e "${YELLOW}[3/5] Building server + creating fixtures...${NC}"
cd "$PROJECT_ROOT"
rm -rf "$WORKSPACE_DIR"
mkdir -p "$WORKSPACE_DIR"

# Share the fastembed cache across runs to avoid HF rate limits.
mkdir -p /tmp/fastembed_cache_shared
ln -s /tmp/fastembed_cache_shared "$WORKSPACE_DIR/fastembed_cache"

HEALTHY_BARE=$(create_bare_with_sources "$WORKSPACE_DIR/origins/alpha.git")
echo "  Healthy fixture repo: $HEALTHY_BARE"

cargo build 2>&1 | grep -E "(Compiling|Finished|error)" || true

# Same environment for the server under test and the second instance of
# Scenario 7. The 1s stuck check lets Scenario 8 observe self-healing. `exec`
# makes `$!` of `run_server &` the server's PID, so cleanup can stop it.
run_server() {
    KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
    KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
    KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
    KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
    KNOT_SERVER_PORT="$SERVER_PORT" \
    KNOT_WORKSPACE_DIR="$WORKSPACE_DIR" \
    KNOT_SERVER_QUEUE_CAPACITY="${KNOT_SERVER_QUEUE_CAPACITY:-4}" \
    KNOT_SERVER_STUCK_CHECK_INTERVAL_SECS=1 \
    RUST_LOG="${RUST_LOG:-info}" \
        exec "$PROJECT_ROOT/target/debug/knot-server"
}

run_server >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

echo -n "  Waiting for knot-server on port $SERVER_PORT (up to 90s for fastembed init)..."
for i in $(seq 1 90); do
    if curl -sf "$BASE_URL/api/repos" >/dev/null 2>&1; then
        echo -e " ${GREEN}ready${NC}"
        break
    fi
    [ "$i" -eq 90 ] && { echo -e " ${RED}did not start${NC}"; exit 1; }
    sleep 1
done

# -------------------------------------------------------
# Step 3: the scenarios
# -------------------------------------------------------
echo -e "${YELLOW}[4/5] Running recovery scenarios...${NC}"

# ── Scenario 1: Bug A — re-registering a healthy repo re-clones cleanly ──
echo -e "\n${CYAN}Scenario 1: Re-register a healthy, indexed repo (Bug A)${NC}"
ALPHA_ID=$(register_repo "$HEALTHY_BARE") || fail "initial registration"
echo "  id=$ALPHA_ID"
wait_status "$ALPHA_ID" "indexed" 90 || fail "initial index did not complete"
echo -e "  ${GREEN}initial index complete${NC}"

# Plant a STALE marker that does not exist in origin. A correct fresh clone
# (wipe + clone) must make it disappear; a buggy pull would keep it.
STALE_FILE="$WORKSPACE_DIR/$ALPHA_ID/STALE_MARKER.txt"
[ -d "$WORKSPACE_DIR/$ALPHA_ID/.git" ] || fail "expected local checkout at $WORKSPACE_DIR/$ALPHA_ID"
echo "stale" > "$STALE_FILE"

# Re-register the SAME url three times in a row; each must end 'indexed',
# never 'error', and must re-clone (STALE marker gone). This is the exact
# regression for the "git fetch failed ... 255" corruption.
for n in 1 2 3; do
    echo -e "  ${CYAN}re-register attempt $n${NC}"
    BEFORE=$(repo_last_indexed "$ALPHA_ID")
    sleep 1  # last_indexed has 1s granularity
    RID=$(register_repo "$HEALTHY_BARE") || fail "re-register #$n"
    [ "$RID" = "$ALPHA_ID" ] || fail "re-register produced a different id ($RID)"
    wait_reindexed "$ALPHA_ID" "$BEFORE" 90 || fail "re-register #$n did not reach indexed"
    [ ! -f "$STALE_FILE" ] || fail "re-register #$n did not re-clone (STALE marker survived)"
    # Re-plant for the next iteration.
    [ "$n" -lt 3 ] && echo "stale" > "$STALE_FILE"
done
echo -e "  ${GREEN}PASS${NC} — 3× re-register stayed 'indexed' and re-cloned each time"

if grep -qE "git fetch failed|exit code: 255|Unable to read current working directory" "$SERVER_LOG"; then
    fail "server log shows the Bug A corruption signature (fetch 255 / cwd gone)"
fi
echo -e "  ${GREEN}PASS${NC} — no fetch-255 / cwd-gone signature in the log"

# ── Scenario 2: Bug B — a never-indexed clone failure is fully wiped ──
echo -e "\n${CYAN}Scenario 2: Failed initial clone is wiped but entry kept (issue #7)${NC}"
# An unreachable remote (nothing listens on port 1) → the worker's pre-clone
# reachability check fails, last_indexed stays None. A missing *local* path
# would be rejected with 400 at registration (Scenario 6).
GHOST_BROKEN="https://127.0.0.1:1/origins/ghostrepo.git"
GHOST_ID=$(register_repo "$GHOST_BROKEN") || fail "registration of broken url"
echo "  id=$GHOST_ID"
wait_status "$GHOST_ID" "error" 60 || fail "broken clone did not reach 'error'"
echo -e "  ${GREEN}status=error${NC}"

repo_exists_in_list "$GHOST_ID" || fail "errored entry must remain visible in GET /api/repos"
echo -e "  ${GREEN}PASS${NC} — entry still present in the registry"

[ ! -e "$WORKSPACE_DIR/$GHOST_ID" ] || fail "local dir must be removed for a never-indexed failure"
echo -e "  ${GREEN}PASS${NC} — local directory removed"

[ ! -e "$WORKSPACE_DIR/progress/$GHOST_ID.json" ] || fail "progress snapshot must be removed"
echo -e "  ${GREEN}PASS${NC} — progress snapshot removed"

GCOUNT=$(neo4j_entity_count "$GHOST_ID")
if [ -z "$GCOUNT" ]; then
    echo -e "  ${YELLOW}SKIP${NC} — cypher-shell unavailable, cannot check Neo4j"
elif [ "$GCOUNT" -eq 0 ]; then
    echo -e "  ${GREEN}PASS${NC} — no Neo4j entities for the errored repo ($GCOUNT)"
else
    fail "Neo4j still has $GCOUNT entities for '$GHOST_ID'"
fi

# ── Scenario 3: recover the errored repo by registering a valid same-id url ──
echo -e "\n${CYAN}Scenario 3: Recover the errored repo via a same-id valid url${NC}"
# A valid bare repo whose basename also derives id 'ghostrepo'.
GHOST_GOOD=$(create_bare_with_sources "$WORKSPACE_DIR/origins/recovered/ghostrepo.git")
RID=$(register_repo "$GHOST_GOOD") || fail "recovery registration"
[ "$RID" = "$GHOST_ID" ] || fail "recovery url derived a different id ($RID vs $GHOST_ID)"
wait_status "$GHOST_ID" "indexed" 90 || fail "recovery did not reach 'indexed'"
[ -d "$WORKSPACE_DIR/$GHOST_ID/.git" ] || fail "recovered repo was not cloned on disk"
echo -e "  ${GREEN}PASS${NC} — errored repo recovered to 'indexed'"

# ── Scenario 4: sync (Pull) on a repo whose local dir vanished → fresh clone ──
echo -e "\n${CYAN}Scenario 4: sync falls back to fresh-clone when local dir is missing${NC}"
# Delete alpha's local checkout out from under the server, then POST /sync.
rm -rf "${WORKSPACE_DIR:?}/$ALPHA_ID"
[ ! -e "$WORKSPACE_DIR/$ALPHA_ID" ] || fail "could not remove local dir for sync test"
BEFORE=$(repo_last_indexed "$ALPHA_ID")
sleep 1
SYNC_CODE=$(curl -s -w "%{http_code}" -o /dev/null -X POST "$BASE_URL/api/repos/$ALPHA_ID/sync")
[ "$SYNC_CODE" = "202" ] || fail "sync returned $SYNC_CODE (expected 202)"
wait_reindexed "$ALPHA_ID" "$BEFORE" 90 || fail "sync did not fall back to fresh-clone (stuck/failed)"
[ -d "$WORKSPACE_DIR/$ALPHA_ID/.git" ] || fail "sync fallback did not re-create the local checkout"
echo -e "  ${GREEN}PASS${NC} — sync on missing dir fresh-cloned and re-indexed"

# ── Scenario 5: previously-indexed repo keeps its dir/index on a pull failure ──
echo -e "\n${CYAN}Scenario 5: transient pull failure keeps index + dir (indexed repo)${NC}"
BEFORE_COUNT=$(neo4j_entity_count "$ALPHA_ID")
# Make origin unreachable: move the bare repo aside. A Pull (fetch) now fails.
mv "$HEALTHY_BARE" "$HEALTHY_BARE.bak"
SYNC_CODE=$(curl -s -w "%{http_code}" -o /dev/null -X POST "$BASE_URL/api/repos/$ALPHA_ID/sync")
[ "$SYNC_CODE" = "202" ] || fail "sync returned $SYNC_CODE (expected 202)"
wait_status "$ALPHA_ID" "error" 60 || fail "pull against a missing origin should error"
[ -d "$WORKSPACE_DIR/$ALPHA_ID/.git" ] \
    || fail "a previously-indexed repo must KEEP its local dir on a transient failure"
echo -e "  ${GREEN}PASS${NC} — local dir preserved after transient pull failure"
if [ -n "$BEFORE_COUNT" ]; then
    AFTER_COUNT=$(neo4j_entity_count "$ALPHA_ID")
    if [ -n "$AFTER_COUNT" ] && [ "$AFTER_COUNT" -ge 1 ] && [ "$AFTER_COUNT" = "$BEFORE_COUNT" ]; then
        echo -e "  ${GREEN}PASS${NC} — index preserved in Neo4j ($AFTER_COUNT entities)"
    else
        fail "index not preserved (before=$BEFORE_COUNT after=$AFTER_COUNT)"
    fi
else
    echo -e "  ${YELLOW}SKIP${NC} — cypher-shell unavailable, cannot check index preservation"
fi
# Restore origin and confirm a subsequent sync recovers to indexed.
mv "$HEALTHY_BARE.bak" "$HEALTHY_BARE"
BEFORE=$(repo_last_indexed "$ALPHA_ID")
sleep 1
curl -s -o /dev/null -X POST "$BASE_URL/api/repos/$ALPHA_ID/sync"
wait_reindexed "$ALPHA_ID" "$BEFORE" 90 || fail "repo did not recover after origin was restored"
echo -e "  ${GREEN}PASS${NC} — recovered to 'indexed' after origin restored"

# ── Scenario 6: re-registering with an unreachable source keeps the index (issue #11) ──
echo -e "\n${CYAN}Scenario 6: re-register with an unreachable source keeps index + dir (issue #11)${NC}"
BEFORE_COUNT=$(neo4j_entity_count "$ALPHA_ID")
BEFORE=$(repo_last_indexed "$ALPHA_ID")
mv "$HEALTHY_BARE" "$HEALTHY_BARE.bak"

# 6a: a local path that no longer exists is rejected before anything is queued.
MISSING_CODE=$(curl -s -w "%{http_code}" -o /dev/null \
    -X POST "$BASE_URL/api/repos" \
    -H "Content-Type: application/json" \
    -d "{\"url\": \"$HEALTHY_BARE\", \"auth_type\": \"ssh\"}")
[ "$MISSING_CODE" = "400" ] || fail "re-register with a missing local path returned $MISSING_CODE (expected 400)"
[ "$(repo_status "$ALPHA_ID")" = "indexed" ] || fail "a rejected re-registration must leave the repo 'indexed'"
echo -e "  ${GREEN}PASS${NC} — missing local path rejected with 400, repo still 'indexed'"

# 6b: an unreachable remote with the same derived id is accepted, then fails
# before the worker wipes anything.
RID=$(register_repo "https://127.0.0.1:1/origins/alpha.git") || fail "re-register with an unreachable remote"
[ "$RID" = "$ALPHA_ID" ] || fail "unreachable remote derived a different id ($RID vs $ALPHA_ID)"
wait_status "$ALPHA_ID" "error" 60 || fail "re-register with an unreachable remote should error"
grep -qF "source https://127.0.0.1:1/origins/alpha.git is unreachable" "$SERVER_LOG" || fail "server log does not report the unreachable source"
[ -d "$WORKSPACE_DIR/$ALPHA_ID/.git" ] || fail "the local checkout was wiped for an unreachable source"
[ "$(repo_last_indexed "$ALPHA_ID")" = "$BEFORE" ] || fail "last_indexed changed for an unreachable source"
echo -e "  ${GREEN}PASS${NC} — status=error, local checkout and last_indexed preserved"
if [ -n "$BEFORE_COUNT" ]; then
    AFTER_COUNT=$(neo4j_entity_count "$ALPHA_ID")
    if [ -n "$AFTER_COUNT" ] && [ "$AFTER_COUNT" -ge 1 ] && [ "$AFTER_COUNT" = "$BEFORE_COUNT" ]; then
        echo -e "  ${GREEN}PASS${NC} — index preserved in Neo4j ($AFTER_COUNT entities)"
    else
        fail "index not preserved (before=$BEFORE_COUNT after=$AFTER_COUNT)"
    fi
else
    echo -e "  ${YELLOW}SKIP${NC} — cypher-shell unavailable, cannot check index preservation"
fi

# 6c: re-registering the restored source recovers it.
mv "$HEALTHY_BARE.bak" "$HEALTHY_BARE"
sleep 1
RID=$(register_repo "$HEALTHY_BARE") || fail "re-register after restoring origin"
wait_reindexed "$ALPHA_ID" "$BEFORE" 90 || fail "repo did not recover after re-registering the restored origin"
echo -e "  ${GREEN}PASS${NC} — recovered to 'indexed' after re-registering the restored origin"

# ── Scenario 7: a second instance on the same port leaves the registry alone (issue #10) ──
echo -e "\n${CYAN}Scenario 7: second knot-server on the same port exits before touching the registry (issue #10)${NC}"
# 'queued' with no job behind it is what the old startup recovery re-enqueued
# and claimed (-> cloning) before the second instance died on bind.
set_repo_status "$ALPHA_ID" "queued"
[ "$(repo_status "$ALPHA_ID")" = "queued" ] || fail "could not set '$ALPHA_ID' to queued"
run_server >"$SECOND_LOG" 2>&1 &
SECOND_PID=$!
for _ in $(seq 1 120); do
    kill -0 "$SECOND_PID" 2>/dev/null || break
    sleep 1
done
if kill -0 "$SECOND_PID" 2>/dev/null; then
    fail "the second instance is still running after 120s (it should fail to bind)"
fi
if wait "$SECOND_PID"; then
    fail "the second instance exited with status 0 (expected a bind failure)"
fi
SECOND_PID=""
grep -q "failed to bind" "$SECOND_LOG" || { cat "$SECOND_LOG"; fail "the second instance did not report the bind failure"; }
if grep -qE "Recovering stuck repo|Worker picked up|Registry loaded" "$SECOND_LOG"; then
    cat "$SECOND_LOG"
    fail "the second instance touched the registry before failing to bind"
fi
[ "$(repo_status "$ALPHA_ID")" = "queued" ] || fail "the second instance changed '$ALPHA_ID' (now '$(repo_status "$ALPHA_ID")')"
[ -d "$WORKSPACE_DIR/$ALPHA_ID/.git" ] || fail "the second instance wiped the local checkout of '$ALPHA_ID'"
echo -e "  ${GREEN}PASS${NC} — second instance failed to bind, registry and checkout untouched"
set_repo_status "$ALPHA_ID" "indexed"

# ── Scenario 8: a repo stuck in-progress with no lock holder heals itself (issue #10) ──
echo -e "\n${CYAN}Scenario 8: a repo stuck in 'cloning' with no worker is re-enqueued (issue #10)${NC}"
BEFORE=$(repo_last_indexed "$ALPHA_ID")
sleep 1  # last_indexed has 1s granularity
set_repo_status "$ALPHA_ID" "cloning"
[ "$(repo_status "$ALPHA_ID")" = "cloning" ] || fail "could not set '$ALPHA_ID' to cloning"
for i in $(seq 1 30); do
    grep -q "Recovering stuck repo '$ALPHA_ID' (was cloning): no worker holds its lock" "$SERVER_LOG" && break
    [ "$i" -eq 30 ] && fail "the stuck check did not recover '$ALPHA_ID' within 30s"
    sleep 1
done
echo -e "  ${GREEN}PASS${NC} — stuck check re-enqueued the repo"
wait_reindexed "$ALPHA_ID" "$BEFORE" 90 || fail "the recovered repo did not reach 'indexed'"
echo -e "  ${GREEN}PASS${NC} — recovered repo re-indexed"

# -------------------------------------------------------
# Step 4: done
# -------------------------------------------------------
echo -e "${YELLOW}[5/5] All recovery scenarios passed.${NC}"
echo -e "\n${GREEN}==================================================${NC}"
echo -e "${GREEN}Issue #7 reindex/recovery E2E: ALL PASSED${NC}"
echo -e "${GREEN}==================================================${NC}"
