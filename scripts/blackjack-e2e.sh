#!/usr/bin/env bash
# Blackjack (docs/BLACKJACK.md) end to end on a local chain of real nodes: 4 validators summoned
# into a temporary directory, programs on, protocol v2 (what today's chains run: nothing here
# patches the genesis to a preview version), the table program created from a local owner account
# and the dealer daemon (services/blackjack/dealer) run as a real child process. Nothing here
# touches ~/.blockula, another node or any public network, and every port it listens on is
# 127.0.0.1:BASE_PORT … BASE_PORT+23 (38300–38323 by default).
#
# The driver is services/blackjack/dealer/test/soak.ts. It:
#   1. creates the table, funds the bankroll, sets limits for a fast test and approves a relayer;
#   2. starts the dealer daemon (its key, secret and store in the temporary directory);
#   3. plays HANDS hands from 5 accounts at once (3 signing their own bets and moves, 2 through
#      the relayer from their table balances) and verifies every settled hand from its `settled`
#      event down to the commitment in its `bet` event;
#   4. checks at every new block that the program's balance is bankroll + reserved + stakes + table
#      and that `reserved` and `stakes` are the open hands' sums;
#   5. kills the daemon with SIGKILL and restarts it twice (once with hands mid-play): no hand may
#      stall or be forfeited;
#   6. stops the daemon, bets, lets the reveal deadline pass and has another account call
#      `timeout`: the player is paid 3:2 from the hand's reserve; then an idle player is stood by
#      the daemon's own `timeout`;
#   7. checks the caps: min bet, max bet, the bankroll's share, the daily limit (and its reset),
#      the open hands limit, and a pause (bets refused, settlement, timeout and withdraw still work);
#   8. ends with no open hand, the whole bankroll back with the owner, and all four validators
#      agreeing on a recent block.
#
#   scripts/blackjack-e2e.sh                 # 1,000 hands, about 10 minutes
#   HANDS=60 scripts/blackjack-e2e.sh        # a quick run, about 3 minutes
#   TEST=live scripts/blackjack-e2e.sh       # the dealer's live test (test/live.ts) on the same chain
#   KEEP=1 scripts/blackjack-e2e.sh          # keep the directory (it's printed) and every log
#   HOLD=1 scripts/blackjack-e2e.sh          # only start the chain and wait (Ctrl-C stops it)
#   BIN=… BLOCK_MS=400 BASE_PORT=38300 VALIDATORS=4 scripts/blackjack-e2e.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-node/target/release/blockula}
HANDS=${HANDS:-1000}
BLOCK_MS=${BLOCK_MS:-400}
BASE_PORT=${BASE_PORT:-38300}
VALIDATORS=${VALIDATORS:-4}
TEST=${TEST:-soak}
DIR=${DIR:-$(mktemp -d "${TMPDIR:-/tmp}/blockula-blackjack.XXXXXX")}
DEALER=services/blackjack/dealer
export BLOCKULA_HOME="$DIR/cli"
mkdir -p "$BLOCKULA_HOME"
PIDS=()
LAST=$((VALIDATORS - 1))

# HTTP on BASE_PORT + i, p2p on BASE_PORT + 20 + i: all inside 38200–38399.
[[ "$VALIDATORS" -ge 1 && "$VALIDATORS" -le 4 ]] || { echo "VALIDATORS is 1 to 4" >&2; exit 2; }
[[ "$BASE_PORT" -ge 38200 && $((BASE_PORT + 23)) -le 38399 ]] || { echo "BASE_PORT must keep BASE_PORT … BASE_PORT+23 inside 38200–38399" >&2; exit 2; }

