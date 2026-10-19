#!/usr/bin/env bash
# Rehearses the public testnet's launch on this machine, the way deploy/launch.sh and
# deploy/entrypoint.sh will run it on Fly: launch.sh (GENESIS_ONLY) builds the keys, operator
# files, spec and genesis, with local addresses and short windows; four validators and the public
# node start through deploy/entrypoint.sh (validators seeded from VALIDATOR_KEY_B64, the public
# node joining with a checkpoint from its PEER_HTTP validators). Then it checks:
#   - blocks finalize, and the bootstrap delegation backs all four seats;
#   - the faucet works through the public node (forwarded to a validator);
#   - the contribution pool is open from genesis (its repo, opening balance, founding jurors);
#   - a validator that goes offline is jailed and leaves, restarts from its volume alone (no
#     secret), its owner tops up the self-bond and unjails it, and it rejoins;
#   - after the (shortened) weak-subjectivity period, a plain `join` from the genesis is refused,
#     and a fresh public node joins through the entrypoint's checkpoint path;
#   - from here on with Fly's p2p preset (public-nat, FLY_P2P_PRESET; the steps above run `local`):
#     every validator restarting at once (launch.sh's key removal and redeploys), then each one
#     restarting in turn (set-github-token.sh), through the participation gate those scripts use
#     (deploy/restart-gate.sh): each comes back signing, linked to the other three and proposing,
#     the chain never stops for MAX_GAP seconds (two validators out), and nobody is jailed;
#   - the public node restarting from its joined home;
#   - the SDK e2e passes against the public node, and every node agrees on a block and root.
#
#   scripts/rehearse-testnet.sh                 # build, run, tear down
#   KEEP=1 scripts/rehearse-testnet.sh          # leave it running
#   BASE_PORT=32400 scripts/rehearse-testnet.sh # ports BASE..BASE+3 (p2p), +10..+13 (validators' HTTP), +20, +21 (public nodes)
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-$PWD/node/target/release/blockula}
CHAIN_ID=${CHAIN_ID:-blockula-rehearsal-local}
BASE=${BASE_PORT:-31400}
EPOCH=${EPOCH:-20}
WINDOW=${WINDOW:-120}          # unbonding and evidence window: the weak-subjectivity period is half
FLY_PRESET=${FLY_P2P_PRESET:-public-nat}   # what deploy/fly-config.sh runs validators with
MAX_GAP=${MAX_GAP:-20}         # seconds without a block that count as a stall during the restarts
DIR=$(mktemp -d "${TMPDIR:-/tmp}/blockula-rehearse.XXXXXX")
NET=$HOME/.config/blockula/$CHAIN_ID-rehearsal-$(date +%s)
RPC_PORT=$((BASE + 20)); RPC=http://127.0.0.1:$RPC_PORT
JOIN_PORT=$((BASE + 21)); JOINER=http://127.0.0.1:$JOIN_PORT
PIDS=()

passed=""
cleanup() {
  if [[ -z "${KEEP:-}" ]]; then
    for p in ${PIDS[@]+"${PIDS[@]}"}; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    # A pass leaves nothing behind; a failure keeps the logs and keys to look at.
    if [[ -n "$passed" ]]; then rm -rf "$DIR" "$NET"; else echo "logs: $DIR; keys and genesis: $NET" >&2; fi
  else
    echo "🧛 still running (KEEP=1). Nodes and logs: $DIR; keys and genesis: $NET"
  fi
}
trap cleanup EXIT
fail() { echo "❌ $*" >&2; exit 1; }
for p in $(seq "$BASE" $((BASE + 3))) $(seq $((BASE + 10)) $((BASE + 13))) "$RPC_PORT" "$JOIN_PORT"; do
  ! lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1 || fail "port $p is in use (set BASE_PORT)"
done
[[ -n "${SKIP_BUILD:-}" ]] || cargo build --release --locked --manifest-path node/Cargo.toml -q
echo "📁 nodes: $DIR  keys: $NET"

