#!/usr/bin/env bash
# `blockula program …` against a `blockula dev --programs` chain, as a person runs it (docs/PROGRAMS-V0.md
# §8.3; WBS S6b's acceptance): the interface offline, upload, create straight after it (which waits for
# the code to be usable), two calls, a view, the reads, what is refused before anything is signed,
# the validator's pause and what it stops. Also `summon --programs` and the dev chain refusing
# `--programs` on a chain that was made without. The dev chain and the CLI's wallet live in a temporary
# BLOCKULA_HOME; nothing touches ~/.blockula or any public network.
#
#   scripts/test-program-cli.sh                     # node/target/release/blockula
#   BIN=… PORT=27970 scripts/test-program-cli.sh    # another binary / port
#   KEEP=1 scripts/test-program-cli.sh              # keep the directory (it's printed) and the outputs
#
# Each command's output is kept in $DIR/out/ and printed at the end. Every command runs with its stdin
# closed (a person's terminal isn't there to answer a [y/N]: `upload` and `create` pass --yes, and are
# checked to refuse without it). Exits 0 when every check holds.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-node/target/release/blockula}
PORT=${PORT:-27970}
DIR=${DIR:-$(mktemp -d "${TMPDIR:-/tmp}/blockula-program-cli.XXXXXX")}
export BLOCKULA_HOME="$DIR/home"
RPC="http://127.0.0.1:$PORT"
OUT="$DIR/out"
GUESTS=node/vm/tests/programs
mkdir -p "$BLOCKULA_HOME" "$OUT"
DEV_PID=""
N=0

cleanup() {
  [[ -n "$DEV_PID" ]] && kill "$DEV_PID" 2>/dev/null || true
  if [[ -z "${KEEP:-}" ]]; then rm -rf "$DIR"; else echo "kept $DIR"; fi
}
trap cleanup EXIT

fail() { echo "❌ FAIL: $*" >&2; echo "--- dev log (tail) ---" >&2; tail -20 "$DIR/dev.log" >&2 || true; exit 1; }

# step <name> <command…>: runs it, keeps its output in out/NN-name.txt, fails if it exits non-zero.
step() {
  local name=$1; shift
  N=$((N + 1)); local f; f=$(printf '%s/%02d-%s.txt' "$OUT" "$N" "$name")
  echo "\$ $*" > "$f"
  if ! "$@" >> "$f" 2>&1 < /dev/null; then cat "$f" >&2; fail "$name exited non-zero"; fi
  LAST=$f
}
# refuse <name> <phrase> <command…>: the command must fail, and say <phrase>.
refuse() {
  local name=$1 phrase=$2; shift 2
  N=$((N + 1)); local f; f=$(printf '%s/%02d-%s.txt' "$OUT" "$N" "$name")
  echo "\$ $*" > "$f"
  if "$@" >> "$f" 2>&1 < /dev/null; then cat "$f" >&2; fail "$name was expected to fail"; fi
  grep -q -- "$phrase" "$f" || { cat "$f" >&2; fail "$name failed, but not with “$phrase”"; }
  LAST=$f
}
has() { grep -q -- "$2" "$1" || { cat "$1" >&2; fail "$(basename "$1") lacks “$2”"; }; }
hasnt() { if grep -q -- "$2" "$1"; then cat "$1" >&2; fail "$(basename "$1") has “$2”"; fi; }
api() { curl -fsS "$RPC/api$1"; }

# ----------------------------------------------------------------------- summon --programs
SUMMONED="$DIR/summoned"
step summon "$BIN" summon pc-summon --programs --home "$SUMMONED" --block-time-ms 1000
has "$LAST" "programs are on from block 1"
grep -q '"features"' "$SUMMONED/genesis.json" && grep -q '"programs"' "$SUMMONED/genesis.json" && grep -q '"activation_height": 1' "$SUMMONED/genesis.json" || fail "the summoned genesis lacks v2 at block 1 and the programs feature"
PLAIN="$DIR/plain"
step summon-plain "$BIN" summon pc-plain --home "$PLAIN"
if grep -q '"features"\|"upgrades"' "$PLAIN/genesis.json"; then fail "a summon without --programs wrote features or upgrades"; fi

# `dev --programs` can't change a chain that was made without programs.
mkdir -p "$DIR/home2"
"$BIN" summon blockula-dev --home "$DIR/home2/dev" > /dev/null 2>&1
refuse dev-programs-on-a-plain-chain "made without programs" env BLOCKULA_HOME="$DIR/home2" "$BIN" dev --programs --port $((PORT + 1))

# ------------------------------------------------------------------------------- the dev chain
"$BIN" dev --programs --port "$PORT" > "$DIR/dev.log" 2>&1 &
DEV_PID=$!
for _ in $(seq 1 90); do
  if curl -fsS "$RPC/api/status" 2>/dev/null | grep -q '"height":[1-9]'; then break; fi
  kill -0 "$DEV_PID" 2>/dev/null || fail "blockula dev --programs exited"
  sleep 1
done
curl -fsS "$RPC/api/status" | grep -q '"height":[1-9]' || fail "the dev chain never produced a block"
grep -q "Programs are on from block 1" "$DIR/dev.log" || fail "blockula dev --programs didn't say programs are on"
grep -q "experimental and not yet audited" "$DIR/dev.log" || fail "blockula dev --programs didn't say programs are experimental and not yet audited"
[[ "$(api /params | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["programs"]["status"])')" == on ]] || fail "programs aren't on in the dev chain"

