#!/usr/bin/env bash
# Local multi-node smoke test: 4 genesis validators (simplex BFT over authenticated p2p) + 1 full
# node that joins over HTTP + a 5th validator onboarded with the CLI (init → faucet → register with
# a self-bond → selected by stake at the next epoch boundary, no admin step). Runs the SDK e2e
# against a validator (with validator
# 0 as the Season 0 admin, so the contribution pool's admin flow runs too) and against the full
# node, restarts a validator mid-run, then checks every node agrees on hashes and state roots.
# The chain has `blockula dev`'s short durations (`summon --dev-durations`), so the e2e's staking
# runs through unbonding and withdrawal, and a ParamChange proposal passes the bootstrap council
# (3 of the 4 validators) and executes after its timelock.
#
#   scripts/local-testnet.sh            # build release, run, tear down
#   KEEP=1 scripts/local-testnet.sh     # leave the testnet running afterwards
#   BIN=node/target/debug/blockula scripts/local-testnet.sh
set -euo pipefail
cd "$(dirname "$0")/.."

N=${N:-4}
DIR=${DIR:-$(mktemp -d "${TMPDIR:-/tmp}/blockula-testnet.XXXXXX")}
BIN=${BIN:-node/target/release/blockula}
FULL_PORT=${FULL_PORT:-7171}
PIDS=()

cleanup() {
  if [[ -z "${KEEP:-}" ]]; then
    for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
  else
    echo "🧛 still running (KEEP=1). Data + logs: $DIR"
  fi
}
trap cleanup EXIT

[[ -n "${SKIP_BUILD:-}" ]] || cargo build --release --locked --manifest-path node/Cargo.toml
echo "📁 $DIR"

EPOCH=${EPOCH:-20}
"$BIN" summon testnet --home "$DIR" --validators "$N" --epoch-blocks "$EPOCH" --dev-durations | tee "$DIR/summon.txt"
GHASH=$(grep -o 'genesis hash: [0-9a-f]*' "$DIR/summon.txt" | awk '{print $3}')
addr() { node -e "console.log(require('$DIR/genesis.json').validators[$1].http_addr)"; }
status() { curl -sf -m 3 "http://$1/api/status"; }
field() { node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>console.log(JSON.parse(s)$1))"; }

start_validator() {
  "$BIN" run --home "$DIR/v$1" >>"$DIR/v$1.log" 2>&1 &
  PIDS[$1]=$!
}
for i in $(seq 0 $((N - 1))); do start_validator "$i"; done

wait_height() { # url height
  for _ in $(seq 1 120); do
    h=$(status "$1" 2>/dev/null | field .height 2>/dev/null || true); h=${h:-0}
    [[ "$h" -ge "$2" ]] && return 0
    sleep 1
  done
  echo "❌ $1 never reached height $2" >&2; exit 1
}
V0=$(addr 0); V2=$(addr 2)
wait_height "$V0" 3
echo "✓ validators are finalizing blocks"

"$BIN" join --genesis "http://$V0" --genesis-hash "$GHASH" --home "$DIR/full" --port "$FULL_PORT" >"$DIR/full.log" 2>&1 &
PIDS[$N]=$!
FULL="127.0.0.1:$FULL_PORT"

echo "— SDK e2e against validator 2 (faucet + txs gossiped to whoever leads)"
BLOCKULA_RPC="http://$V2" BLOCKULA_ADMIN_SEED="$(cat "$DIR/v0/validator.key")" npm test -w @blockula/sdk