# Four hosts the genesis builder sees as four (it refuses two operators on one p2p host), all this
# machine: validators bind :: (dual stack).
P2P_ADDRS="127.0.0.1:$BASE localhost:$((BASE + 1)) [::1]:$((BASE + 2)) [::ffff:127.0.0.1]:$((BASE + 3))"
# A short downtime window, so an offline validator is jailed within a minute: 12 of 40 leader
# slots. Far stricter than the testnet's 600 of 1,200, and wide enough for a restart under
# Fly's p2p preset that relinks within seconds (one that takes minutes is jailed here: a failure).
GENESIS_ONLY=1 REHEARSAL_DIR=$NET CHAIN_ID=$CHAIN_ID EPOCH_BLOCKS=$EPOCH UNBONDING_BLOCKS=$WINDOW \
  P2P_ADDRS=$P2P_ADDRS RPC_URL=$RPC SKIP_BUILD=1 DOWNTIME_WINDOW=40 DOWNTIME_MAX_MISSED=12 \
  EXTRA_PARAMS='jail_blocks = 20\n' \
  deploy/launch.sh | tee "$DIR/launch.txt"
GHASH=$("$BIN" checkpoint --genesis "$NET/genesis.json")
SECRET=$(cat "$NET/forwarder-secret")

vhttp() { echo "http://127.0.0.1:$((BASE + 10 + $1))"; }
peers_of() { local out="" j; for j in 0 1 2 3; do [ "$j" = "$1" ] || out="$out $(vhttp "$j")"; done; echo "${out# } $RPC"; }
validators_http() { echo "$(vhttp 0) $(vhttp 1) $(vhttp 2) $(vhttp 3)"; }

# A validator through the entrypoint, with the p2p preset in PRESET; with "seed", from the secret
# (its first boot).
PRESET=local
start_validator() { # i [seed]
  local i=$1 key=""
  [ "${2:-}" = seed ] && key=$(base64 < "$NET/v$i/validator.key" | tr -d '\n')
  env ROLE=validator BLOCKULA_DATA="$DIR/v$i" BLOCKULA_ETC="$NET/v$i" BLOCKULA_BIN="$BIN" BIND=:: P2P_PRESET="$PRESET" \
    HTTP_PORT=$((BASE + 10 + i)) PEER_HTTP="$(peers_of "$i")" BLOCKULA_FORWARDER_SECRET="$SECRET" \
    ${key:+VALIDATOR_KEY_B64=$key} sh deploy/entrypoint.sh >> "$DIR/v$i.log" 2>&1 &
  PIDS[i]=$!
}
# A public node through the entrypoint (`join` with a checkpoint from the validators, then `run`).
start_rpc() { # data dir, port, log
  env ROLE=rpc BLOCKULA_DATA="$1" BLOCKULA_ETC="$NET" BLOCKULA_BIN="$BIN" BIND=127.0.0.1 HTTP_PORT="$2" \
    GENESIS_HASH="$GHASH" PEER_HTTP="$(validators_http)" BLOCKULA_FORWARDER_SECRET="$SECRET" CHECKPOINT_RETRY_SECS=2 \
    sh deploy/entrypoint.sh >> "$3" 2>&1 &
  PIDS+=($!)
}
j() { node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{const v=JSON.parse(s);console.log(String((function(){return $1})()))})"; }
get() { curl -fsS -m 5 "$1"; }
height() { get "$1/api/status" 2>/dev/null | j 'v.height' 2>/dev/null || echo 0; }
wait_height() { # url height [secs]
  for _ in $(seq 1 "${3:-180}"); do [ "$(height "$1")" -ge "$2" ] && return 0; sleep 1; done
  fail "$1 never reached height $2 (at $(height "$1"))"
}
until_true() { # what secs command…
  local what=$1 secs=$2; shift 2
  for _ in $(seq 1 "$secs"); do "$@" && return 0; sleep 1; done
  fail "timed out: $what"
}
# The participation gate launch.sh and set-github-token.sh wait for around restarts
# (deploy/restart-gate.sh). It reads each validator as on Fly, `fly ssh console -a <app> -C
# "blockula status --rpc …"`: here that runs `blockula status` against the validator's own port.
PREFIX=rehearsal N=4 RPC_URL=$RPC HTTP_PORT=7070 RPC_APP=rehearsal-rpc GENESIS_HASH=$GHASH
PROGRESS_SECS=${PROGRESS_SECS:-2} GATE_SECS=${GATE_SECS:-300}
# shellcheck source=deploy/restart-gate.sh
source deploy/restart-gate.sh
fly() {
  local app="" cmd=""
  while [ $# -gt 0 ]; do case $1 in -a) app=$2; shift ;; -C) cmd=$2; shift ;; esac; shift; done
  case "$app $cmd" in
    "$PREFIX-v"[0-3]" blockula status --rpc "*) "$BIN" status --rpc "$(vhttp "${app##*-v}")" ;;
    *) echo "rehearsal: no stand-in for fly -a $app -C $cmd" >&2; return 1 ;;
  esac
}
stop_node() { kill "$1" 2>/dev/null || true; wait "$1" 2>/dev/null || true; }
# The longest time between consecutive blocks from #$1 to #$2, in ms (the public node's blocks).
max_gap() {
  local before=$(($2 + 1)) low
  : > "$DIR/gap.jsonl"
  while [ "$before" -gt "$1" ]; do
    get "$RPC/api/blocks?before=$before&limit=100" | jq -c '.[] | [.height, .timestamp_ms]' >> "$DIR/gap.jsonl"
    low=$(tail -n 1 "$DIR/gap.jsonl" | jq '.[0]')
    [ "$low" -lt "$before" ] || break
    before=$low
  done
  jq -s --argjson lo "$1" --argjson hi "$2" 'map(select(.[0] >= $lo and .[0] <= $hi)) | unique_by(.[0]) | [range(1; length) as $k | .[$k][1] - .[$k - 1][1]] | max // 0' "$DIR/gap.jsonl"
}

