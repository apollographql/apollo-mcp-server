# AIR-399 offline serve-smoke profile (for the harness smoke.sh runner).
#
# Boots apollo-mcp-server with the search tool over the offline catalog fixture, replays
# every query in the checked-in search-quality baseline through the real MCP streamable
# HTTP endpoint, and asserts the top-k results reproduce the fixture — proving the
# baseline was captured against a real running search, is deterministic, and is not
# hand-authored.
#
# Run:
#   bash /work/harness/smoke.sh /work/AIR-399/apollo-mcp-server/smoke.d/AIR-399.sh
# (or `bash /work/harness/smoke.sh AIR-399` when this profile is available in the
# harness smoke.d/ directory.)

air399_repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
air399_port="${AIR399_SMOKE_PORT:-4599}"
air399_fixtures="$air399_repo/crates/apollo-mcp-server/src/introspection/tools/testdata/search_baseline"

smoke_build() {
  # debug=false + no incremental keeps the target dir small enough for constrained containers
  (cd "$air399_repo" && cargo build --config profile.dev.debug=false --config build.incremental=false -j 2 -p apollo-mcp-server --bin apollo-mcp-server)
}

smoke_start() {
  cat > "$WORK/air399-config.yaml" <<EOF
transport:
  type: streamable_http
  address: 127.0.0.1
  port: $air399_port
schema:
  source: local
  path: $air399_fixtures/catalog.graphql
operations:
  source: local
  paths: []
introspection:
  search:
    enabled: true
overrides:
  mutation_mode: all
health_check:
  enabled: true
EOF
  smoke_bg "$air399_repo/target/debug/apollo-mcp-server" "$WORK/air399-config.yaml"
}

smoke_ready() {
  http_ok "http://127.0.0.1:$air399_port/health"
}

smoke_check() {
  local out status qid
  out="$(python3 "$air399_repo/smoke.d/check_search_baseline.py" \
    --endpoint "http://127.0.0.1:$air399_port/mcp" \
    --baseline "$air399_fixtures/baseline.json" 2>&1)"
  status=$?
  printf '%s\n' "$out" | sed 's/^/    /'

  # One assertion per baseline query, so a regression names the exact query that broke.
  while IFS= read -r qid; do
    expect "search baseline reproduces: $qid" "$out" "PASS $qid"
  done < <(python3 -c "import json
for q in json.load(open('$air399_fixtures/baseline.json'))['queries']:
    print(q['id'])")

  expect "baseline checker verdict" "$out" "RESULT: PASS"
  if [ "$status" -eq 0 ]; then ok "baseline checker exited 0"; else bad "baseline checker exited $status"; fi
}
