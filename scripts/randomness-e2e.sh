#!/usr/bin/env bash
# Randomness for programs (docs/RANDOMNESS.md) on a local chain of real nodes: 4 validators summoned
# into a temporary directory on free local ports, running protocol v3 (the preview version) from
# block 1 with programs on, and the `dice` guest driven with `blockula program …` as a person
# would. Nothing here touches ~/.blockula, another node or any public network.
#
#   1. A roll can't settle in its own epoch; two epochs later it settles on a beacon value all
#      four validators revealed for. Another wallet sends the settle and is paid the bounty (a
#      hundredth of the stake), out of the stake on a miss and from the bank on a hit.
#   2. Validator 2 loses its beacon secrets after committing (stopped, the file emptied,
#      restarted), so it can't reveal: the value is derived from the other three, it is jailed, and
#      the roll that named that epoch settles all the same.
#   3. Two rolls nobody settles. Past the deadline (32 epochs after the target) the first still
#      settles on its value, which the chain keeps for 64 epochs: waiting gave its player nothing.
#      The second is left until the value is gone: its stake is forfeit to the bank, never
#      refunded, less the bounty for whoever closes it.
#
# A value with no reveals at all (the hash-chain fallback) needs every validator to sit a round
# out, which a running node doesn't do: the simulator covers it
# (`sim::programs::the_dice_settles_identically_on_four_validators`).
#
#   scripts/randomness-e2e.sh                    # node/target/release/blockula, about 13 minutes
#   BIN=… EPOCH=20 BLOCK_MS=500 scripts/randomness-e2e.sh
#   KEEP=1 scripts/randomness-e2e.sh             # keep the directory (it's printed) and the nodes' logs
#   SKIP_EXPIRY=1 scripts/randomness-e2e.sh      # without step 3 (about a minute)
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-node/target/release/blockula}
EPOCH=${EPOCH:-20}
BLOCK_MS=${BLOCK_MS:-500}
DIR=${DIR:-$(mktemp -d "${TMPDIR:-/tmp}/blockula-randomness.XXXXXX")}
GUESTS=node/vm/tests/programs
export BLOCKULA_HOME="$DIR/cli"
mkdir -p "$BLOCKULA_HOME" "$DIR/out"
PIDS=()
N=0

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  if [[ -z "${KEEP:-}" ]]; then rm -rf "$DIR"; else echo "kept $DIR"; fi
}
trap cleanup EXIT
fail() { echo "❌ FAIL: $*" >&2; for i in 0 1 2 3; do echo "--- v$i log (tail) ---" >&2; tail -5 "$DIR/v$i.log" >&2 || true; done; exit 1; }
json() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

# ------------------------------------------------------------------------------- the chain
"$BIN" summon randomness-e2e --home "$DIR" --validators 4 --block-time-ms "$BLOCK_MS" --epoch-blocks "$EPOCH" --programs > "$DIR/summon.txt"
# The preview version from block 1, in every home, before the chain's first start (a genesis can't
# change afterwards): what a dev or test chain needs for `beacon_v1`, `epoch_v1` and `blake3_v1`.
for g in "$DIR/genesis.json" "$DIR"/v*/genesis.json; do
  python3 -I - "$g" <<'PY'
import json, sys
path = sys.argv[1]
g = json.load(open(path))
g["upgrades"] = [{"version": 3, "activation_height": 1}]
json.dump(g, open(path, "w"), indent=2)
PY
done
http() { json "d['validators'][$1]['http_addr']" < "$DIR/genesis.json"; }
RPC="http://$(http 0)"
api() { curl -fsS -m 5 "$RPC/api$1"; }
start_validator() { "$BIN" run --home "$DIR/v$1" >> "$DIR/v$1.log" 2>&1 & PIDS[$1]=$!; }
for i in 0 1 2 3; do start_validator "$i"; done
height() { api /status 2>/dev/null | json "d['height']" 2>/dev/null || echo 0; }
wait_height() { # height
  for _ in $(seq 1 1200); do [[ "$(height)" -ge "$1" ]] && return 0; sleep 0.5; done
  fail "the chain never reached height $1 (it is at $(height))"
}
epoch_now() { echo $(( $(height) / EPOCH )); }
wait_epoch() { wait_height $(( $1 * EPOCH + 1 )); }
wait_height 3
[[ "$(api /upgrade | json "d['protocol_version']")" == 3 ]] || fail "the chain isn't running protocol v3"
echo "✓ 4 validators on protocol v3 at $RPC, epochs of $EPOCH blocks of $BLOCK_MS ms ($DIR)"

