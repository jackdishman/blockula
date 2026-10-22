#!/usr/bin/env bash
# A fast join after a validator-set change whose handoff quorum needs a validator that joined
# after genesis (what local-testnet.sh's fast join hit on 2026-10-21). Three genesis validators,
# A, B and C; D registers and runs a node, E only registers (no node); both join by stake, then E
# unbonds and leaves. The change out of {A, B, C, D, E} needs 4 of its 5 handoff signatures: A, B
# and C hold theirs, and only D holds its own. Unless the genesis validators ask D (only the
# staking state says where it is), they never complete that proof, and a joiner fast-syncing from
# them can't verify the validator-set history: it falls back to syncing from genesis. Exits 0 when
# the joiner starts from a snapshot past the change, 1 when it doesn't.
#
#   scripts/test-handoff-exchange.sh                  # node/target/release/blockula
#   BIN=… PORT=28000 scripts/test-handoff-exchange.sh # ports PORT…PORT+4: the joiner, D, E (unused)
#   scripts/test-handoff-exchange.sh --self-test      # checks the verdict below, starts no node
#
# The genesis validators get free ports from `summon`, as in local-testnet.sh.
set -euo pipefail
cd "$(dirname "$0")/.."

# The verdict: snapshot height, change epoch, epoch blocks. A pass needs a snapshot in the epoch
# of the change or later: a joiner starting before it needs only the genesis signatures, so a
# snapshot from before the change would pass without proving anything.
verdict() {
  local from=$1 change=$2 epoch=$3
  if [[ ! "$from" =~ ^[0-9]+$ ]]; then
    echo "❌ FAIL: the joiner didn't start from a snapshot"
    return 1
  fi
  if (( from < change * epoch )); then
    echo "❌ FAIL: the joiner fast-synced from snapshot #$from, before the change at epoch $change (block $((change * epoch)))"
    return 1
  fi
  echo "✅ PASS: the joiner fast-synced from snapshot #$from (past the change at epoch $change, block $((change * epoch)))"
}

if [[ "${1:-}" == --self-test ]]; then
  expect() { # want (0 pass, 1 fail), from, change, epoch
    local got=0
    verdict "$2" "$3" "$4" >/dev/null || got=1
    if [[ "$got" != "$1" ]]; then echo "❌ verdict($2, $3, $4) = $got, want $1" >&2; exit 1; fi
  }
  expect 0 100 3 10 # after the change
  expect 0 30 3 10  # the change's first block
  expect 1 29 3 10  # the block before it
  expect 1 20 3 10  # an older snapshot
  expect 1 "" 3 10  # no snapshot
  expect 1 abc 3 10
  echo "✅ verdict self-test passed"
  exit 0
fi

BIN=${BIN:-node/target/release/blockula}
PORT=${PORT:-28000}
EPOCH=${EPOCH:-10}
DIR=${DIR:-$(mktemp -d "${TMPDIR:-/tmp}/blockula-handoffs.XXXXXX")}
# The CLI remembers nothing for nodes on this machine; keep it out of ~/.blockula regardless.
export BLOCKULA_HOME="$DIR/cli"
PIDS=()

cleanup() {
  if [[ -z "${KEEP:-}" ]]; then
    for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
  else
    echo "still running (KEEP=1). Data + logs: $DIR"
  fi
}
trap cleanup EXIT

for p in $(seq "$PORT" $((PORT + 4))); do
  if lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then echo "❌ port $p is taken" >&2; exit 2; fi
done
echo "📁 $DIR ($("$BIN" --version 2>/dev/null || echo "$BIN"))"

"$BIN" summon testnet --home "$DIR" --validators 3 --epoch-blocks "$EPOCH" --dev-durations >"$DIR/summon.txt"
GHASH=$(grep -o 'genesis hash: [0-9a-f]*' "$DIR/summon.txt" | awk '{print $3}')
addr() { node -e "console.log(require('$DIR/genesis.json').validators[$1].http_addr)"; }
status() { curl -sf -m 3 "http://$1/api/status"; }
field() { node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{try{console.log(JSON.parse(s)$1)}catch{console.log('')}})"; }
wait_height() { # host:port height
  for _ in $(seq 1 300); do
    h=$(status "$1" 2>/dev/null | field .height 2>/dev/null || true)
    [[ "${h:-0}" =~ ^[0-9]+$ && "${h:-0}" -ge "$2" ]] && return 0
    sleep 1
  done
  echo "❌ $1 never reached height $2" >&2; exit 1
}
active() { # address → true/false: in the set of the current epoch
  curl -sf "http://$V0/api/validators" | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>console.log(JSON.parse(s).active.some(v=>v.validator.address==='$1')))"
}
wait_for() { # what, seconds, command…
  local what=$1 secs=$2; shift 2
  for _ in $(seq 1 "$secs"); do "$@" && return 0; sleep 1; done
  echo "❌ timed out waiting for $what" >&2; exit 1
}

