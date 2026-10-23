#!/usr/bin/env bash
# Offline tests of the testnet's deploy scripts (CI `ops` job): deploy/entrypoint.sh against a
# stand-in `blockula` that records how it was started, the machine configurations
# deploy/launch.sh deploys (deploy/fly-config.sh), and launch.sh and set-github-token.sh against
# stand-in `fly`, `curl` and `blockula` commands. Nothing here talks to Fly.
#
#   bash deploy/test-deploy.sh
set -euo pipefail
cd "$(dirname "$0")/.."
T=$(mktemp -d "${TMPDIR:-/tmp}/blockula-deploy-test.XXXXXX")
trap 'rm -rf "$T"' EXIT
fails=0
ok() { echo "✓ $1"; }
bad() { echo "✗ $1" >&2; fails=$((fails + 1)); }
# The haystack goes to grep as a here-string, never through a pipe: under pipefail, grep -q quits at
# its match and a long haystack's writer then dies of SIGPIPE, which would read as no match (a
# refute passing over a log that holds what it refutes).
expect() { # what haystack needle
  if grep -qF -- "$3" <<<"$2"; then ok "$1"; else bad "$1: no \"$3\" in: $2"; fi
}
refute() { # what haystack needle
  if grep -qF -- "$3" <<<"$2"; then bad "$1: \"$3\" in: $2"; else ok "$1"; fi
}

# ---- the entrypoint, with a stand-in node ----