echo "— governance: a ParamChange the bootstrap council passes (⅔ of the validators vote aye)"
proposals() { curl -sf "http://$V0/api/proposals?limit=1"; }
# (A key the council may change: the validators' own economics, e.g. jail_blocks, are the token
# holders' alone, `params::COUNCIL_KEYS`.)
before=$(curl -sf "http://$V0/api/params" | field '.params.multisig_expiry_blocks')
want=$((before + 7))
"$BIN" gov propose param multisig_expiry_blocks "$want" --title "local testnet: multisig_expiry_blocks $want" --key "$DIR/v0/validator.key" --rpc "http://$V0"
PID=$(proposals | field '.proposals[0].id')
# A vote binds what `gov show` printed (its digest): the CLI refuses a proposal that reads
# differently by the time it signs (audit gap-7-6).
DIGEST=$("$BIN" gov show "$PID" --rpc "http://$V0" | sed -n 's/^   digest: \([0-9a-f]\{64\}\).*/\1/p')
[[ ${#DIGEST} == 64 ]] || { echo "❌ gov show printed no digest for proposal #$PID" >&2; exit 1; }
for i in 0 1 2; do "$BIN" gov vote "$PID" aye 1 --conviction 0 --digest "$DIGEST" --key "$DIR/v$i/validator.key" --rpc "http://$V0"; done
status=""
for _ in $(seq 1 240); do
  status=$(curl -sf "http://$V0/api/proposal/$PID" | field '.status')
  [[ "$status" == "executed" || "$status" == "rejected" || "$status" == "failed" ]] && break; sleep 1
done
now=$(curl -sf "http://$V0/api/params" | field '.params.multisig_expiry_blocks')
[[ "$status" == "executed" && "$now" == "$want" ]] || { echo "❌ proposal #$PID: $status, multisig_expiry_blocks $now (wanted $want)" >&2; curl -sf "http://$V0/api/proposal/$PID" >&2; exit 1; }
echo "✓ proposal #$PID passed by the council and ran: multisig_expiry_blocks $before → $now"

echo "— onboarding a validator with the CLI"
"$BIN" validator init --home "$DIR/v$N" --genesis "http://$V0" --genesis-hash "$GHASH" | tee "$DIR/init.txt"
NEWV=$(grep -o 'boo1[0-9a-z]*' "$DIR/init.txt" | head -1)
curl -sf -X POST -H 'content-type: application/json' -d "{\"to\":\"$NEWV\"}" "http://$V0/api/faucet" >/dev/null
for _ in $(seq 1 60); do
  bal=$(curl -sf "http://$V0/api/account/$NEWV" | field .balance 2>/dev/null || echo 0)
  [[ "$bal" != "0" ]] && break; sleep 1
done
"$BIN" validator register --home "$DIR/v$N" --rpc "http://$V2"
"$BIN" run --home "$DIR/v$N" >>"$DIR/v$N.log" 2>&1 &
PIDS[$((N + 1))]=$!
active=""
for _ in $(seq 1 $((EPOCH * 3 + 30))); do
  active=$(curl -sf "http://$V0/api/validators" | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>console.log(JSON.parse(s).active.some(v=>v.validator.address==='$NEWV')))")
  [[ "$active" == "true" ]] && break; sleep 1
done
[[ "$active" == "true" ]] || { echo "❌ new validator never became active" >&2; exit 1; }
NEWHTTP=$(node -e "console.log(require('$DIR/v$N/node.json').http_addr)")
wait_height "$NEWHTTP" "$(status "$V0" | field .height)"
proposed=0
for _ in $(seq 1 60); do
  proposed=$(curl -sf "http://$V0/api/blocks?limit=100" | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>console.log(JSON.parse(s).filter(b=>b.proposer.address==='$NEWV').length))")
  [[ "$proposed" -gt 0 ]] && break; sleep 1
done
[[ "$proposed" -gt 0 ]] || { echo "❌ new validator never proposed a block" >&2; exit 1; }
echo "✓ validator $NEWV joined by stake at an epoch boundary and is proposing blocks"

echo "— restarting validator 1"
kill "${PIDS[1]}"; wait "${PIDS[1]}" 2>/dev/null || true
sleep 5
start_validator 1

wait_height "$FULL" "$(status "$V0" | field .height)"
echo "— SDK e2e against the full node (txs + faucet forwarded to validators)"
BLOCKULA_RPC="http://$FULL" npm test -w @blockula/sdk

# Agreement: same hash and state root at a common height on every node.
NODES=()
for i in $(seq 0 $((N - 1))); do NODES+=("$(addr "$i")"); done
NODES+=("$NEWHTTP" "$FULL")
target=$(status "$V0" | field .height)
for n in "${NODES[@]}"; do wait_height "$n" "$target"; done
ref=""
for n in "${NODES[@]}"; do
  line=$(curl -sf "http://$n/api/block/$target" | field '.hash+" "+JSON.parse(s).state_root')
  echo "  $n  #$target  $line"
  [[ -z "$ref" ]] && ref=$line
  [[ "$line" == "$ref" ]] || { echo "❌ nodes disagree at height $target" >&2; exit 1; }
done
echo "✓ all $((N + 2)) nodes agree on block #$target (hash + state root)"

echo "— CLI wallet"
W="$DIR/wallet-home"
BLOCKULA_HOME="$W" "$BIN" wallet new
BLOCKULA_HOME="$W" "$BIN" wallet faucet --rpc "http://$FULL"
for _ in $(seq 1 30); do
  out=$(BLOCKULA_HOME="$W" "$BIN" wallet balance --rpc "http://$FULL")
  [[ "$out" == *"100 FANG"* ]] && break; sleep 1
done
echo "  $out"
BLOCKULA_HOME="$W" "$BIN" wallet send "$NEWV" 1.5 --memo "cli wallet" --rpc "http://$V0"

echo "— CLI hunts + contribution pool"
BLOCKULA_HOME="$W" "$BIN" hunt create jackdishman/blockula 1 10 --title "CLI hunt" --days 1 --rpc "http://$V0"
WADDR=$(node -e "console.log(require('$W/wallet.json').address)")
HID=$(curl -sf "http://$V0/api/account/$WADDR" | field '.hunts_created.at(-1)')
BLOCKULA_HOME="$W" "$BIN" hunt award "$HID" --pr 1 --to "$NEWV" --skip-github --yes --rpc "http://$V0"
BLOCKULA_HOME="$W" "$BIN" hunt show "$HID" --rpc "http://$FULL"
BLOCKULA_HOME="$W" "$BIN" contrib fund 1 --rpc "http://$V0"
"$BIN" contrib status --rpc "http://$FULL"

echo "— fast join from a certified snapshot"
wait_height "$V0" 106
FAST="127.0.0.1:$((FULL_PORT + 1))"
# Dev durations shrink the weak-subjectivity period (half the evidence window, at most the
# unbonding period) to a minute of blocks, so trusting only the genesis is "stale" by now: this
# smoke test accepts that risk explicitly instead of fetching a checkpoint (OPERATIONS.md).
"$BIN" join --genesis "http://$V0" --genesis-hash "$GHASH" --allow-stale-checkpoint --home "$DIR/fast" --port "$((FULL_PORT + 1))" >"$DIR/fast.log" 2>&1 &
PIDS[$((N + 2))]=$!
wait_height "$FAST" "$(status "$V0" | field .height)"
from=$(status "$FAST" | field .fast_synced_from)
[[ "$from" =~ ^[0-9]+$ && "$from" -ge 100 ]] || { echo "❌ fast join didn't start from a snapshot (fast_synced_from=$from)" >&2; cat "$DIR/fast.log" >&2; exit 1; }
target=$(status "$V0" | field .height)
wait_height "$FAST" "$target"
a=$(curl -sf "http://$V0/api/block/$target" | field '.hash+" "+JSON.parse(s).state_root')
b=$(curl -sf "http://$FAST/api/block/$target" | field '.hash+" "+JSON.parse(s).state_root')
[[ "$a" == "$b" ]] || { echo "❌ fast-synced node disagrees at #$target" >&2; exit 1; }
echo "✓ fast-synced from snapshot #$from and agrees with validators at #$target"
NODES+=("$FAST")

echo "— health and memory"
for n in "${NODES[@]}"; do
  code=$(curl -s -o /dev/null -w '%{http_code}' "http://$n/api/health")
  echo "  $n /api/health → $code"
done
for p in "${PIDS[@]}"; do
  [[ -n "$p" ]] && ps -o pid=,rss=,command= -p "$p" 2>/dev/null | awk '{printf "  pid %s  RSS %.0f MB  %s %s %s\n", $1, $2/1024, $3, $4, $5}'
done
