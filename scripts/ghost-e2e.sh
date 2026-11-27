#!/usr/bin/env bash
# Ghost Mode (docs/GHOST-MODE.md) end to end on a local dev chain, with `blockula ghost …` as a
# person would run it: `blockula dev --ghost` in a temporary directory, two wallets, and
#
#   1. bob registers a name and publishes his Ghost Mode address;
#   2. alice shields 40, 30 and 20 FANG (three notes);
#   3. alice sends 80 FANG privately to bob's name: that takes three notes, so the CLI merges two
#      first and says so;
#   4. bob's scan finds the note and its memo;
#   5. bob unshields 25.5 FANG to a public account that doesn't exist yet;
#   6. the two wallets' shielded balances, the public account and `/api/ghost`'s pool add up.
#
# Nothing here touches ~/.blockula, another node or any public network: the chain, its wallets and
# their note stores live under a temporary directory (BLOCKULA_HOME), the node listens on
# 127.0.0.1 only, and its p2p port is one the OS picks (`dev` never uses 30300). No key, seed or
# note secret is printed: the commands print none, and the viewing key this exports goes to a
# file whose length is all that's read.
#
#   scripts/ghost-e2e.sh                 # node/target/release/blockula on port 7171, about 15 seconds
#   BIN=… PORT=7272 scripts/ghost-e2e.sh
#   KEEP=1 scripts/ghost-e2e.sh          # keep the directory (it's printed) and the node's log
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-node/target/release/blockula}
PORT=${PORT:-7171}
DIR=${DIR:-$(mktemp -d "${TMPDIR:-/tmp}/blockula-ghost.XXXXXX")}
RPC="http://127.0.0.1:$PORT"
NODE_PID=
N=0

cleanup() {
  if [[ -n "$NODE_PID" ]]; then kill "$NODE_PID" 2>/dev/null || true; wait "$NODE_PID" 2>/dev/null || true; fi
  if [[ -z "${KEEP:-}" ]]; then rm -rf "$DIR"; else echo "kept $DIR"; fi
}
trap cleanup EXIT
fail() { echo "❌ FAIL: $*" >&2; if [[ -f "$DIR/node.log" ]]; then echo "--- node log (tail) ---" >&2; tail -5 "$DIR/node.log" >&2; fi; exit 1; }
json() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
api() { curl -fsS -m 5 "$RPC/api$1"; }

[[ -x "$BIN" ]] || fail "$BIN isn't built: cargo build --release --manifest-path node/Cargo.toml"
# The ports other things on a developer's machine use, and anything already listening.
case "$PORT" in 7070|3000|30300) fail "port $PORT is left alone: pick another (PORT=7171)";; esac
if curl -s -m 1 -o /dev/null "http://127.0.0.1:$PORT/" 2>/dev/null || nc -z 127.0.0.1 "$PORT" 2>/dev/null; then fail "something is already listening on port $PORT: pick another (PORT=…)"; fi

# ------------------------------------------------------------------------------- the chain
mkdir -p "$DIR/node" "$DIR/alice" "$DIR/bob" "$DIR/carol" "$DIR/out"
BLOCKULA_HOME="$DIR/node" "$BIN" dev --ghost --port "$PORT" > "$DIR/node.log" 2>&1 &
NODE_PID=$!
height() { api /status 2>/dev/null | json "d['height']" 2>/dev/null || echo 0; }
for _ in $(seq 1 120); do [[ "$(height)" -ge 2 ]] && break; kill -0 "$NODE_PID" 2>/dev/null || fail "the dev node exited"; sleep 0.5; done
[[ "$(height)" -ge 2 ]] || fail "the dev chain made no blocks"
[[ "$(api /ghost | json "d['active']")" == True ]] || fail "Ghost Mode isn't on in the dev chain"
[[ "$(api /status | json "d['chain_id']")" == blockula-dev ]] || fail "the node on port $PORT isn't the dev chain this script started"
CAP=$(api /ghost | json "d['pool_cap_display']")
echo "✓ blockula dev --ghost at $RPC: Ghost Mode on, pool cap $CAP ($DIR)"

