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
expect() { # what haystack needle
  if printf '%s\n' "$2" | grep -qF -- "$3"; then ok "$1"; else bad "$1: no \"$3\" in: $2"; fi
}
refute() { # what haystack needle
  if printf '%s\n' "$2" | grep -qF -- "$3"; then bad "$1: \"$3\" in: $2"; else ok "$1"; fi
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
(env -i PATH="$PATH" BLOCKULA_DATA="$T/v4" BLOCKULA_ETC="$T/etc" BLOCKULA_BIN="$T/blockula" STUB_LOG="$T/log" STUB_EXIT=10 ROLE=validator ${ENTRY_SH:-sh} deploy/entrypoint.sh > "$T/out10" 2>&1) &
idle=$!
sleep 2
if kill -0 "$idle" 2>/dev/null; then
  kill -TERM "$idle"; wait "$idle" && ok "validator: exit 10 (halt height) idles instead of exiting, and stops cleanly" || bad "validator: idle exited $? on TERM"
else
  bad "validator: exit 10 didn't idle: $(cat "$T/out10")"
fi
expect "validator: says why it stays down" "$(cat "$T/out10")" "staying down"

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
expect "rpc: ARCHIVE=1 keeps everything" "$(log)" "--archive"
refute "rpc: ARCHIVE wins over PRUNE_BLOCKS" "$(log)" "--prune-blocks"
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
RPC_ARCHIVE=1
r=$(rpc_toml)
expect "rpc config: RPC_ARCHIVE=1" "$r" 'ARCHIVE = "1"'
refute "rpc config: an archive has no window" "$r" "PRUNE_BLOCKS"
PY=${PYTHON:-python3}
if "$PY" -c "import tomllib" 2>/dev/null; then
  for cfg in "$v0" "$v1" "$r"; do
    printf '%s\n' "$cfg" | "$PY" -c "import sys, tomllib; tomllib.loads(sys.stdin.read())" && ok "config parses as TOML" || bad "config isn't TOML: $cfg"
  done
else
  echo "(python3 has no tomllib here: TOML syntax not checked)"
fi

# ---- launch.sh and set-github-token.sh, against a stand-in fly ----

# The stand-in fly keeps each app's secret names in $FAKE_FLY/<app>.secrets (never values),
# staged ones in <app>.staged until the app's next deploy (FAKE_FLY_HIDE_STAGED: `secrets list`
# doesn't show them), and
# logs every call; FAKE_FLY_BUILD_FAIL fails the image build, FAKE_FLY_DEPLOY_FAIL=<app> that
# app's deploy. Each deploy records whether the app held VALIDATOR_KEY_B64 once it applied them.
mkdir -p "$T/bin" "$T/fly"
cat > "$T/bin/fly" <<'STUB'
#!/usr/bin/env bash
S=$FAKE_FLY
echo "$*" >> "$S/log"
app="" cfg=""
args=("$@")
for ((k = 0; k < ${#args[@]}; k++)); do
  case ${args[$k]} in -a) app=${args[$((k + 1))]} ;; -c) cfg=${args[$((k + 1))]} ;; esac
done
secrets=$S/$app.secrets
case "$1 ${2:-}" in
  "auth whoami") echo tester ;;
  "apps list") echo '[]' ;;
  "machines list") echo "[{\"id\":\"m-$app\"}]" ;;
  "volumes list") echo '[]' ;;
  "ips list") echo '[{"Type":"v4","Address":"203.0.113.7"}]' ;;
  "secrets list")
    touch "$secrets" "$S/$app.staged"
    { cat "$secrets"; [ -n "${FAKE_FLY_HIDE_STAGED:-}" ] || cat "$S/$app.staged"; } | jq -Rn '[inputs | select(length > 0) | {name: .}]' ;;
  "secrets import")
    to=$secrets; printf '%s\n' "$*" | grep -q -- --stage && to=$S/$app.staged
    touch "$to"; while IFS='=' read -r k _; do [ -z "$k" ] || grep -qx "$k" "$to" || echo "$k" >> "$to"; done ;;
  "secrets unset") for f in "$secrets" "$S/$app.staged"; do touch "$f"; { grep -vx "$3" "$f" || true; } > "$f.tmp"; mv "$f.tmp" "$f"; done ;;
  "ssh console")
    if [[ "$*" = *"checkpoint --peer"* && "${FAKE_LOCAL_DOWN:-}" = "$app" ]]; then exit 1; fi ;;
  deploy*)
    if printf '%s\n' "$*" | grep -q -- --build-only; then [ -z "${FAKE_FLY_BUILD_FAIL:-}" ] || exit 1; exit 0; fi
    app=$(sed -nE 's/^app = "(.*)"/\1/p' "$cfg")
    touch "$S/$app.secrets" "$S/$app.staged"
    sort -u "$S/$app.secrets" "$S/$app.staged" > "$S/$app.tmp"; mv "$S/$app.tmp" "$S/$app.secrets"; : > "$S/$app.staged"
    echo "$app key=$(grep -qx VALIDATOR_KEY_B64 "$S/$app.secrets" && echo yes || echo no)" >> "$S/deploys"
    [ "${FAKE_FLY_DEPLOY_FAIL:-}" != "$app" ] || exit 1 ;;