# The stand-in: logs its arguments and whether it saw the key's variable, then acts the part.
cat > "$T/blockula" <<'STUB'
#!/bin/sh
echo "$* | key_env=${VALIDATOR_KEY_B64:+set}" >> "$STUB_LOG"
case "$1" in
  checkpoint) [ -z "${STUB_CHECKPOINT_FAIL:-}" ] || { echo "no peer answered" >&2; exit 1; }; echo "4319:$(printf 'ab%.0s' $(seq 32)):$(printf 'cd%.0s' $(seq 32))" ;;
  join) while [ $# -gt 0 ]; do [ "$1" = --home ] && echo '{}' > "$2/join.json"; shift; done ;;
esac
exit "${STUB_EXIT:-0}"
STUB
chmod +x "$T/blockula"
SEED=$(printf '5a%.0s' $(seq 32))
KEY_B64=$(printf '%s\n' "$SEED" | base64 | tr -d '\n')

# entry <data dir> [VAR=value …]: runs the entrypoint (a fresh log), prints its exit code.
entry() {
  local data=$1; shift
  mkdir -p "$data" "$T/etc"
  [ -f "$T/etc/genesis.json" ] || echo '{"chain_id":"t"}' > "$T/etc/genesis.json"
  : > "$T/log"
  env -i PATH="$PATH" HOME="$T" BLOCKULA_DATA="$data" BLOCKULA_ETC="$T/etc" BLOCKULA_BIN="$T/blockula" STUB_LOG="$T/log" CHECKPOINT_RETRY_SECS=0 "$@" ${ENTRY_SH:-sh} deploy/entrypoint.sh > "$T/out" 2>&1 && echo 0 || echo $?
}
log() { cat "$T/log"; }

# Validator: the first boot seeds the key from the secret; the node never sees the variable.
d=$T/v
code=$(entry "$d" ROLE=validator VALIDATOR_KEY_B64="$KEY_B64" PEER_HTTP="http://a.internal:7070 http://b.internal:7171" PRUNE_BLOCKS=43200 NETWORK_HALTED_AT=1234 HALT_HEIGHT=99)
[ "$code" = 0 ] && ok "validator: first boot runs" || bad "validator: first boot exited $code: $(cat "$T/out")"
[ "$(cat "$d/home/validator.key")" = "$SEED" ] && ok "validator: key seeded from VALIDATOR_KEY_B64" || bad "validator: key not seeded"
[ "$(ls -l "$d/home/validator.key" | cut -c1-10)" = "-rw-------" ] && ok "validator: key is mode 600" || bad "validator: key mode $(ls -l "$d/home/validator.key")"
[ -f "$d/home/.seeded" ] && [ -f "$d/home/genesis.json" ] && ok "validator: volume marked seeded, genesis copied" || bad "validator: no .seeded or genesis"
expect "validator: runs with its flags" "$(log)" "run --home $d/home --bind :: --halt-height 99 --network-halted-at 1234 --prune-blocks 43200 --peer-http http://a.internal:7070 --peer-http http://b.internal:7171 | key_env="
refute "validator: the node never sees VALIDATOR_KEY_B64" "$(log)" "key_env=set"
# Later boots (the secret deleted from Fly, or still in a stale machine config): the volume's key.
code=$(entry "$d" ROLE=validator HTTP_PORT=7333)
[ "$code" = 0 ] && expect "validator: restarts from the volume alone (HTTP_PORT sets its HTTP port)" "$(log)" "run --home $d/home --port 7333 --bind ::" || bad "validator: restart exited $code: $(cat "$T/out")"
code=$(entry "$d" ROLE=validator VALIDATOR_KEY_B64="$(printf '%s\n' "$(printf '77%.0s' $(seq 32))" | base64 | tr -d '\n')")
[ "$(cat "$d/home/validator.key")" = "$SEED" ] && refute "validator: a secret never replaces the volume's key, nor reaches the node" "$(log)" "key_env=set" || bad "validator: key replaced"
# A volume that held a key once and lost it, or never had one and no secret: refuse.
rm "$d/home/validator.key"
code=$(entry "$d" ROLE=validator VALIDATOR_KEY_B64="$KEY_B64")
[ "$code" = 1 ] && [ ! -f "$d/home/validator.key" ] && [ ! -s "$T/log" ] && expect "validator: a seeded volume without its key refuses (no double-signing clone)" "$(cat "$T/out")" "keys are seeded once" || bad "validator: lost key exited $code"
code=$(entry "$T/v2" ROLE=validator)
[ "$code" = 1 ] && [ ! -s "$T/log" ] && ok "validator: no key and no secret refuses" || bad "validator: no key exited $code"
code=$(entry "$T/v3" ROLE=validator VALIDATOR_KEY_B64="$(printf 'not a key' | base64)")
[ "$code" = 1 ] && [ ! -e "$T/v3/home/validator.key" ] && [ ! -e "$T/v3/home/.seeded" ] && ok "validator: a malformed secret refuses and writes nothing" || bad "validator: bad secret exited $code"
# Deliberate stops stay down; crashes pass through.
code=$(entry "$T/v4" ROLE=validator VALIDATOR_KEY_B64="$KEY_B64" STUB_EXIT=3)
[ "$code" = 3 ] && ok "validator: a crash's exit code passes through (the restart policy restarts it)" || bad "validator: crash exited $code"
# 14: a thread the OS wouldn't start for a program (protocol v2), which a restart cures.
code=$(entry "$T/v4" ROLE=validator STUB_EXIT=14)
[ "$code" = 14 ] && ok "validator: exit 14 (a program's thread not started) passes through, to restart" || bad "validator: exit 14 exited $code"
# stays_down CODE WHAT NEEDLE: the entrypoint idles after the node exits CODE, and says why.
stays_down() {
  (env -i PATH="$PATH" BLOCKULA_DATA="$T/v4" BLOCKULA_ETC="$T/etc" BLOCKULA_BIN="$T/blockula" STUB_LOG="$T/log" STUB_EXIT="$1" ROLE=validator ${ENTRY_SH:-sh} deploy/entrypoint.sh > "$T/out$1" 2>&1) &
  local idle=$!
  sleep 2
  if kill -0 "$idle" 2>/dev/null; then
    kill -TERM "$idle"; wait "$idle" && ok "validator: exit $1 ($2) idles instead of exiting, and stops cleanly" || bad "validator: exit $1 idle exited $? on TERM"
  else
    bad "validator: exit $1 didn't idle: $(cat "$T/out$1")"
  fi
  expect "validator: exit $1: says why it stays down" "$(cat "$T/out$1")" "blockula exited $1: staying down, $3"
}
stays_down 10 "halt height" "it reached --halt-height"
stays_down 13 "a program faulted on a certified block" "a program faulted while this node executed a certified block"

# RPC: an empty home joins with a checkpoint its PEER_HTTP nodes agree on, pinned to GENESIS_HASH.
H=$(printf 'ee%.0s' $(seq 32))
CP="4319:$(printf 'ab%.0s' $(seq 32)):$(printf 'cd%.0s' $(seq 32))"
code=$(entry "$T/r" ROLE=rpc GENESIS_HASH="$H" PEER_HTTP="http://v0.internal:7070 http://v1.internal:7070" CLIENT_IP_HEADER=fly-client-ip PRUNE_BLOCKS=1209600 VALIDATOR_KEY_B64=leaked)
[ "$code" = 0 ] || bad "rpc: first boot exited $code: $(cat "$T/out")"
expect "rpc: asks the peers for a checkpoint of the pinned genesis" "$(log)" "checkpoint --peer http://v0.internal:7070 --peer http://v1.internal:7070 --genesis-hash $H"
expect "rpc: joins with it" "$(log)" "join --genesis $T/etc/genesis.json --genesis-hash $H --checkpoint $CP --home $T/r/full --port 7171 --bind :: --client-ip-header fly-client-ip --prune-blocks 1209600 --peer-http http://v0.internal:7070 --peer-http http://v1.internal:7070"
refute "rpc: never passes a validator secret on" "$(log)" "key_env=set"
code=$(entry "$T/r" ROLE=rpc GENESIS_HASH="$H" PEER_HTTP="http://v0.internal:7070" HTTP_PORT=7999)
[ "$code" = 0 ] && expect "rpc: a joined home restarts with run" "$(log)" "run --home $T/r/full --port 7999 --bind ::" || bad "rpc: restart exited $code"
refute "rpc: a joined home doesn't fetch a checkpoint again" "$(log)" "checkpoint"
code=$(entry "$T/r2" ROLE=rpc GENESIS_HASH="$H" PEER_HTTP="http://v0.internal:7070" CHECKPOINT="12:$H")
expect "rpc: CHECKPOINT overrides the peers" "$(log)" "--checkpoint 12:$H --home"
refute "rpc: with CHECKPOINT, no fetch" "$(log)" "checkpoint --peer"
code=$(entry "$T/r3" ROLE=rpc GENESIS_HASH="$H" PEER_HTTP="http://v0.internal:7070" STUB_CHECKPOINT_FAIL=1 CHECKPOINT_TRIES=2)
[ "$(grep -c '^checkpoint' "$T/log")" = 2 ] && ok "rpc: retries the peers" || bad "rpc: checkpoint tries: $(log)"
expect "rpc: then joins from the genesis alone (join refuses if that's stale)" "$(log)" "join --genesis $T/etc/genesis.json --genesis-hash $H --home"
expect "rpc: and says so" "$(cat "$T/out")" "no checkpoint from PEER_HTTP"
code=$(entry "$T/r4" ROLE=rpc ARCHIVE=1 PRUNE_BLOCKS=5)
expect "rpc: ARCHIVE=1 keeps everything, replayed from genesis (never fast-synced from a snapshot)" "$(log)" "join --genesis $T/etc/genesis.json --home $T/r4/full --port 7171 --bind :: --archive --full"
refute "rpc: ARCHIVE wins over PRUNE_BLOCKS" "$(log)" "--prune-blocks"
code=$(entry "$T/r4" ROLE=rpc ARCHIVE=1)
expect "rpc: an archive restarts with --full too" "$(log)" "run --home $T/r4/full --port 7171 --bind :: --archive --full"
code=$(entry "$T/r5" ROLE=rpc ARCHIVE=yes)
[ "$code" = 1 ] && ok "rpc: a malformed ARCHIVE refuses" || bad "rpc: ARCHIVE=yes exited $code"
code=$(entry "$T/x")
[ "$code" = 1 ] && ok "no ROLE refuses" || bad "no ROLE exited $code"

# ---- the machine configurations launch.sh deploys ----

PREFIX=blockula RPC_APP=blockula-testnet N=4 REGIONS=(iad sjc fra ams) RPC_REGION=iad NET=/home/op/net P2P_PORT=30300 HTTP_PORT=7070 RPC_PORT=7171
VALIDATOR_VM=shared-cpu-2x VALIDATOR_MEMORY=2048mb RPC_VM=shared-cpu-1x RPC_MEMORY=1024mb GENESIS_HASH=$H
VALIDATOR_PRUNE_BLOCKS="" RPC_PRUNE_BLOCKS=1209600 RPC_ARCHIVE="" HALT_HEIGHT="" NETWORK_HALTED_AT=""
# shellcheck source=deploy/fly-config.sh
source deploy/fly-config.sh
v1=$(validator_toml 1)
refute "validator config: no reference to the key's secret (restarts don't need it)" "$v1" "VALIDATOR_KEY"
refute "validator config: no secret files at all" "$v1" "secret_name"
expect "validator config: peers are the other validators and the public node" "$v1" 'PEER_HTTP = "http://blockula-v0.internal:7070 http://blockula-v2.internal:7070 http://blockula-v3.internal:7070 http://blockula-testnet.internal:7171"'
refute "validator config: not itself" "$v1" "blockula-v1.internal"
refute "validator config: the node's default window unless set" "$v1" "PRUNE_BLOCKS"
refute "validator config: no halt unless asked" "$v1" "HALT"
expect "validator config: one machine's restart policy" "$v1" 'policy = "on-failure"'
HALT_HEIGHT=500 NETWORK_HALTED_AT=499 VALIDATOR_PRUNE_BLOCKS=43200
v0=$(validator_toml 0)
expect "validator config: HALT_HEIGHT" "$v0" 'HALT_HEIGHT = "500"'
expect "validator config: NETWORK_HALTED_AT" "$v0" 'NETWORK_HALTED_AT = "499"'
expect "validator config: VALIDATOR_PRUNE_BLOCKS" "$v0" 'PRUNE_BLOCKS = "43200"'
r=$(rpc_toml)
expect "rpc config: pins the genesis" "$r" "GENESIS_HASH = \"$H\""
expect "rpc config: its HTTP port" "$r" 'HTTP_PORT = "7171"'
expect "rpc config: its checkpoint peers are the validators" "$r" 'PEER_HTTP = "http://blockula-v0.internal:7070 http://blockula-v1.internal:7070 http://blockula-v2.internal:7070 http://blockula-v3.internal:7070"'
expect "rpc config: keeps 14 days by default" "$r" 'PRUNE_BLOCKS = "1209600"'
refute "rpc config: never halts with the validators" "$r" "HALT"
expect "rpc config: restarts after a refused join" "$r" 'policy = "on-failure"'
RPC_ARCHIVE=1 RPC_SYNC_PEERS=http://blockula-rpc.internal:7171
r=$(rpc_toml)
expect "rpc config: RPC_ARCHIVE=1" "$r" 'ARCHIVE = "1"'
refute "rpc config: an archive has no window" "$r" "PRUNE_BLOCKS"
expect "rpc config: RPC_SYNC_PEERS adds peers after the validators" "$r" 'PEER_HTTP = "http://blockula-v0.internal:7070 http://blockula-v1.internal:7070 http://blockula-v2.internal:7070 http://blockula-v3.internal:7070 http://blockula-rpc.internal:7171"'
PY=${PYTHON:-python3}
if "$PY" -c "import tomllib" 2>/dev/null; then
  for cfg in "$v0" "$v1" "$r"; do
    printf '%s\n' "$cfg" | "$PY" -c "import sys, tomllib; tomllib.loads(sys.stdin.read())" && ok "config parses as TOML" || bad "config isn't TOML: $cfg"
  done
else
  echo "(python3 has no tomllib here: TOML syntax not checked)"
fi

# ---- launch.sh and set-github-token.sh, against a stand-in fly ----

# The stand-in fly, curl and network (deploy/test-standins.sh says what each knob does).
# shellcheck source=deploy/test-standins.sh
source deploy/test-standins.sh
mkdir -p "$T/bin" "$T/fly"
write_standins "$T/bin"
# A stand-in blockula for the genesis steps: keys, operator files, a genesis with the spec's clock
# (and the stand-in network's genesis hash).
cat > "$T/bin/blockula-launch" <<'STUB'
#!/usr/bin/env bash
case "$1 ${2:-}" in
  "keygen "*) echo "{\"address\":\"tboo1$(openssl rand -hex 8)\",\"seed\":\"$(openssl rand -hex 32)\"}" ;;
  "genesis operator") while [ $# -gt 0 ]; do [ "$1" = --out ] && echo "# operator" > "$2"; shift; done ;;
  "genesis build")
    spec=$4 out=$6
    printf '{"chain_id":"%s","created_at_ms":%s}\n' "$(sed -nE 's/^chain_id = "(.*)"/\1/p' "$spec")" "$(sed -nE 's/^genesis_time_ms = //p' "$spec")" > "$out" ;;
  "checkpoint --genesis") printf 'ab%.0s' $(seq 32); echo ;;