cleanup() {
  local code=$?
  for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
  # The dealer daemon is the driver's child; if the driver died without stopping it, its pid is here.
  if [[ -f "$DIR/dealer/daemon.pid" ]]; then kill -9 "$(cat "$DIR/dealer/daemon.pid")" 2>/dev/null || true; fi
  wait 2>/dev/null || true
  if [[ -z "${KEEP:-}" ]]; then rm -rf "$DIR"; else echo "kept $DIR"; fi
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
tails() {
  for i in $(seq 0 "$LAST"); do echo "--- v$i log (tail) ---" >&2; tail -5 "$DIR/v$i.log" >&2 || true; done
  for f in "$DIR"/dealer/daemon.log "$DIR"/driver.log; do [[ -f "$f" ]] && { echo "--- $(basename "$f") (tail) ---" >&2; tail -25 "$f" >&2; }; done
  return 0
}
fail() { echo "❌ FAIL: $*" >&2; tails; exit 1; }
json() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

for i in $(seq 0 "$LAST"); do
  for port in $((BASE_PORT + i)) $((BASE_PORT + 20 + i)); do
    if lsof -nP -iTCP:"$port" -sTCP:LISTEN > /dev/null 2>&1; then echo "port $port is in use: set BASE_PORT" >&2; exit 2; fi
  done
done

# ------------------------------------------------------------------------------- the chain
"$BIN" summon blackjack-e2e --home "$DIR" --validators "$VALIDATORS" --block-time-ms "$BLOCK_MS" --programs > "$DIR/summon.txt"
# A summoned local chain publishes its validators at ports the OS picked. This one must listen
# inside its own range, so every home's genesis gets fixed addresses before the chain's first
# start (a genesis can't change afterwards). A validator's proof of possession signs its
# addresses, so each is made again with `blockula validator pop`, from a scratch home whose
# genesis has the new addresses and no operators yet.
[[ "$VALIDATORS" -eq 1 ]] && HOMES=("$DIR") || HOMES=($(for i in $(seq 0 "$LAST"); do echo "$DIR/v$i"; done))
GENESES=("$DIR/genesis.json"); [[ "$VALIDATORS" -eq 1 ]] || for h in "${HOMES[@]}"; do GENESES+=("$h/genesis.json"); done
python3 -I - "$BASE_PORT" "${GENESES[@]}" <<'PY'
import json, sys
base = int(sys.argv[1])
for path in sys.argv[2:]:
    g = json.load(open(path))
    for i, v in enumerate(g["validators"]):
        v["http_addr"] = "127.0.0.1:%d" % (base + i)
        v["p2p_addr"] = "127.0.0.1:%d" % (base + 20 + i)
        for o in g["operators"]:
            if o["consensus_key"] == v["public_key"]:
                o["http_addr"], o["p2p_addr"] = v["http_addr"], v["p2p_addr"]
    json.dump(g, open(path, "w"), indent=2)
PY
mkdir -p "$DIR/scratch"
python3 -I -c "
import json, sys
g = json.load(open(sys.argv[1])); g['operators'] = []
json.dump(g, open(sys.argv[2], 'w'))" "$DIR/genesis.json" "$DIR/scratch/genesis.json"
for i in $(seq 0 "$LAST"); do
  cp "${HOMES[$i]}/validator.key" "$DIR/scratch/validator.key"
  OWNER=$(json "d['validators'][$i]['address']" < "$DIR/genesis.json")
  "$BIN" validator pop --home "$DIR/scratch" --owner "$OWNER" > "$DIR/pop$i.json"
done
rm -rf "$DIR/scratch"
python3 -I - "$DIR" "$VALIDATORS" "${GENESES[@]}" <<'PY'
import json, sys
d, n = sys.argv[1], int(sys.argv[2])
pops = {}
for i in range(n):
    p = json.load(open("%s/pop%d.json" % (d, i)))
    pops[p["owner"]] = p["pop"]
for path in sys.argv[3:]:
    g = json.load(open(path))
    for o in g["operators"]:
        o["pop"] = pops[o["owner"]]
    json.dump(g, open(path, "w"), indent=2)
PY
rm -f "$DIR"/pop*.json

http() { echo "127.0.0.1:$((BASE_PORT + $1))"; }
RPC="http://$(http 0)"
RPCS=$(for i in $(seq 0 "$LAST"); do printf 'http://%s,' "$(http "$i")"; done); RPCS=${RPCS%,}
api() { curl -fsS -m 5 "$RPC/api$1"; }
for i in $(seq 0 "$LAST"); do "$BIN" run --home "${HOMES[$i]}" >> "$DIR/v$i.log" 2>&1 & PIDS[$i]=$!; done
height() { api /status 2>/dev/null | json "d['height']" 2>/dev/null || echo 0; }
# A fresh validator watches the network for about 15 seconds before it signs (slashing protection).
for _ in $(seq 1 240); do [[ "$(height)" -ge 3 ]] && break; sleep 0.5; done
[[ "$(height)" -ge 3 ]] || fail "the chain never reached height 3 (it is at $(height))"
[[ "$(api /upgrade | json "d['protocol_version']")" == 2 ]] || fail "the chain isn't running protocol v2"
# Every port these nodes listen on is in the range.
for p in "${PIDS[@]}"; do
  while read -r port; do
    [[ "$port" -ge "$BASE_PORT" && "$port" -le $((BASE_PORT + 23)) ]] || fail "a validator listens on port $port, outside ${BASE_PORT}–$((BASE_PORT + 23))"
  done < <(lsof -nP -a -p "$p" -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR > 1 { n = split($9, a, ":"); print a[n] }')
done
echo "✓ $VALIDATORS validators on protocol v2 at $RPC, blocks of $BLOCK_MS ms, listening only on ${BASE_PORT}–$((BASE_PORT + 23)) ($DIR)"

if [[ -n "${HOLD:-}" ]]; then
  echo "holding: BLOCKULA_RPC=$RPC BJ_RPCS=$RPCS (Ctrl-C stops the chain)"
  wait "${PIDS[0]}" || true
  exit 0
fi

# ------------------------------------------------------------------------------- the table
export BLOCKULA_RPC="$RPC" BJ_RPCS="$RPCS" BJ_DIR="$DIR" HANDS BLOCK_MS
STARTED=$(date +%s)
case "$TEST" in
  soak) node --import tsx "$DEALER/test/soak.ts" 2>&1 | tee "$DIR/driver.log" || fail "the soak failed (its output is above)" ;;
  live) node --import tsx "$DEALER/test/live.ts" 2>&1 | tee "$DIR/driver.log" || fail "the live test failed (its output is above)" ;;
  *) fail "TEST is soak or live" ;;
esac

# Every validator agrees on a recent block, from this side too.
H=$(( $(height) - 2 ))
HASHES=$(for i in $(seq 0 "$LAST"); do curl -fsS -m 5 "http://$(http "$i")/api/block/$H" | json "d['hash']"; done | sort -u | wc -l)
[[ "$HASHES" -eq 1 ]] || fail "the validators disagree about block $H"
echo "✅ blackjack e2e passed ($TEST): $VALIDATORS validators agree at block $H, $(( $(date +%s) - STARTED )) s after the chain was up"
