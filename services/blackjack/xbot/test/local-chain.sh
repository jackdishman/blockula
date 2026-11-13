#!/usr/bin/env bash
# The bot's live test (test/live.ts) on a chain of its own: one validator summoned into a
# temporary directory with programs on, listening only on 127.0.0.1:PORT (HTTP) and
# 127.0.0.1:PORT+20 (p2p), both inside 38500–38599. Nothing here touches ~/.blockula, another
# node or any shared chain, and the node is stopped and its directory removed at the end.
#
#   services/blackjack/xbot/test/local-chain.sh           # start the chain, run the live test, stop it
#   HOLD=1 services/blackjack/xbot/test/local-chain.sh    # only start the chain and wait (Ctrl-C stops it)
#   BIN=… PORT=38540 BLOCK_MS=500 KEEP=1 …
#
# (`blockula dev --port N` also works for the test, but it picks its p2p port itself.)
set -euo pipefail
cd "$(dirname "$0")/../../../.."

BIN=${BIN:-node/target/release/blockula}
PORT=${PORT:-38500}
BLOCK_MS=${BLOCK_MS:-500}
P2P=$((PORT + 20))
[[ "$PORT" -ge 38500 && "$P2P" -le 38599 ]] || { echo "PORT must keep PORT and PORT+20 inside 38500–38599" >&2; exit 2; }
for port in "$PORT" "$P2P"; do
  if lsof -nP -iTCP:"$port" -sTCP:LISTEN > /dev/null 2>&1; then echo "port $port is in use: set PORT" >&2; exit 2; fi
done
DIR=$(mktemp -d "${TMPDIR:-/tmp}/blockula-xbot.XXXXXX")
export BLOCKULA_HOME="$DIR/cli"
mkdir -p "$BLOCKULA_HOME"
PID=""

cleanup() {
  local code=$?
  [[ -n "$PID" ]] && kill "$PID" 2>/dev/null || true
  wait 2>/dev/null || true
  if [[ -z "${KEEP:-}" ]]; then rm -rf "$DIR"; else echo "kept $DIR"; fi
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
fail() { echo "❌ FAIL: $*" >&2; tail -20 "$DIR/node.log" >&2 || true; exit 1; }
json() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

"$BIN" summon xbot-live --home "$DIR" --validators 1 --block-time-ms "$BLOCK_MS" --programs > "$DIR/summon.txt"
# A summoned chain publishes its validator at ports the OS picked. This one must listen inside
# its range, so the genesis gets fixed addresses before the first start, and the validator's
# proof of possession (which signs its addresses) is made again.
python3 -I - "$PORT" "$P2P" "$DIR/genesis.json" <<'PY'
import json, sys
http, p2p, path = "127.0.0.1:" + sys.argv[1], "127.0.0.1:" + sys.argv[2], sys.argv[3]
g = json.load(open(path))
for v in g["validators"]:
    v["http_addr"], v["p2p_addr"] = http, p2p
for o in g["operators"]:
    o["http_addr"], o["p2p_addr"] = http, p2p
json.dump(g, open(path, "w"), indent=2)
PY
mkdir -p "$DIR/scratch"
python3 -I -c "
import json, sys
g = json.load(open(sys.argv[1])); g['operators'] = []
json.dump(g, open(sys.argv[2], 'w'))" "$DIR/genesis.json" "$DIR/scratch/genesis.json"
cp "$DIR/validator.key" "$DIR/scratch/validator.key"
OWNER=$(json "d['validators'][0]['address']" < "$DIR/genesis.json")
"$BIN" validator pop --home "$DIR/scratch" --owner "$OWNER" > "$DIR/pop.json"
rm -rf "$DIR/scratch"
python3 -I - "$DIR/pop.json" "$DIR/genesis.json" <<'PY'
import json, sys
pop = json.load(open(sys.argv[1]))
g = json.load(open(sys.argv[2]))
for o in g["operators"]:
    if o["owner"] == pop["owner"]:
        o["pop"] = pop["pop"]
json.dump(g, open(sys.argv[2], "w"), indent=2)
PY
rm -f "$DIR/pop.json"

RPC="http://127.0.0.1:$PORT"
"$BIN" run --home "$DIR" >> "$DIR/node.log" 2>&1 &
PID=$!
height() { curl -fsS -m 5 "$RPC/api/status" 2>/dev/null | json "d['height']" 2>/dev/null || echo 0; }
# A fresh validator watches the network for about 15 seconds before it signs (slashing protection).
for _ in $(seq 1 240); do [[ "$(height)" -ge 3 ]] && break; sleep 0.5; done
[[ "$(height)" -ge 3 ]] || fail "the chain never reached height 3"
# Every port the node listens on is in the range.
while read -r port; do
  [[ "$port" -ge 38500 && "$port" -le 38599 ]] || fail "the node listens on port $port, outside 38500–38599"
done < <(lsof -nP -a -p "$PID" -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR > 1 { n = split($9, a, ":"); print a[n] }')
echo "✓ one validator at $RPC (p2p $P2P), blocks of $BLOCK_MS ms ($DIR)"

if [[ -n "${HOLD:-}" ]]; then
  echo "holding: BLOCKULA_RPC=$RPC (Ctrl-C stops the chain)"
  wait "$PID" || true
  exit 0
fi

BLOCKULA_RPC="$RPC" node --import tsx services/blackjack/xbot/test/live.ts || fail "the live test failed (its output is above)"