esac
STUB
chmod +x "$T/bin/blockula-launch"
world() { FAKE_FLY="$T/fly" "$T/bin/world" "$@"; }
wlog() { cat "$T/fly/world/log"; }
# Restarts the stand-in network logged while another validator wasn't back.
early() { grep -c 'while not back' "$T/fly/world/log" || true; }
# launch [VAR=value …]: runs launch.sh (a fresh log and network), prints its exit code.
launch() {
  : > "$T/fly/log"; : > "$T/fly/deploys"; world init
  env PATH="$T/bin:$PATH" HOME="$T/home" FAKE_FLY="$T/fly" BIN="$T/bin/blockula-launch" SKIP_BUILD=1 PROGRESS_SECS=0 GATE_ROUNDS=20 "$@" \
    bash deploy/launch.sh > "$T/launch.out" 2>&1 && echo 0 || echo $?
}
has() { grep -qx "$2" "$T/fly/$1.secrets" 2>/dev/null; }
NETDIR=$T/home/.config/blockula

# The stand-in network itself notices a restart while another validator isn't back yet (what the
# old gate, new finalized blocks on the restarted node, let through).
world init; world restart blockula-v0 secrets; world restart blockula-v1 secrets
[ "$(early)" = 1 ] && ok "stand-in network: flags a restart while another validator isn't back" || bad "stand-in network: $(wlog)"