for i in 0 1 2 3; do start_validator "$i" seed; done
start_rpc "$DIR/rpc" "$RPC_PORT" "$DIR/rpc.log"
wait_height "$(vhttp 0)" 3
for i in 0 1 2 3; do grep -q "validator key seeded" "$DIR/v$i.log" || fail "v$i wasn't seeded from VALIDATOR_KEY_B64"; done
echo "✓ 4 validators finalize blocks (each seeded from its secret on first boot)"
wait_height "$RPC" 3
echo "✓ the public node joined through the entrypoint ($(grep -o 'checkpoint [0-9]*:' "$DIR/rpc.log" | head -1 || echo 'from the genesis'))"

# The bootstrap delegation backs all four seats from the first boundary.
wait_height "$(vhttp 0)" $((EPOCH + 2))
V=$(get "$RPC/api/validators")
backed=$(echo "$V" | j 'v.active.filter(x => BigInt(x.stake) >= 25000000n * 10n**9n).length')
[ "$backed" = 4 ] || fail "the bootstrap delegation backs $backed of 4 seats: $(echo "$V" | j 'JSON.stringify(v.active.map(x => [x.validator.address, x.stake]))')"
echo "✓ the bootstrap delegation backs all 4 seats (25M FANG each)"

# The faucet, through the public node (forwarded to a validator, which pays from its float).
W=$("$BIN" keygen --prefix tboo); WADDR=$(echo "$W" | j 'v.address')
curl -fsS -X POST -H 'content-type: application/json' -d "{\"to\":\"$WADDR\"}" "$RPC/api/faucet" > /dev/null
until_true "the faucet drip" 60 sh -c "curl -fsS '$RPC/api/account/$WADDR' | grep -q '\"balance\":\"100000000000\"'"
echo "✓ the faucet paid 100 FANG through the public node"

