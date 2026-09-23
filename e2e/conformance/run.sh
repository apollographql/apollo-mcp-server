#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
npm ci --ignore-scripts --no-audit --no-fund
npm test -- "$@"