# A first attempt dies at the image build, after the secrets were staged.
code=$(launch RESET=1 CHAIN_ID=t-a FAKE_FLY_BUILD_FAIL=1)
[ "$code" != 0 ] && [ -f "$NETDIR/t-a/secrets-staged" ] && [ ! -f "$NETDIR/t-a/launched" ] && [ ! -s "$T/fly/deploys" ] \
  && ok "launch: a failed image build stages secrets and deploys nothing" || bad "launch: failed build exited $code: $(cat "$T/launch.out")"
# The retry (the same command) wipes the apps' secrets, so it must stage them again.
code=$(launch RESET=1 CHAIN_ID=t-a)
[ "$code" = 0 ] || bad "launch: retry exited $code: $(cat "$T/launch.out")"
expect "launch: after RESET the retry stages each validator's key again" "$(cat "$T/fly/deploys")" "blockula-v3 key=yes"
refute "launch: no validator deploys without its key" "$(cat "$T/fly/deploys")" "v0 key=no"
has blockula-testnet BLOCKULA_FORWARDER_SECRET && ok "launch: the public node has the forwarder secret again" || bad "launch: the public node lost its forwarder secret"
has blockula-v2 VALIDATOR_KEY_B64 && bad "launch: a key was left in Fly" || ok "launch: every key removed from Fly once seeded"
# The key removal restarts the seeded validators once, together (no read of any validator between
# their restarts), then waits for all of them to sign, link and propose again.
together=$(awk '/^restart blockula-v[0-9]+ \(secrets\)/ { if (!first) first = NR; last = NR } /^status / { s[NR] = 1 } END { if (!first) { print "none"; exit } for (i = first; i <= last; i++) if (s[i]) { print "no"; exit } print "yes" }' "$T/fly/world/log")
[ "$together" = yes ] && [ "$(grep -c '^restart blockula-v[0-9]* (secrets)' "$T/fly/world/log")" = 4 ] \
  && ok "launch: the seeded validators restart once, together, to drop their keys" || bad "launch: key-removal restarts ($together): $(wlog | grep '^restart')"
expect "launch: then the gate waits for every one to sign, link and propose again" "$(cat "$T/launch.out")" "✓ after the restart: all 4 validators sign"
for i in 0 3; do expect "launch: blockula-v$i proposed since its restart" "$(cat "$T/launch.out")" "blockula-v$i proposed #"; done
refute "launch: no key in argv (hex)" "$(cat "$T/fly/log")" "$(tr -d '\n' < "$NETDIR/t-a/v0/validator.key")"
refute "launch: no key in argv (base64)" "$(cat "$T/fly/log")" "$(base64 < "$NETDIR/t-a/v0/validator.key" | tr -d '\n')"
# The spec's downtime window: half an epoch away jails (600 of 1,200 leader slots for four
# validators), set by DOWNTIME_* (never a second line for the same key from EXTRA_PARAMS).
spec=$(cat "$NETDIR/t-a/spec.toml")
expect "launch: the spec's downtime window takes half an epoch away for four validators" "$spec" "downtime_window = 1200"
expect "launch: … of which it may miss half" "$spec" "downtime_max_missed = 600"
code=$(launch GENESIS_ONLY=1 CHAIN_ID=t-l REGIONS="iad sjc fra ams lhr sin nrt" DOWNTIME_WINDOW=900)
spec=$(cat "$NETDIR/t-l-rehearsal/spec.toml" 2>/dev/null || true)
[ "$code" = 0 ] && expect "launch: seven validators may miss fewer slots (300)" "$spec" "downtime_max_missed = 300" \
  && expect "launch: DOWNTIME_WINDOW sets the window" "$spec" "downtime_window = 900" || bad "launch: downtime window for seven (exit $code): $(cat "$T/launch.out")"