esac
exit 0
STUB
# The stand-in curl: /api/status rises 3 blocks a call (FAKE_STALL: stays put); GitHub accepts.
cat > "$T/bin/curl" <<'STUB'
#!/usr/bin/env bash
for a; do url=$a; done
case $url in
  */api/status) h=$(cat "$FAKE_FLY/height" 2>/dev/null || echo 100); [ -n "${FAKE_STALL:-}" ] || h=$((h + 3)); echo "$h" > "$FAKE_FLY/height"; echo "{\"height\":$h,\"genesis_hash\":\"abababababababababababababababababababababababababababababababab\"}" ;;
  */api/health) echo '{}' ;;
  https://api.github.com/*) cat > /dev/null; printf 'HTTP/2 200 \r\ncontent-type: application/json\r\n\r\n' ;;
  *) exit 7 ;;
esac
STUB
# A stand-in blockula for the genesis steps: keys, operator files, a genesis with the spec's clock.
cat > "$T/bin/blockula-launch" <<'STUB'
#!/usr/bin/env bash
case "$1 ${2:-}" in
  "keygen "*) echo "{\"address\":\"tboo1$(openssl rand -hex 8)\",\"seed\":\"$(openssl rand -hex 32)\"}" ;;
  "genesis operator") while [ $# -gt 0 ]; do [ "$1" = --out ] && echo "# operator" > "$2"; shift; done ;;
  "genesis build")
    spec=$4 out=$6
    printf '{"chain_id":"%s","created_at_ms":%s}\n' "$(sed -nE 's/^chain_id = "(.*)"/\1/p' "$spec")" "$(sed -nE 's/^genesis_time_ms = //p' "$spec")" > "$out" ;;
  "checkpoint --genesis") { sha256sum "$3" 2>/dev/null || shasum -a 256 "$3"; } | cut -c1-64 ;;
esac
STUB
chmod +x "$T/bin/fly" "$T/bin/curl" "$T/bin/blockula-launch"
# launch [VAR=value …]: runs launch.sh (a fresh log), prints its exit code.
launch() {
  : > "$T/fly/log"; : > "$T/fly/deploys"
  env PATH="$T/bin:$PATH" HOME="$T/home" FAKE_FLY="$T/fly" BIN="$T/bin/blockula-launch" SKIP_BUILD=1 PROGRESS_SECS=0 "$@" \
    bash deploy/launch.sh > "$T/launch.out" 2>&1 && echo 0 || echo $?
}
has() { grep -qx "$2" "$T/fly/$1.secrets" 2>/dev/null; }
NETDIR=$T/home/.config/blockula

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

# The chain makes no progress after the first key removal: the others keep their keys.
code=$(launch RESET=1 CHAIN_ID=t-c FAKE_STALL=1)
[ "$code" != 0 ] && expect "launch: no new blocks stops the rolling key removal" "$(cat "$T/launch.out")" "stopping before the next validator" || bad "launch: stall exited $code: $(cat "$T/launch.out")"
! has blockula-v0 VALIDATOR_KEY_B64 && has blockula-v1 VALIDATOR_KEY_B64 && has blockula-v3 VALIDATOR_KEY_B64 \
  && ok "launch: only the first validator restarted" || bad "launch: keys after a stall: $(cat "$T/fly/"*.secrets)"