# step <name> <command…>: runs it, keeps its output in out/NN-name.txt, fails if it exits non-zero.
step() {
  local name=$1; shift
  N=$((N + 1)); LAST=$(printf '%s/%02d-%s.txt' "$DIR/out" "$N" "$name")
  if ! "$@" > "$LAST" 2>&1 < /dev/null; then cat "$LAST" >&2; fail "$name exited non-zero"; fi
}
has() { grep -q -- "$2" "$1" || { cat "$1" >&2; fail "$(basename "$1") lacks “$2”"; }; }
P="$BIN program"
# The current epoch's beacon as the node reports it: "<epoch> <reveals> <addresses that missed>".
beacon() { api /beacon | json "' '.join([str(d['current']['epoch']), str(d['current']['reveals']), ','.join(m['address'] for m in d['current']['missed'])])"; }

# ------------------------------------------------------------------------------- the dice
step wallet-new "$BIN" wallet new --rpc "$RPC"
step faucet "$BIN" wallet faucet --rpc "$RPC"
step upload $P upload "$GUESTS/dice.wasm" --interface "$GUESTS/dice.iface.json" --yes --rpc "$RPC"
HASH_W=$(awk '/^code / {print $2; exit}' "$LAST"); HASH_I=$(awk '/^interface / {print $2; exit}' "$LAST")
# Rolls need two reveals: one validator may withhold, none may be the only one to reveal.
step create $P create "$HASH_W" "$HASH_I" --init init --input 02000000 --storage-limit 1 --yes --rpc "$RPC"
DICE=$(awk '/^program / {print $2}' "$LAST")
[[ "$DICE" == boo1* ]] || fail "no program address from create"
step fund $P call "$DICE" fund --value 60 --yes --rpc "$RPC"
echo "✓ the dice is at $DICE with a bank of 60"
# A second wallet, which only settles other people's rolls.
SETTLER="$DIR/settler"; mkdir -p "$SETTLER"
step settler-new env BLOCKULA_HOME="$SETTLER" "$BIN" wallet new --rpc "$RPC"
step settler-faucet env BLOCKULA_HOME="$SETTLER" "$BIN" wallet faucet --rpc "$RPC"
# The dice's balance, in drops. A stake is 1 FANG, so the settler's bounty is 10,000,000 drops.
bank() { api "/account/$DICE" | json "d['balance']"; }
BOUNTY=10000000
# paid <bank before> <pick> : fails unless the bank paid out exactly what FACE says (six stakes
# for a hit on <pick>, nothing for a miss) and the bounty.
paid() {
  local want=$BOUNTY; [[ "$FACE" == "0$2" ]] && want=$((6000000000 + BOUNTY))
  local got=$(( $1 - $(bank) ))
  [[ "$got" -eq "$want" ]] || fail "the bank paid $got drops for face $FACE on pick $2 (expected $want)"
}