code=$(launch GENESIS_ONLY=1 CHAIN_ID=t-m EXTRA_PARAMS='jail_blocks = 20\ndowntime_window = 40\n')
[ "$code" != 0 ] && [ ! -e "$NETDIR/t-m-rehearsal" ] && expect "launch: EXTRA_PARAMS can't set a downtime parameter again" "$(cat "$T/launch.out")" "use DOWNTIME_WINDOW and DOWNTIME_MAX_MISSED" \
  || bad "launch: EXTRA_PARAMS with a downtime parameter (exit $code): $(cat "$T/launch.out")"
code=$(launch RESET=1 CHAIN_ID=t-a)
[ "$code" != 0 ] && expect "launch: RESET refused once launched" "$(cat "$T/launch.out")" "never reused" || bad "launch: RESET of a launched chain exited $code"
code=$(launch CHAIN_ID=t-a)
[ "$code" = 0 ] && expect "launch: re-running a launched chain redeploys it (keys stay off Fly)" "$(cat "$T/fly/deploys")" "blockula-v0 key=no" || bad "launch: upgrade exited $code: $(cat "$T/launch.out")"

# A fly whose `secrets list` doesn't show staged secrets: the keys are still removed once deployed.
code=$(launch RESET=1 CHAIN_ID=t-e FAKE_FLY_HIDE_STAGED=1)
[ "$code" = 0 ] && ! has blockula-v0 VALIDATOR_KEY_B64 && ! has blockula-v3 VALIDATOR_KEY_B64 \
  && ok "launch: keys removed even when staged secrets aren't listed before the deploy" || bad "launch: hidden staged secrets exited $code: $(cat "$T/launch.out")"

# A validator deploy fails: the launch stops (no public node), and the chain counts as launched.
code=$(launch RESET=1 CHAIN_ID=t-b FAKE_FLY_DEPLOY_FAIL=blockula-v2)
[ "$code" != 0 ] && expect "launch: a failed validator deploy stops the launch" "$(cat "$T/launch.out")" "deploying blockula-v2 failed" || bad "launch: failed deploy exited $code: $(cat "$T/launch.out")"
refute "launch: no public node after a failed validator deploy" "$(cat "$T/fly/deploys")" "blockula-testnet"
code=$(launch RESET=1 CHAIN_ID=t-b)
[ -f "$NETDIR/t-b/launched" ] && [ "$code" != 0 ] && ok "launch: once validators were deployed, RESET is refused (their genesis may have signed blocks)" || bad "launch: RESET after a partial launch exited $code"

# Before the key removal, the public node and every seeded validator must answer for this genesis,
# or nothing restarts and the keys stay in Fly (for a rerun).
code=$(launch RESET=1 CHAIN_ID=t-f FAKE_LOCAL_DOWN=blockula-v1 GATE_ROUNDS=3)
[ "$code" != 0 ] && has blockula-v0 VALIDATOR_KEY_B64 && has blockula-v3 VALIDATOR_KEY_B64 && ! grep -q '(secrets)' "$T/fly/world/log" \
  && expect "launch: a validator that doesn't answer: nothing restarts, the keys stay" "$(cat "$T/launch.out")" "blockula-v1 doesn't answer" || bad "launch: a silent validator (exit $code): $(cat "$T/launch.out")"
code=$(launch RESET=1 CHAIN_ID=t-g FAKE_PUBLIC_DOWN=1 GATE_ROUNDS=3)
[ "$code" != 0 ] && has blockula-v0 VALIDATOR_KEY_B64 && has blockula-v3 VALIDATOR_KEY_B64 \
  && expect "launch: a public node that doesn't answer: nothing restarts, the keys stay" "$(cat "$T/launch.out")" "the public node (https://blockula-testnet.fly.dev) doesn't answer" || bad "launch: a silent public node (exit $code): $(cat "$T/launch.out")"
code=$(launch RESET=1 CHAIN_ID=t-i FAKE_GENESIS="$(printf 'cd%.0s' $(seq 32))")
[ "$code" != 0 ] && has blockula-v0 VALIDATOR_KEY_B64 && ! grep -q 'blockula status' "$T/fly/log" \
  && expect "launch: a public node on another genesis stops it at once" "$(cat "$T/launch.out")" "serves another genesis" || bad "launch: another genesis (exit $code): $(cat "$T/launch.out")"
code=$(launch RESET=1 CHAIN_ID=t-j FAKE_NO_START=blockula-v1 GATE_ROUNDS=3)
[ "$code" != 0 ] && has blockula-v0 VALIDATOR_KEY_B64 && has blockula-v1 VALIDATOR_KEY_B64 && ! grep -q '(secrets)' "$T/fly/world/log" \
  && expect "launch: a validator without a process start time (nothing to tell its new process by): nothing restarts, the keys stay" "$(cat "$T/launch.out")" "reports no process start time" || bad "launch: no process start time (exit $code): $(cat "$T/launch.out")"
# A validator whose volume doesn't show its key seeded keeps the key in Fly and isn't restarted.
code=$(launch RESET=1 CHAIN_ID=t-k FAKE_UNSEEDED=blockula-v2)
[ "$code" = 0 ] && has blockula-v2 VALIDATOR_KEY_B64 && ! has blockula-v1 VALIDATOR_KEY_B64 \
  && [ "$(sed -n 's/^restart \([^ ]*\) (secrets).*/\1/p' "$T/fly/world/log" | sort | tr '\n' ' ')" = "blockula-v0 blockula-v1 blockula-v3 " ] \
  && expect "launch: a key not confirmed on its volume stays in Fly, and its validator isn't restarted" "$(cat "$T/launch.out")" "blockula-v2: couldn't confirm the key was seeded" || bad "launch: an unseeded volume (exit $code): $(cat "$T/launch.out")"