# step <name> <who> <command…>: runs `blockula <command…>` as that wallet, keeps its output in
# out/NN-name.txt (LAST), fails if it exits non-zero.
step() {
  local name=$1 who=$2; shift 2
  N=$((N + 1)); LAST=$(printf '%s/%02d-%s.txt' "$DIR/out" "$N" "$name")
  if ! BLOCKULA_HOME="$DIR/$who" "$BIN" "$@" > "$LAST" 2>&1 < /dev/null; then cat "$LAST" >&2; fail "$name exited non-zero"; fi
}
has() { grep -q -- "$2" "$1" || { cat "$1" >&2; fail "$(basename "$1") lacks “$2”"; }; }
address_of() { sed -n 's/.*New wallet \([a-z0-9]*\).*/\1/p' "$1"; }
txs_of() { sed -n 's/^✓ .*(tx \([0-9a-f]*\).*/\1/p' "$1" | tr '\n' ' '; }
proofs_of() { sed -n 's/^ *proved in \([0-9]*\) ms · \([0-9]*\) bytes.*/\1ms\/\2B/p' "$1" | tr '\n' ' '; }
# shielded <who>: SHIELDED is that wallet's shielded balance as `ghost balance` prints it, in drops
# (LAST is its output). Called as a command, never in $(…), so `step` keeps its count.
shielded() {
  step "balance-$1" "$1" ghost balance --rpc "$RPC"
  SHIELDED=$(python3 -I -c "import sys,re; from decimal import Decimal; m=re.search(r'👻 ([0-9.]+) \S+ shielded', open(sys.argv[1]).read()); print(int(Decimal(m.group(1))*10**9))" "$LAST")
}
fang() { python3 -I -c "import sys; from decimal import Decimal; print(Decimal(int(sys.argv[1]))/10**9)" "$1"; }

# ------------------------------------------------------------------------------- the wallets
step alice-new alice wallet new --rpc "$RPC"; ALICE=$(address_of "$LAST")
step bob-new bob wallet new --rpc "$RPC"; BOB=$(address_of "$LAST")
step carol-new carol wallet new --rpc "$RPC"; CAROL=$(address_of "$LAST")
[[ "$ALICE" == boo1* && "$BOB" == boo1* && "$CAROL" == boo1* ]] || fail "wallet new printed no address"
step alice-faucet alice wallet faucet --rpc "$RPC"
step bob-faucet bob wallet faucet --rpc "$RPC"
echo "✓ alice $ALICE and bob $BOB have 100 FANG each; carol $CAROL has no account yet"