# roll <pick>: the roll's id (hex), its epoch in ROLLED_AT.
roll() {
  step "roll-$1" $P call "$DICE" roll --input "0$1" --value 1 --storage-limit 1 --yes --rpc "$RPC"
  ID=$(sed -n 's/.*roll returned \([0-9a-f]\{16\}\).*/\1/p' "$LAST")
  [[ ${#ID} == 16 ]] || { cat "$LAST" >&2; fail "the roll returned no id"; }
}
# settle <id>: sent by the second wallet. The face rolled (hex byte; 00 for a refund or a forfeit).
settle() {
  step "settle-$1" env BLOCKULA_HOME="$SETTLER" $P call "$DICE" settle --input "$1" --rpc "$RPC"
  FACE=$(sed -n 's/.*settle returned \([0-9a-f]\{2\}\).*/\1/p' "$LAST")
  [[ ${#FACE} == 2 ]] || { cat "$LAST" >&2; fail "the settle returned no face"; }
}

# ---- 1. A normal roll: every validator reveals.
wait_epoch 2
roll 3; A=$ID; A_EPOCH=$(epoch_now); A_TARGET=$((A_EPOCH + 2))
N=$((N + 1)); EARLY=$(printf '%s/%02d-settle-too-early.txt' "$DIR/out" "$N")
if $P call "$DICE" settle --input "$A" --rpc "$RPC" > "$EARLY" 2>&1 < /dev/null && ! grep -q "aborted with code 35" "$EARLY"; then cat "$EARLY" >&2; fail "a settle in the roll's own epoch went through"; fi
has "$EARLY" "aborted with code 35"
echo "✓ roll $A in epoch $A_EPOCH names epoch $A_TARGET; a settle now aborts (35: not yet)"
wait_epoch "$A_TARGET"
GOT=$(beacon); [[ "$GOT" == "$A_TARGET 4 " ]] || fail "the beacon is “$GOT” (expected epoch $A_TARGET, 4 reveals, nobody missed)"
BANK=$(bank)
settle "$A"
[[ "$FACE" =~ ^0[1-6]$ ]] || fail "roll $A settled on face $FACE"
has "$LAST" "settled"
paid "$BANK" 3
echo "✓ epoch $A_TARGET: 4 reveals; roll $A settled on face ${FACE#0} by another wallet, which got the bounty"

# ---- 2. Validator 2 commits for an epoch and can't reveal.
roll 5; B=$ID; B_EPOCH=$(epoch_now); B_TARGET=$((B_EPOCH + 2))
# Its commitment for B_TARGET is on-chain once it's in the round; then it loses its secrets.
V2=$(json "d['validators'][2]['address']" < "$DIR/genesis.json")
COMMITTED="any(p['validator']['address'] == '$V2' and p['committed_for_epoch_after_next'] is not None for p in d['validators']) and d['epoch'] == $B_EPOCH"
for _ in $(seq 1 120); do
  [[ "$(epoch_now)" -gt "$B_EPOCH" ]] && fail "validator 2 didn't commit for epoch $B_TARGET during epoch $B_EPOCH"
  [[ "$(api /beacon | json "$COMMITTED")" == True ]] && break
  sleep 0.5
done
kill "${PIDS[2]}" 2>/dev/null || true; wait "${PIDS[2]}" 2>/dev/null || true
echo '{}' > "$DIR/v2/beacon-secrets.json"
start_validator 2
wait_epoch "$B_TARGET"
GOT=$(beacon); [[ "$GOT" == "$B_TARGET 3 $V2" ]] || fail "the beacon is “$GOT” (expected epoch $B_TARGET, 3 reveals, validator 2 missed)"
[[ "$(api /validators | json "any(v['validator']['address'] == '$V2' and v['jailed'] for v in d['jailed'])")" == True ]] || fail "validator 2 isn't jailed"
BANK=$(bank)
settle "$B"
[[ "$FACE" =~ ^0[1-6]$ ]] || fail "roll $B settled on face $FACE"
paid "$BANK" 5
echo "✓ epoch $B_TARGET: 3 reveals, validator 2 missed and is jailed; roll $B settled on face ${FACE#0}"

# ---- 3. Two rolls left alone past their deadline.
if [[ -z "${SKIP_EXPIRY:-}" ]]; then
  roll 6; C=$ID; C_EPOCH=$(epoch_now); C_TARGET=$((C_EPOCH + 2)); C_DEADLINE=$((C_TARGET + 32))
  roll 6; D=$ID; D_TARGET=$(( $(epoch_now) + 2 )); D_GONE=$((D_TARGET + 64))
  echo "… rolls $C and $D in epoch $C_EPOCH; waiting for epoch $((C_DEADLINE + 1)) (about $(( (C_DEADLINE + 1 - C_EPOCH) * EPOCH * BLOCK_MS / 1000 )) s)"
  wait_epoch $((C_DEADLINE + 1))
  BANK=$(bank)
  settle "$C"
  [[ "$FACE" =~ ^0[1-6]$ ]] || fail "roll $C settled on face $FACE past its deadline, while its value is kept"
  has "$LAST" "settled"
  paid "$BANK" 6
  echo "✓ epoch $((C_DEADLINE + 1)): past roll $C's deadline (epoch $C_DEADLINE) it settled on face ${FACE#0} all the same: no refund for waiting"
  echo "… waiting for epoch $D_GONE, the first without epoch $D_TARGET's value (about $(( (D_GONE - C_DEADLINE - 1) * EPOCH * BLOCK_MS / 1000 )) s)"
  wait_epoch "$D_GONE"
  BANK=$(bank)
  settle "$D"
  [[ "$FACE" == 00 ]] || fail "roll $D settled on face $FACE after its value was gone"
  has "$LAST" "forfeited"
  GOT=$(( BANK - $(bank) ))
  [[ "$GOT" -eq "$BOUNTY" ]] || fail "the forfeit moved $GOT drops out of the bank (expected the bounty, $BOUNTY)"
  echo "✓ epoch $D_GONE: epoch $D_TARGET's value is gone; roll $D's stake is forfeit to the bank, less the settler's bounty"
fi

# Every node that's running agrees on the head.
H=$(( $(height) - 2 ))
ROOTS=$(for i in 0 1 2 3; do curl -fsS -m 5 "http://$(http "$i")/api/block/$H" | json "d['hash']"; done | sort -u | wc -l)
[[ "$ROOTS" -eq 1 ]] || fail "the validators disagree about block $H"
echo "✅ randomness e2e passed: 4 validators agree at block $H"
