#!/usr/bin/env bash
set -euo pipefail

BLUE='\033[0;34m'
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}knot-server E2E Test Suite${NC}"
echo -e "${BLUE}========================================${NC}"

FAILED_TESTS=()
PASSED_TESTS=()

run_test() {
    local test_name="$1"
    local test_script="$2"
    echo -e "\n${YELLOW}[Running: $test_name]${NC}"
    if "$PROJECT_ROOT/tests/$test_script"; then
        echo -e "${GREEN}$test_name PASSED${NC}"
        PASSED_TESTS+=("$test_name")
    else
        echo -e "${RED}$test_name FAILED${NC}"
        FAILED_TESTS+=("$test_name")
    fi
    sleep 3
}

# Run suites
run_test "Full E2E: Lifecycle + Errors" "run_e2e.sh"
run_test "Cluster Coordination: Stale Lock Recovery" "run_cluster_e2e.sh"

# Summary
echo -e "\n${BLUE}========================================${NC}"
echo -e "${BLUE}E2E Summary${NC}"
echo -e "${BLUE}========================================${NC}"
for t in "${PASSED_TESTS[@]}"; do echo -e "  ${GREEN}$t${NC}"; done
if [ ${#FAILED_TESTS[@]} -gt 0 ]; then
    for t in "${FAILED_TESTS[@]}"; do echo -e "  ${RED}$t${NC}"; done
    exit 1
else
    echo -e "\n${GREEN}All E2E tests passed!${NC}"
fi
