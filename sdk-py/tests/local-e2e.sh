#!/usr/bin/env bash
# A disposable loopback dev chain; no real keys or user's ~/.blockula directory.
set -euo pipefail
cd "$(dirname "$0")/../.."
BIN=${BIN:-node/target/release/blockula}
PYTHON=${PYTHON:-python3}
PORT=${PORT:-7273}
SDK_E2E_HOME=$(mktemp -d "${TMPDIR:-/tmp}/blockula-python.XXXXXX")
BLOCKULA_HOME="$SDK_E2E_HOME" "$BIN" dev --port "$PORT" >"$SDK_E2E_HOME/node.log" 2>&1 &
SDK_E2E_PID=$!
cleanup() {
  kill "$SDK_E2E_PID" 2>/dev/null || true
  wait "$SDK_E2E_PID" 2>/dev/null || true
}
trap cleanup EXIT
ready=0
for _ in $(seq 1 120); do
  if ! kill -0 "$SDK_E2E_PID" 2>/dev/null; then
    cat "$SDK_E2E_HOME/node.log" >&2
    exit 1
  fi
  if curl -fsS --max-time 1 "http://127.0.0.1:$PORT/api/health" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.25
done
if [ "$ready" != 1 ]; then
  cat "$SDK_E2E_HOME/node.log" >&2
  echo 'local node did not become healthy' >&2
  exit 1
fi
BLOCKULA_RPC="http://127.0.0.1:$PORT" "$PYTHON" sdk-py/tests/e2e.py