# ------------------------------------------------------------------------------- the CLI
step wallet-new "$BIN" wallet new --rpc "$RPC"
step faucet "$BIN" wallet faucet --rpc "$RPC"
P="$BIN program"
step interface-encode $P interface encode "$GUESTS/counter.iface.json"
HASH_I=$(awk '/^interface_hash/ {print $2}' "$LAST")
[[ ${#HASH_I} == 64 ]] || fail "no interface hash from interface encode"

# An upload and a create ask before they sign (a deposit is permanent in v0): not at a terminal, they
# say so and sign nothing.
refuse upload-asks "pass --yes to confirm" $P upload "$GUESTS/counter.wasm" --interface "$GUESTS/counter.iface.json" --rpc "$RPC"
has "$LAST" "fees      at most"
has "$LAST" "never gives it back"
step upload $P upload "$GUESTS/counter.wasm" --interface "$GUESTS/counter.iface.json" --yes --rpc "$RPC"
has "$LAST" "deposit "
has "$LAST" "stored code"
has "$LAST" "also debited from the signer: upload deposit"
HASH_W=$(awk '/^code / {print $2; exit}' "$LAST")
[[ ${#HASH_W} == 64 ]] || fail "no code hash from upload"
[[ "$(awk '/^interface / {print $2; exit}' "$LAST")" == "$HASH_I" ]] || fail "upload's interface hash isn't interface encode's"

# Straight after the upload, with no waiting: `create` waits until the code is usable.
refuse create-asks "pass --yes to confirm" $P create "$HASH_W" "$HASH_I" --rpc "$RPC"
has "$LAST" "opening the program's account burns"
has "$LAST" "bonded for good"
step create $P create "$HASH_W" "$HASH_I" --yes --rpc "$RPC"
has "$LAST" "send nothing to that address before this create is final"
has "$LAST" "opening the program's account burns"
has "$LAST" "also debited from the signer: record deposit .* + account creation fee (burned)"
PROGRAM=$(awk '/^program / {print $2}' "$LAST")
[[ "$PROGRAM" == boo1* ]] || fail "no program address from create: $PROGRAM"
has "$LAST" "created program $PROGRAM"

# The first increment bonds a storage entry: a call's --storage-limit is 0 unless it says otherwise.
refuse call-without-a-deposit "--storage-limit (0 unless given)" $P call "$PROGRAM" increment --rpc "$RPC"
has "$LAST" "storage limit 0: this call can't bond new storage"
has "$LAST" "fees at most"
has "$LAST" "at version [0-9a-f]\{64\}$"
step call-1 $P call "$PROGRAM" increment --storage-limit 1 --rpc "$RPC"
hasnt "$LAST" "storage limit 0: this call can't bond"
has "$LAST" "the programs report"
has "$LAST" "increment returned 0100000000000000"
has "$LAST" "fuel: reserved"
step call-2 $P call "$PROGRAM" increment --rpc "$RPC"
has "$LAST" "increment returned 0200000000000000"
step view $P view "$PROGRAM" get --rpc "$RPC"
has "$LAST" "return data 0200000000000000"
step show $P show "$PROGRAM" --rpc "$RPC"
has "$LAST" "methods (named by the deployer)"
has "$LAST" "immutable"
step code $P code "$HASH_W" --rpc "$RPC"
has "$LAST" "methods (named by the deployer): get, increment"
step interface $P interface "$HASH_I" --rpc "$RPC"
has "$LAST" "- get (view)"

# Refused before anything is signed.
refuse name-for-program "never by a .boo name" $P call alice.boo get --rpc "$RPC"
refuse call-a-view "is a view: read it with" $P call "$PROGRAM" get --rpc "$RPC"
refuse view-a-call "is a call, which only a transaction runs" $P view "$PROGRAM" increment --rpc "$RPC"
refuse over-the-fuel-bound "above program_max_tx_fuel" $P call "$PROGRAM" increment --fuel 999999999 --rpc "$RPC"
refuse send-to-unpayable "isn't payable" $P call "$PROGRAM" increment --value 1 --yes --rpc "$RPC"
# (Code that is stored already has no deposit to fall short of; the token guest is new.)
refuse short-storage-limit "is below the deposit" $P upload "$GUESTS/token.wasm" --interface "$GUESTS/token.iface.json" --storage-limit 0.000001 --yes --rpc "$RPC"

# The validator pauses programs: from the next block, calls are refused and cost nothing; views go on.
[[ "$(api /params | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["programs"]["fix_pending"])')" == False ]] || fail "a fix is pending on a fresh dev chain"
step pause $P pause --key "$BLOCKULA_HOME/dev/validator.key" --yes --rpc "$RPC"
has "$LAST" "A pause stops new program transactions"
has "$LAST" "may sign now"
sleep 2
[[ "$(api /params | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["programs"]["status"])')" == paused ]] || fail "programs aren't paused after the pause"
refuse call-while-paused "programs are paused until block" $P call "$PROGRAM" increment --rpc "$RPC"
refuse second-pause "may not sign a pause now" $P pause --key "$BLOCKULA_HOME/dev/validator.key" --yes --rpc "$RPC"
step view-while-paused $P view "$PROGRAM" get --rpc "$RPC"
has "$LAST" "return data 0200000000000000"

echo "✅ PASS: blockula program ran against \`blockula dev --programs\` ($N steps). Outputs:"
for f in "$OUT"/*.txt; do echo "──── $(basename "$f")"; cat "$f"; done