for i in 0 1 2; do
  "$BIN" run --home "$DIR/v$i" >>"$DIR/v$i.log" 2>&1 &
  PIDS+=($!)
done
V0=$(addr 0)
wait_height "$V0" 3
echo "✓ 3 genesis validators are finalizing blocks"

newcomer() { # name p2p-port http-port → its address (registered with the minimum self-bond)
  "$BIN" validator init --home "$DIR/$1" --genesis "http://$V0" --genesis-hash "$GHASH" --public-p2p-addr "127.0.0.1:$2" --public-http-addr "127.0.0.1:$3" >"$DIR/$1.init.txt"
  local a; a=$(grep -o 'boo1[0-9a-z]*' "$DIR/$1.init.txt" | head -1)
  curl -sf -X POST -H 'content-type: application/json' -d "{\"to\":\"$a\"}" "http://$V0/api/faucet" >/dev/null
  funded() { [[ "$(curl -sf "http://$V0/api/account/$a" | field .balance)" =~ ^[1-9] ]]; }
  wait_for "$1's funding" 60 funded
  "$BIN" validator register --home "$DIR/$1" --rpc "http://$V0" >"$DIR/$1.register.txt"
  echo "$a"
}
D=$(newcomer d $((PORT + 1)) $((PORT + 2)))
"$BIN" run --home "$DIR/d" >>"$DIR/d.log" 2>&1 &
PIDS+=($!)
E=$(newcomer e $((PORT + 3)) $((PORT + 4)))
echo "✓ D ($D) runs a node; E ($E) only registered"
both() { [[ "$(active "$D")" == true && "$(active "$E")" == true ]]; }
wait_for "D and E in the active set" $((EPOCH * 8 + 60)) both
echo "✓ the set is A, B, C, D, E (E has no node: A, B, C and D carry every quorum)"

"$BIN" stake undelegate "$E" --all --key "$DIR/e/validator.key" --rpc "http://$V0" >"$DIR/e.undelegate.txt"
gone() { [[ "$(active "$E")" == false ]]; }
wait_for "E to leave" $((EPOCH * 8 + 60)) gone
CHANGE=$(curl -sf "http://$V0/api/validators" | field .epoch)
echo "✓ E unbonded and left: the set is A, B, C, D from epoch $CHANGE (the change needs 4 of 5 handoff signatures)"

# A snapshot past the change, and time for a round of every node's handoff exchange (30 s).
snap() { [[ "$(curl -sf "http://$V0/api/snapshot/latest" | field .height)" -ge $((CHANGE * EPOCH)) ]] 2>/dev/null; }
wait_for "a snapshot past epoch $CHANGE's first block" 400 snap
sleep 35
for n in "$V0" "$(addr 1)" "$(addr 2)" "127.0.0.1:$((PORT + 2))"; do
  echo "  $n /api/epochs: proven through $(curl -sf "http://$n/api/epochs" | field .proven_through) of $(curl -sf "http://$n/api/epochs" | field .known_through)"
done

"$BIN" join --genesis "http://$V0" --genesis-hash "$GHASH" --allow-stale-checkpoint --home "$DIR/fast" --port "$PORT" >"$DIR/fast.log" 2>&1 &
PIDS+=($!)
from=""
for _ in $(seq 1 180); do
  from=$(status "127.0.0.1:$PORT" 2>/dev/null | field .fast_synced_from || true)
  [[ "$from" =~ ^[0-9]+$ ]] && break
  grep -q "no usable snapshot" "$DIR/fast.log" 2>/dev/null && break
  sleep 1
done
if verdict "$from" "$CHANGE" "$EPOCH"; then
  exit 0
fi
[[ "$from" =~ ^[0-9]+$ ]] || echo "   $(grep -o 'no usable snapshot.*' "$DIR/fast.log" || tail -3 "$DIR/fast.log")"
exit 1
