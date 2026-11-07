#!/usr/bin/env bash
# A fresh named chain on a real node whose binary has the wallet built in, for e2e/served.spec.ts
# (playwright.config.ts starts it when E2E_SERVED=1). Build the binary first:
#   npm run build:wallet && cargo build --release --locked --manifest-path node/Cargo.toml
#   usage: e2e/served-node.sh <port>     BLOCKULA_BIN overrides the binary
set -euo pipefail
port=${1:?usage: e2e/served-node.sh <port>}
bin=${BLOCKULA_BIN:-$(cd "$(dirname "$0")/../.." && pwd)/node/target/release/blockula}
[ -x "$bin" ] || { echo "no blockula binary at $bin: build it with the wallet first (npm run build:wallet, then cargo build --release)" >&2; exit 1; }
home=$(mktemp -d "${TMPDIR:-/tmp}/blockula-served-e2e.XXXXXX")
export BLOCKULA_HOME=$home
"$bin" summon served-e2e --home "$home/chain" --name "Served Ledger" --token SRV --block-time-ms 1000 >/dev/null
exec "$bin" run --home "$home/chain" --port "$port"