# ---- 1. bob: a name, and his Ghost Mode address under it.
NAME="ghostbob$((RANDOM % 9000 + 1000))"
step bob-name bob wallet name "$NAME" --rpc "$RPC"
step bob-publish bob ghost address --publish --rpc "$RPC"
has "$LAST" "published a Ghost Mode address"
step bob-address bob ghost address --rpc "$RPC"
has "$LAST" "Published under $BOB"
FINGERPRINT=$(sed -n 's/.*fingerprint: \([0-9a-f-]*\).*/\1/p' "$LAST")
[[ ${#FINGERPRINT} == 24 ]] || fail "no fingerprint from ghost address"
[[ "$(api "/ghost/address/$NAME.boo" | json "d['fingerprint']")" == "$FINGERPRINT" ]] || fail "the chain files another address under $NAME.boo"
echo "✓ bob is $NAME.boo, his Ghost Mode address is published (fingerprint $FINGERPRINT)"

# ---- 2. alice shields three notes.
SHIELDS=; PROOFS=
for amount in 40 30 20; do
  step "alice-shield-$amount" alice ghost shield "$amount" --rpc "$RPC"
  has "$LAST" "shielded $amount FANG"
  SHIELDS+=$(txs_of "$LAST"); PROOFS+=$(proofs_of "$LAST")
done
shielded alice; [[ "$SHIELDED" == 90000000000 ]] || fail "alice's shielded balance isn't 90 FANG"
[[ "$(api /ghost | json "d['pool']")" == 90000000000 ]] || fail "the pool isn't 90 FANG"
echo "✓ alice shielded 40 + 30 + 20 FANG: txs $SHIELDS"

# ---- 3. alice → bob, privately: three notes, so a merge first.
FEE=$(api /ghost | json "d['canonical_fee']"); FEE_NEW=$(api /ghost | json "d['canonical_fee_new_account']")
step alice-send alice ghost send "$NAME.boo" 80 --memo "for the garlic" --fingerprint "$FINGERPRINT" --rpc "$RPC"
has "$LAST" "one action spends at most two"
has "$LAST" "merged 2 notes"
has "$LAST" "sent 80 FANG privately to $NAME.boo"
SENDS=$(txs_of "$LAST"); PROOFS+=$(proofs_of "$LAST")
# A ghost transfer names nobody: the explorer's summary is all the chain says about it.
SEND_TX=$(echo "$SENDS" | awk '{print $NF}')
SUMMARY=$(api "/tx/$SEND_TX" | json "d['summary']")
[[ "$SUMMARY" == *"a ghost transaction happened"* ]] || fail "the private transfer's summary is “$SUMMARY”"
[[ "$(api "/tx/$SEND_TX" | json "d.get('from') or ''")" == "" ]] || fail "the private transfer names a sender"
echo "✓ alice sent 80 FANG to $NAME.boo in two actions (a merge, then the payment): txs $SENDS"

# ---- 4. bob's scan finds it.
step bob-scan bob ghost scan --rpc "$RPC"
has "$LAST" "found 1 new note of yours"
shielded bob; [[ "$SHIELDED" == 80000000000 ]] || fail "bob's shielded balance isn't 80 FANG"
has "$LAST" "for the garlic"
echo "✓ bob's scan found the note: 80 FANG shielded, memo “for the garlic”"

# ---- 5. bob unshields to carol, whose account this opens.
step bob-unshield bob ghost unshield "$CAROL" 25.5 --rpc "$RPC"
has "$LAST" "unshielded 25.5 FANG to $CAROL"
UNSHIELD=$(txs_of "$LAST"); PROOFS+=$(proofs_of "$LAST")
[[ "$(api "/account/$CAROL" | json "d['balance']")" == 25500000000 ]] || fail "carol's account doesn't hold 25.5 FANG"
echo "✓ bob unshielded 25.5 FANG to carol's new account: tx $UNSHIELD"

# ---- 6. It adds up.
shielded alice; A=$SHIELDED; shielded bob; B=$SHIELDED; POOL=$(api /ghost | json "d['pool']")
[[ "$A" == $((10000000000 - 2 * FEE)) ]] || fail "alice holds $A drops shielded (expected 10 FANG less two fees of $FEE)"
[[ "$B" == $((54500000000 - FEE_NEW)) ]] || fail "bob holds $B drops shielded (expected 54.5 FANG less the fee $FEE_NEW)"
[[ "$POOL" == $((A + B)) ]] || fail "the pool holds $POOL drops and the wallets $((A + B))"
[[ "$POOL" == $((90000000000 - 25500000000 - 2 * FEE - FEE_NEW)) ]] || fail "the pool holds $POOL drops"
[[ "$(api /ghost | json "d['frontier_size']")" == 12 && "$(api /ghost | json "d['nullifiers']")" == 12 ]] || fail "six actions should have left 12 leaves and 12 nullifiers"
[[ "$(api '/ghost/commitments?from=10&limit=1' | json "str(len(d['commitments'])) + ' ' + str(d['next'])")" == "1 11" ]] || fail "/api/ghost/commitments doesn't page"

# The note stores are private files, and a viewing key exports (to a file: only its length is read).
for w in alice bob; do
  STORE=$(ls "$DIR/$w/ghost/"*.json)
  MODE=$(stat -f %Lp "$STORE" 2>/dev/null || stat -c %a "$STORE")
  [[ "$MODE" == 600 ]] || fail "$w's note store is mode $MODE, not 600"
done
BLOCKULA_HOME="$DIR/bob" "$BIN" ghost export-viewing-key --incoming > "$DIR/out/bob-ivk.hex" 2> /dev/null
[[ "$(tr -d '\n' < "$DIR/out/bob-ivk.hex" | wc -c | tr -d ' ')" == 194 ]] || fail "the incoming viewing key isn't 97 bytes of hex"
# No seed in anything a command or the node printed (read and compared inside one process: the
# seeds never reach a command line or this script's variables).
python3 -I - "$DIR" <<'PY' || fail "a wallet seed was printed"
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
seeds = [json.loads((root / w / "wallet.json").read_text())["seed"] for w in ("alice", "bob", "carol")]
printed = [p.read_text(errors="replace") for p in [*(root / "out").iterdir(), root / "node.log"]]
sys.exit(1 if any(s in text for s in seeds for text in printed) else 0)
PY

echo
echo "✓ Ghost Mode end to end on $(api /status | json "d['chain_id']") at block #$(height):"
echo "    alice shielded     $(fang "$A") FANG   (90 shielded − 80 sent − 2 fees of $(fang "$FEE"))"
echo "    bob shielded       $(fang "$B") FANG   (80 received − 25.5 unshielded − a fee of $(fang "$FEE_NEW"), which opened carol's account)"
echo "    carol (public)     25.5 FANG"
echo "    pool (/api/ghost)  $(fang "$POOL") FANG = alice + bob"
echo "    prove time / tx size per action: $PROOFS"
echo "    shields $SHIELDS"
echo "    merge + send $SENDS"
echo "    unshield $UNSHIELD"