# After it, the launch fails unless every validator comes back: a stalled chain, a validator that
# never returns (public progress can't hide it). The keys are off Fly by then (on the volumes).
code=$(launch RESET=1 CHAIN_ID=t-c FAKE_STALL=1 GATE_ROUNDS=4)
[ "$code" != 0 ] && ! has blockula-v0 VALIDATOR_KEY_B64 \
  && expect "launch: a chain that doesn't resume after the key removal fails the launch" "$(cat "$T/launch.out")" "aren't back at full participation" || bad "launch: stall exited $code: $(cat "$T/launch.out")"
code=$(launch RESET=1 CHAIN_ID=t-h FAKE_DOWN_AFTER_RESTART=blockula-v2 GATE_ROUNDS=4)
[ "$code" != 0 ] && expect "launch: a validator that doesn't come back fails the launch, by name" "$(cat "$T/launch.out")" "blockula-v2 doesn't answer" || bad "launch: a validator that never returns (exit $code): $(cat "$T/launch.out")"

# A genesis an earlier attempt built two days ago isn't launched as it is.
code=$(launch RESET=1 CHAIN_ID=t-d FAKE_FLY_BUILD_FAIL=1)
g=$NETDIR/t-d/genesis.json
jq --argjson t "$((($(date +%s) - 2 * 86400) * 1000))" '.created_at_ms = $t' "$g" > "$T/g" && mv "$T/g" "$g"
code=$(launch RESET=1 CHAIN_ID=t-d)
[ "$code" != 0 ] && expect "launch: refuses an unlaunched genesis built days ago" "$(cat "$T/launch.out")" "was built 48 h ago" || bad "launch: old genesis exited $code: $(cat "$T/launch.out")"
[ ! -f "$NETDIR/t-d/launched" ] && [ ! -s "$T/fly/deploys" ] && ok "launch: and deploys nothing" || bad "launch: old genesis deployed"
code=$(launch RESET=1 CHAIN_ID=t-d GENESIS_MAX_AGE_HOURS=72)
[ "$code" = 0 ] && ok "launch: GENESIS_MAX_AGE_HOURS launches it anyway" || bad "launch: old genesis with override exited $code: $(cat "$T/launch.out")"

# set-github-token.sh: one app at a time, each back (signing, linked, proposing, full pace) before
# the next; a read that fails never counts as passing.
token() {
  : > "$T/fly/log"; rm -f "$T/fly/"*.secrets "$T/fly/"*.staged; world init
  printf 'github_pat_abc123\n' | env PATH="$T/bin:$PATH" FAKE_FLY="$T/fly" PROGRESS_SECS=0 GATE_ROUNDS=20 "$@" bash deploy/set-github-token.sh > "$T/token.out" 2>&1 && echo 0 || echo $?
}
none_set() { local a; for a in blockula-v0 blockula-v1 blockula-v2 blockula-v3 blockula-testnet; do ! has "$a" GITHUB_TOKEN || return 1; done; }
first_only() { has blockula-v0 GITHUB_TOKEN && ! has blockula-v1 GITHUB_TOKEN && ! has blockula-testnet GITHUB_TOKEN; }
code=$(token)
[ "$code" = 0 ] && has blockula-v3 GITHUB_TOKEN && has blockula-testnet GITHUB_TOKEN && ok "token: set on every app" || bad "token: exited $code: $(cat "$T/token.out")"
refute "token: never in argv" "$(cat "$T/fly/log")" "github_pat_"
expect "token: each validator is read on its own machine, over loopback" "$(cat "$T/fly/log")" "ssh console -a blockula-v0 -C blockula status --rpc http://127.0.0.1:7070"
expect "token: and so is the public node" "$(cat "$T/fly/log")" "ssh console -a blockula-testnet -C blockula status --rpc http://127.0.0.1:7171"
[ "$(sed -n 's/^restart \([^ ]*\) (secrets).*/\1/p' "$T/fly/world/log" | tr '\n' ' ')" = "blockula-v0 blockula-v1 blockula-v2 blockula-v3 blockula-testnet " ] \
  && ok "token: one app at a time, the public node last" || bad "token: restarts: $(wlog | grep '^restart')"
[ "$(early)" = 0 ] && ok "token: no app restarts while a validator isn't back" || bad "token: restarted early: $(wlog | grep 'while not back')"
# The public node, last, is back on the reads that show its new process caught up on its machine
# and then a fresh head at RPC_URL: the run ends on them.
rpc_back() {
  [ "$(grep '^status blockula-testnet ' "$T/fly/world/log" | tail -n 1)" = "status blockula-testnet process=new syncing=false" ] \
    && [ "$(tail -n 1 "$T/fly/world/log")" = "pub-status age=500" ]
}
rpc_back && ok "token: done once the public node's new process serves a fresh head" || bad "token: the public node's last reads: $(grep 'testnet\|pub-status' "$T/fly/world/log" | tail -n 3)"
# The gate waits for each part of being back, and only then goes on.
code=$(token FAKE_ABSTAIN_READS=3)
[ "$code" = 0 ] && [ "$(early)" = 0 ] && expect "token: waits while the restarted validator abstains (slashing protection watching)" "$(wlog)" "status blockula-v0 abstaining=true" \
  || bad "token: abstaining (exit $code, $(early) early): $(cat "$T/token.out")"
code=$(token FAKE_UNLINKED=blockula-v2 FAKE_UNLINKED_READS=3)
[ "$code" = 0 ] && [ "$(early)" = 0 ] && expect "token: waits while a validator that stayed up lacks a p2p link" "$(wlog)" "status blockula-v2 abstaining=false peers=2" \
  || bad "token: unlinked (exit $code, $(early) early): $(cat "$T/token.out")"