# The contribution pool, open from genesis.
C=$(get "$RPC/api/contributions")
[ "$(echo "$C" | j 'JSON.stringify(v.repos)')" = '[{"repo":"jackdishman/blockula","weight":10}]' ] || fail "the pool's repos: $C"
pool=$(echo "$C" | j 'v.pool.balance')
[ "$(node -p "BigInt('$pool') >= 100000n * 10n**9n")" = true ] || fail "the pool holds $pool drops"
jurors=$(get "$RPC/api/souls?limit=10" | j 'v.souls.filter(s => s.soul.points >= 24000).length')
[ "$jurors" -ge 3 ] || fail "founding jurors' Soul: $(get "$RPC/api/souls?limit=10")"
echo "✓ the contribution pool is open: jackdishman/blockula, $(node -p "Number(BigInt('$pool') / 10n**9n)") FANG, $jurors founding jurors with Soul"
# A founding juror (a stand-in key launch.sh made) takes the faucet and joins the juror pool: its
# genesis Soul is what seats it.
J0=$NET/juror-0.json; J0ADDR=$(jq -r .address "$J0")
curl -fsS -X POST -H 'content-type: application/json' -d "{\"to\":\"$J0ADDR\"}" "$RPC/api/faucet" > /dev/null
until_true "juror-0's drip" 60 sh -c "curl -fsS '$RPC/api/account/$J0ADDR' | grep -q '\"balance\":\"100000000000\"'"
"$BIN" contrib join-jury --key "$J0" --rpc "$RPC" --chain-id "$CHAIN_ID" --genesis-hash "$GHASH" | tail -1
in_pool() { [ "$(get "$RPC/api/contributions" | j 'v.jurors.pool')" -ge 1 ]; }
until_true "a founding juror in the juror pool" 30 in_pool
echo "✓ a founding juror joined the juror pool on its genesis Soul"

# A validator goes offline: jailed (downtime), out at the boundary.
V3=$(jq -r .address "$NET/v3/consensus.json")
kill "${PIDS[3]}"; wait "${PIDS[3]}" 2>/dev/null || true
pkill -f "run --home $DIR/v3/home" 2>/dev/null || true
jailed() { get "$RPC/api/validators" | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{const v=JSON.parse(s);process.exit((v.jailed??[]).some(x=>x.validator.address==='$V3')&&!v.active.some(x=>x.validator.address==='$V3')?0:1)})"; }
until_true "v3's jailing and exit" 300 jailed
jail_until() { get "$RPC/api/validators" | j "(v.jailed.find(x => x.validator.address === '$V3') ?? {}).jailed?.until ?? ''"; }
echo "✓ v3 went offline: jailed until #$(jail_until), out of the set"
# It restarts from its volume (no secret this time), its owner tops up the slashed self-bond and
# unjails it. While it was offline it may have been jailed again (a missed beacon reveal, more
# missed slots), which keeps the later release: unjail once the newest one is over.
start_validator 3
OWNER3=$NET/owner-3.json
as_owner3() { "$@" --key "$OWNER3" --rpc "$RPC" --chain-id "$CHAIN_ID" --genesis-hash "$GHASH"; }
as_owner3 "$BIN" stake delegate "$V3" 50 | tail -1
unjailed=""
for _ in 1 2 3 4 5; do
  until_h=$(jail_until)
  [ -n "$until_h" ] || { unjailed=1; break; }
  wait_height "$RPC" "$until_h" 180
  if as_owner3 "$BIN" validator unjail "$V3" > "$DIR/unjail.log" 2>&1; then
    tail -1 "$DIR/unjail.log"; unjailed=1; break
  fi
  echo "  (unjail refused: $(tail -1 "$DIR/unjail.log"); retrying)"
  grep -qi "self-bond" "$DIR/unjail.log" && as_owner3 "$BIN" stake delegate "$V3" 50 | tail -1
  sleep 2