# Three validators still finalize when the restarted fourth never returns. Public progress alone
# must not authorize restarting another validator and taking the network below quorum.
code=$(launch RESET=1 CHAIN_ID=t-f FAKE_LOCAL_DOWN=blockula-v0)
[ "$code" != 0 ] && has blockula-v1 VALIDATOR_KEY_B64 && has blockula-v3 VALIDATOR_KEY_B64 \
  && ok "launch: public progress cannot hide a restarted validator that never returns" || bad "launch: continued after a validator failed to return (exit $code)"

# A genesis an earlier attempt built two days ago isn't launched as it is.
code=$(launch RESET=1 CHAIN_ID=t-d FAKE_FLY_BUILD_FAIL=1)
g=$NETDIR/t-d/genesis.json
jq --argjson t "$((($(date +%s) - 2 * 86400) * 1000))" '.created_at_ms = $t' "$g" > "$T/g" && mv "$T/g" "$g"
code=$(launch RESET=1 CHAIN_ID=t-d)
[ "$code" != 0 ] && expect "launch: refuses an unlaunched genesis built days ago" "$(cat "$T/launch.out")" "was built 48 h ago" || bad "launch: old genesis exited $code: $(cat "$T/launch.out")"
[ ! -f "$NETDIR/t-d/launched" ] && [ ! -s "$T/fly/deploys" ] && ok "launch: and deploys nothing" || bad "launch: old genesis deployed"
code=$(launch RESET=1 CHAIN_ID=t-d GENESIS_MAX_AGE_HOURS=72)
[ "$code" = 0 ] && ok "launch: GENESIS_MAX_AGE_HOURS launches it anyway" || bad "launch: old genesis with override exited $code: $(cat "$T/launch.out")"

# set-github-token.sh: one app at a time, stopping when the chain stalls.
token() { : > "$T/fly/log"; printf 'github_pat_abc123\n' | env PATH="$T/bin:$PATH" FAKE_FLY="$T/fly" PROGRESS_SECS=0 "$@" bash deploy/set-github-token.sh > "$T/token.out" 2>&1 && echo 0 || echo $?; }
rm -f "$T/fly/"*.secrets "$T/fly/"*.staged
code=$(token)
[ "$code" = 0 ] && has blockula-v3 GITHUB_TOKEN && has blockula-testnet GITHUB_TOKEN && ok "token: set on every app" || bad "token: exited $code: $(cat "$T/token.out")"
refute "token: never in argv" "$(cat "$T/fly/log")" "github_pat_"
expect "token: the validator confirms its own finalized height" "$(cat "$T/fly/log")" "-a blockula-v0 -C blockula checkpoint --peer http://127.0.0.1:7070 --height"
expect "token: the public node confirms its own finalized height" "$(cat "$T/fly/log")" "-a blockula-testnet -C blockula checkpoint --peer http://127.0.0.1:7171 --height"
expect "token: the check is pinned to this genesis" "$(cat "$T/fly/log")" "--genesis-hash abababababababababababababababababababababababababababababababab"
rm -f "$T/fly/"*.secrets "$T/fly/"*.staged
code=$(token FAKE_STALL=1)
[ "$code" != 0 ] && has blockula-v0 GITHUB_TOKEN && ! has blockula-v1 GITHUB_TOKEN && ok "token: no new blocks stops before the next app" || bad "token: stall exited $code: $(cat "$T/token.out")"
rm -f "$T/fly/"*.secrets "$T/fly/"*.staged
code=$(token FAKE_LOCAL_DOWN=blockula-v0)
[ "$code" != 0 ] && has blockula-v0 GITHUB_TOKEN && ! has blockula-v1 GITHUB_TOKEN \
  && ok "token: public progress cannot hide a restarted validator that never returns" || bad "token: continued after a validator failed to return (exit $code)"

if [ $fails -gt 0 ]; then echo "✗ $fails failed" >&2; exit 1; fi
echo "✓ deploy scripts behave"