# The tracker also links to candidates (and the last epoch's set, bootstrappers): only links to
# the active set count.
code=$(token FAKE_UNLINKED=blockula-v2 FAKE_UNLINKED_READS=3 FAKE_CANDIDATE_LINKS=1)
[ "$code" = 0 ] && [ "$(early)" = 0 ] && expect "token: a link to a candidate doesn't stand in for a missing link to the active set" "$(cat "$T/token.out")" "blockula-v2 is linked to 2 of the 3 other active validators" \
  || bad "token: a candidate's link (exit $code, $(early) early): $(cat "$T/token.out")"
code=$(token FAKE_PROPOSE_AFTER=4)
[ "$code" = 0 ] && [ "$(early)" = 0 ] && expect "token: waits until the restarted validator has proposed a block" "$(cat "$T/token.out")" "has not proposed a block since its restart" \
  || bad "token: no proposal yet (exit $code, $(early) early): $(cat "$T/token.out")"
code=$(token FAKE_SLOW_BLOCKS=8)
[ "$code" = 0 ] && [ "$(early)" = 0 ] && expect "token: waits until the chain is back at full pace" "$(cat "$T/token.out")" "not full pace" \
  || bad "token: crawl (exit $code, $(early) early): $(cat "$T/token.out")"
code=$(token FAKE_ABSTAINING=blockula-v3 FAKE_ABSTAINING_READS=3)
[ "$code" = 0 ] && [ "$(early)" = 0 ] && expect "token: waits while a validator no script restarted abstains (it restarted on its own)" "$(wlog)" "status blockula-v3 abstaining=true" \
  || bad "token: another validator abstaining (exit $code, $(early) early): $(cat "$T/token.out")"
code=$(token FAKE_RESTART_LAG=3)
[ "$code" = 0 ] && [ "$(early)" = 0 ] && expect "token: waits for the restarted validator's new process (the old one still answers, and proposes)" "$(cat "$T/token.out")" "still runs the process it ran before the restart" \
  || bad "token: restart lag (exit $code, $(early) early): $(cat "$T/token.out")"
code=$(token FAKE_SYNCING=blockula-v3 FAKE_SYNCING_READS=3)
[ "$code" = 0 ] && [ "$(early)" = 0 ] && expect "token: waits while a validator catches up (linked, its slashing protection open, but behind the chain)" "$(cat "$T/token.out")" "blockula-v3 is catching up" \
  || bad "token: catching up (exit $code, $(early) early): $(cat "$T/token.out")"
for slow in lag syncing down stale; do
  case $slow in
    lag) waits="blockula-testnet still runs the process it ran before the restart" ;;
    syncing) waits="blockula-testnet is catching up" ;;
    down) waits="the public node (https://blockula-testnet.fly.dev) doesn't answer" ;;
    stale) waits="the newest block on the public node is 60 s old" ;;
  esac
  code=$(token FAKE_RPC_SLOW=$slow FAKE_RPC_SLOW_READS=3)
  [ "$code" = 0 ] && rpc_back && expect "token: waits for the public node after its restart ($slow)" "$(cat "$T/token.out")" "$waits" \
    || bad "token: the public node $slow after its restart (exit $code): $(cat "$T/token.out")"