done
[ -n "$unjailed" ] || fail "v3 couldn't be unjailed: $(cat "$DIR/unjail.log")"
# Back in the set with the bootstrap delegation on it again: its share, as large as the other
# seats' (within 1%). Not 25M exactly: the downtime slash took the delegation's part of it too,
# and the delegation splits what's left evenly.
back() { get "$RPC/api/validators" | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{const v=JSON.parse(s);const me=v.active.find(x=>x.validator.address==='$V3');const others=v.active.filter(x=>x.validator.address!=='$V3').map(x=>BigInt(x.stake));if(!me||!others.length)process.exit(1);const low=others.reduce((a,b)=>a<b?a:b);process.exit(BigInt(me.stake)>=20000000n*10n**9n&&BigInt(me.stake)*100n>=low*99n?0:1)})"; }
until_true "v3 rejoining, backed by the bootstrap delegation" 300 back
[ "$(grep -c "validator key seeded" "$DIR/v3.log")" = 1 ] || fail "v3 was re-seeded on restart"
stake3=$(get "$RPC/api/validators" | j "Number(BigInt(v.active.find(x => x.validator.address === '$V3').stake) / 10n**9n)")
echo "✓ v3 restarted from its volume alone, was topped up and unjailed by its owner, and rejoined (backed again: $stake3 FANG at stake)"

# From here on the validators run Fly's p2p preset. A restarted validator relinks only as its
# peers' reconnection limits allow: public-nat's were Commonware's `recommended` (minutes) when
# they stretched the blockula-testnet-2 launch's restarts and jailed a validator, and this is
# where that shows up again if they regress. Do the restarts BEFORE the late join: restart
# downtime increases wall-clock lag, but a fresh timed checkpoint must still admit that joiner
# (review-r3-sync-1 / resume-relaunch-7).
four() { get "$RPC/api/validators" | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{const v=JSON.parse(s);process.exit(v.active.length===4&&!(v.jailed??[]).length?0:1)})"; }
set_of() { get "$RPC/api/validators" | j 'JSON.stringify({active: v.active.length, jailed: (v.jailed ?? []).map(x => x.validator.address)})'; }
PRESET=$FLY_PRESET
# launch.sh's key removal and its redeploys (relaunch step 6, upgrades) restart every validator at
# once: all four stop and start from their volumes, slashing protection must let them sign again,
# and the gate launch.sh waits for must see each one sign, link to the other three and propose.
gate_wait "before restarting every validator" gate_answering 0 1 2 3 || fail "before restarting every validator: $GATE_WHY"
starts="0=${GATE_STARTED[0]} 1=${GATE_STARTED[1]} 2=${GATE_STARTED[2]} 3=${GATE_STARTED[3]}"
h0=$(height "$(vhttp 0)"); t=$SECONDS
for i in 0 1 2 3; do kill "${PIDS[$i]}" 2>/dev/null || true; done
for i in 0 1 2 3; do stop_node "${PIDS[$i]}"; done
pkill -f "run --home $DIR/v[0-3]/home" 2>/dev/null || true
for i in 0 1 2 3; do start_validator "$i"; done
wait_height "$(vhttp 0)" $((h0 + 2 * EPOCH)) 300
for i in 0 1 2 3; do wait_height "$(vhttp "$i")" $((h0 + 2 * EPOCH)) 60; done
# shellcheck disable=SC2086 # one INDEX=START word per validator
gate_wait "every validator back" gate_check $starts || fail "after restarting every validator at once under $PRESET: $GATE_WHY"
four || fail "after restarting every validator at once under $PRESET: $(set_of)"
echo "✓ every validator restarted at once under $PRESET (the key removal, a redeploy): resumed at #$h0+, all 4 signing, linked and proposing in $((SECONDS - t)) s, none jailed"
# set-github-token.sh restarts them one at a time, each once the network is at full
# participation: before the next restart the gate must see the restarted validator sign, link to
# the other three and propose, and the chain back at full pace. A gate that let a restart through
# early would take a second validator out: no quorum, a gap of MAX_GAP seconds or more.
gate_wait "before the rolling restart" gate_check || fail "the network isn't at full participation before the rolling restart: $GATE_WHY"
r0=$(height "$RPC"); took=""
for i in 0 1 2 3; do
  start=${GATE_STARTED[$i]}; t=$SECONDS
  stop_node "${PIDS[$i]}"
  pkill -f "run --home $DIR/v$i/home" 2>/dev/null || true
  sleep 4   # a Fly machine's restart
  start_validator "$i"
  gate_wait "v$i back" gate_check "$i=$start" || fail "v$i isn't back after its restart under $PRESET: $GATE_WHY"
  took="$took v$i $((SECONDS - t)) s,"
