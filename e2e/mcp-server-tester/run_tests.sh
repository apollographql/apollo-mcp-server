#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: run-tools.sh <test-dir>

Runs:
  npx mcp-server-tester tools <test-dir>/tool-tests.yaml --server-config <test-dir>/apollo-mcp-server-config.json

Notes:
  - <test-dir> is resolved relative to this script's directory (not the caller's cwd),
    so calling: foo/bar/run-tools.sh local-directory
    uses:       foo/bar/local-directory/tool-tests.yaml
  - If ../../target/release/apollo-mcp-server (relative to this script) doesn't exist,
    it is built from the repo root (../../) with: cargo build --release
USAGE
  exit 1
}

[[ "${1:-}" == "-h" || "${1:-}" == "--help" || $# -eq 0 ]] && usage

RAW_DIR_ARG="${1%/}"
SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

# If absolute path, use it as-is; otherwise, resolve relative to the script dir.
if [[ "$RAW_DIR_ARG" = /* ]]; then
  TEST_DIR="$RAW_DIR_ARG"
else
  TEST_DIR="$(cd -P -- "$SCRIPT_DIR/$RAW_DIR_ARG" && pwd)"
fi

TEST_DIR="${1%/}"  # strip trailing slash if present
TESTS="$TEST_DIR/tool-tests.yaml"
MCP_CONFIG="$TEST_DIR/config.yaml"

# Sanity checks
[[ -f "$TESTS" ]]  || { echo "✗ Missing file: $TESTS";  exit 2; }
[[ -f "$MCP_CONFIG" ]] || { echo "✗ Missing file: $MCP_CONFIG"; exit 2; }

REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BIN_PATH="$REPO_ROOT/target/release/apollo-mcp-server"

if [[ ! -x "$BIN_PATH" ]]; then
  echo "ℹ️  Binary not found at: $BIN_PATH"
  echo "➡️  Building release binary from: $REPO_ROOT"
  (cd "$REPO_ROOT" && cargo build --release)

  # Re-check after build
  if [[ ! -x "$BIN_PATH" ]]; then
    echo "✗ Build succeeded but binary not found/executable at: $BIN_PATH"
    exit 3
  fi
fi

# Template → generated server-config
TEMPLATE_PATH="${SERVER_CONFIG_TEMPLATE:-"$SCRIPT_DIR/server-config.template.json"}"
[[ -f "$TEMPLATE_PATH" ]] || { echo "✗ Missing server-config template: $TEMPLATE_PATH"; exit 4; }

TMP_DIR="$(mktemp -d)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT INT TERM # cleanup before exiting
GEN_CONFIG="$TMP_DIR/server-config.generated.json"

# Safe replacement for <test-dir> with absolute path (handles /, &, and |)
safe_dir="${TEST_DIR//\\/\\\\}"
safe_dir="${safe_dir//&/\\&}"
safe_dir="${safe_dir//|/\\|}"

# Replace the literal token "<test-dir>" everywhere
sed "s|<test-dir>|$safe_dir|g" "$TEMPLATE_PATH" > "$GEN_CONFIG"

# Run the command
exec npx -y mcp-server-tester tools "$TESTS" --server-config "$GEN_CONFIG"