done
# It stops (fail closed), leaving the apps it hasn't restarted untouched.
code=$(token FAKE_STALL=1 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && ok "token: a stalled chain: nothing restarts" || bad "token: stall exited $code: $(cat "$T/token.out")"
code=$(token FAKE_OUTSIDER=1 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: an active set with a validator it doesn't run: nothing restarts, at once" "$(cat "$T/token.out")" "the active set has 5 validators, more than the 4 this script runs" \
  || bad "token: an outside validator in the set (exit $code): $(cat "$T/token.out")"
# Validators other operators run, named in OUTSIDE_VALIDATORS:
# the gate can't read their machines, so each must have proposed in the window it reads.
code=$(token FAKE_OUTSIDER=1 FAKE_OUTSIDER_PROPOSES=1 OUTSIDE_VALIDATORS=tboo1outsider)
[ "$code" = 0 ] && [ "$(early)" = 0 ] && expect "token: an outside validator OUTSIDE_VALIDATORS names, proposing: every app restarts, one at a time" "$(cat "$T/token.out")" "outside validators that proposed in them: tboo1outsider" \
  || bad "token: a named outside validator (exit $code, $(early) early): $(cat "$T/token.out")"
code=$(token FAKE_OUTSIDER=1 FAKE_OUTSIDER_PROPOSES=1 FAKE_OUTSIDER_DOWN=1 OUTSIDE_VALIDATORS=tboo1outsider GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a named outside validator that proposes nothing: nothing restarts" "$(cat "$T/token.out")" "outside validator tboo1outsider proposed none of the last 20 blocks" \
  || bad "token: a silent outside validator (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_OUTSIDER=1 FAKE_OUTSIDER_PROPOSES=1 OUTSIDE_VALIDATORS=tboo1outsider PACE_BLOCKS=8 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a window too short to see every validator lead: nothing restarts, at once" "$(cat "$T/token.out")" "PACE_BLOCKS (8) is fewer blocks than it takes each of 5 validators to lead (9)" \
  && refute "token: … without waiting" "$(cat "$T/token.out")" "   … " || bad "token: a short window with outside validators (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_ACTIVE_UNLISTED=1 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a public node that doesn't say how many validators are active never passes" "$(cat "$T/token.out")" "doesn't list the active set" \
  || bad "token: no active count (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_SYNCING_NULL=blockula-v1 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a validator that doesn't say it isn't catching up never passes" "$(cat "$T/token.out")" "blockula-v1 is catching up" \
  || bad "token: syncing null (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_STALL_AFTER_RESTART=1 GATE_ROUNDS=4)
[ "$code" != 0 ] && first_only && ok "token: the chain stalls after a restart: stops before the next app" || bad "token: stall after a restart exited $code: $(cat "$T/token.out")"
code=$(token FAKE_DOWN_AFTER_RESTART=blockula-v0 GATE_ROUNDS=4)
[ "$code" != 0 ] && first_only && expect "token: public progress cannot hide a restarted validator that never returns" "$(cat "$T/token.out")" "blockula-v0 doesn't answer" \
  || bad "token: continued after a validator failed to return (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_LOCAL_DOWN=blockula-v2 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a validator that doesn't answer before anything restarts: nothing restarts" "$(cat "$T/token.out")" "blockula-v2 doesn't answer" \
  || bad "token: a silent validator (exit $code): $(cat "$T/token.out")"
# The public node restarts only once it answers on its machine with its process start time (what
# tells its new process from the old one); the validators keep the token, and it stops there.
validators_only() { local a; for a in blockula-v0 blockula-v1 blockula-v2 blockula-v3; do has "$a" GITHUB_TOKEN || return 1; done; ! has blockula-testnet GITHUB_TOKEN; }
code=$(token FAKE_LOCAL_DOWN=blockula-testnet GATE_ROUNDS=3)
[ "$code" != 0 ] && validators_only && expect "token: a public node that doesn't answer on its machine isn't restarted" "$(cat "$T/token.out")" "blockula-testnet doesn't answer" \
  || bad "token: a silent public node machine (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_NO_START=blockula-testnet GATE_ROUNDS=3)
[ "$code" != 0 ] && validators_only && expect "token: nor one that reports no process start time" "$(cat "$T/token.out")" "reports no process start time" \
  || bad "token: a public node without a process start time (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_FEW_BLOCKS=5 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a public node holding fewer blocks than the pace is measured over never passes" "$(cat "$T/token.out")" "holds fewer than 20 blocks" \
  || bad "token: fewer blocks than the pace window (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_BLOCKS_DOWN=1 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a public node whose /api/blocks fails never passes" "$(cat "$T/token.out")" "doesn't answer /api/blocks" \
  || bad "token: /api/blocks failing (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_PUBLIC_DOWN_AFTER=2 GATE_ROUNDS=4)
[ "$code" != 0 ] && first_only && expect "token: the public node stops answering: stops before the next app" "$(cat "$T/token.out")" "the public node (https://blockula-testnet.fly.dev) doesn't answer" \
  || bad "token: a silent public node (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_OUT_OF_SET=blockula-v1 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a validator out of the active set (jailed): nothing restarts" "$(cat "$T/token.out")" "blockula-v1 is not in the active set" \
  || bad "token: a jailed validator (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_NOT_VOTING=blockula-v2 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a validator whose node doesn't vote: nothing restarts" "$(cat "$T/token.out")" "blockula-v2 is not voting" \
  || bad "token: a validator not voting (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_NO_VALIDATOR=blockula-v2 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: a node with no validator of its own (its key not on the chain it follows): nothing restarts" "$(cat "$T/token.out")" "blockula-v2 reports no validator of its own" \
  || bad "token: no validator of its own (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_JAILED=blockula-v1)
[ "$code" != 0 ] && none_set && expect "token: a jailed validator: nothing restarts" "$(cat "$T/token.out")" "blockula-v1 is jailed" \
  && refute "token: and it stops at once (waiting can't unjail it)" "$(cat "$T/token.out")" "   … " || bad "token: a jailed validator (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_NO_TIMES=1 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && expect "token: blocks whose times can't be read never pass" "$(cat "$T/token.out")" "no times the gate can read" \
  || bad "token: blocks without times (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_PUBLIC_EMPTY=1 GATE_ROUNDS=3)
[ "$code" != 0 ] && none_set && ok "token: a public node at height 0 (no blocks) never passes" || bad "token: an empty public node (exit $code): $(cat "$T/token.out")"
# The old gate's `before=0`: a public node that didn't answer made the target #6, which any node
# meets. Now a failed read never counts: with every other read failing and the restarted validator
# never signing again, it stops after that validator.
code=$(token FAKE_PUBLIC_FAIL_EVERY=2 FAKE_ABSTAIN_READS=100000 GATE_ROUNDS=6)
[ "$code" != 0 ] && first_only && [ "$(early)" = 0 ] && ok "token: a flaky public node never lets the gate pass" || bad "token: flaky public node (exit $code): $(cat "$T/token.out")"
for f in deploy/launch.sh deploy/set-github-token.sh deploy/restart-gate.sh; do refute "$f: no failed read turns into height 0" "$(cat "$f")" "|| echo 0"; done
code=$(token FAKE_NO_STATUS=1)
[ "$code" != 0 ] && none_set && expect "token: a deployed blockula without \`status\`: nothing restarts, and it says why" "$(cat "$T/token.out")" "older than this script" \
  || bad "token: no status command (exit $code): $(cat "$T/token.out")"
code=$(token FAKE_OTHER_GENESIS=blockula-v2)
[ "$code" != 0 ] && none_set && expect "token: a validator on another genesis: nothing restarts" "$(cat "$T/token.out")" "serves another genesis" \
  || bad "token: another genesis (exit $code): $(cat "$T/token.out")"

if [ $fails -gt 0 ]; then echo "✗ $fails failed" >&2; exit 1; fi
echo "✓ deploy scripts behave"