done
r1=$(height "$RPC")
gap=$(max_gap "$r0" "$r1")
[ "$gap" -lt $((MAX_GAP * 1000)) ] || fail "no block for $(gate_secs "$gap") s during the rolling restart (#$r0-#$r1): two validators were out at once"
four || fail "after the rolling restart under $PRESET: $(set_of)"
echo "✓ a rolling restart through the participation gate under $PRESET:${took%,} (restart to back); the longest wait for a block $(gate_secs "$gap") s, none jailed"
RPC_PID=${PIDS[4]}
stop_node "$RPC_PID"
pkill -f "run --home $DIR/rpc/full" 2>/dev/null || true
pkill -f "join --genesis .* --home $DIR/rpc/full" 2>/dev/null || true
start_rpc "$DIR/rpc" "$RPC_PORT" "$DIR/rpc.log"
wait_height "$RPC" "$(height "$(vhttp 0)")" 120
grep -q "Full node for" "$DIR/rpc.log" || fail "the public node didn't restart: $(tail -5 "$DIR/rpc.log")"
echo "✓ the public node restarted from its joined home and caught up"

# Weak subjectivity: past the (shortened) period, the genesis alone is refused; the entrypoint's
# timed checkpoint path joins even after the network-wide restart above.
ws=$(get "$RPC/api/status" | j 'v.weak_subjectivity_blocks')
wait_height "$(vhttp 0)" $((ws + 2 * EPOCH)) 300
if "$BIN" join --genesis "$NET/genesis.json" --genesis-hash "$GHASH" --home "$DIR/stale" --port "$JOIN_PORT" > "$DIR/stale.log" 2>&1; then
  fail "a join from the genesis alone was accepted past the weak-subjectivity period ($ws blocks)"
fi
grep -q "weak-subjectivity" "$DIR/stale.log" || fail "the stale join failed for another reason: $(cat "$DIR/stale.log")"
echo "✓ past the weak-subjectivity period ($ws blocks), a join from the genesis alone is refused"
start_rpc "$DIR/joiner" "$JOIN_PORT" "$DIR/joiner.log"
wait_height "$JOINER" "$(height "$(vhttp 0)")" 300
cp_h=$(get "$JOINER/api/status" | j 'v.checkpoint_height')
[ "$cp_h" -gt 0 ] || fail "the joiner trusts no checkpoint: $(get "$JOINER/api/status")"
echo "✓ a fresh public node joined after the period through the entrypoint (checkpoint #$cp_h from its validators)"

echo "— SDK e2e against the public node"
BLOCKULA_RPC=$RPC npm test -w @blockula/sdk > "$DIR/e2e.log" 2>&1 || { tail -40 "$DIR/e2e.log" >&2; fail "SDK e2e (log: $DIR/e2e.log)"; }
tail -3 "$DIR/e2e.log"

# Agreement and disk use.
target=$(height "$(vhttp 0)")
ref=""
for n in $(validators_http) "$RPC" "$JOINER"; do
  wait_height "$n" "$target" 120
  line=$(get "$n/api/block/$target" | j 'v.hash + " " + v.state_root')
  [ -z "$ref" ] && ref=$line
  [ "$line" = "$ref" ] || fail "$n disagrees at #$target: $line vs $ref"
done
echo "✓ all 6 nodes agree on block #$target ($ref)"
for d in v0 rpc; do
  kb=$(du -sk "$DIR/$d" | cut -f1)
  echo "  $d: $((kb / 1024)) MiB for $target blocks, ~$((kb * 1024 / target)) bytes a block"
done
passed=1
echo "✓ rehearsal passed ($CHAIN_ID, genesis $GHASH)"
