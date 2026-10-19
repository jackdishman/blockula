#!/usr/bin/env bash
# 🧛 Launches (or upgrades) a protocol v1 public testnet on Fly.io. Safe to re-run.
#   N validator apps: own dedicated IPv4 (p2p only) + volume each (scheduled snapshots off: a
#     restored validator volume can double-sign), spread across regions
#   1 public full node app with HTTPS at https://$RPC_APP.fly.dev: the API, the explorer, and the
#     sync source for anyone joining. It reaches validators over Fly's private network.
# The genesis is built with the genesis builder (`blockula genesis operator` + `genesis build`,
# docs/GENESIS_CEREMONY.md) from a spec this script writes once: the DEC-5 allocation with `tboo`
# addresses, one operator per validator with its own owner account (which holds the self-bond and
# pays the validator's system fees), no admin keys, a faucet funded on the validators'
# consensus-key accounts, and the contribution pool open from genesis (its repos, an opening
# balance out of the endowment, founding jurors' Soul). Keys are generated once on this laptop
# under ~/.config/blockula/$CHAIN_ID (mode 700): consensus keys are uploaded only to their
# validator, seeded onto its volume on first boot, then deleted from Fly; owner, founder, grant
# and stand-in juror keys never leave this machine. (A public testnet is still one
# operator's machines; independent operators come with the incentivized testnet, OPS-1.)
#
#   deploy/launch.sh                  launch, or upgrade the nodes of, $CHAIN_ID
#   GENESIS_ONLY=1 deploy/launch.sh   rehearsal: only build keys, operator files, spec and genesis
#                                     under ~/.config/blockula/$CHAIN_ID-rehearsal (placeholder IPs
#                                     unless IPS="ip ip …", or P2P_ADDRS="host:port …"); no Fly calls
#   RESET=1 deploy/launch.sh          wipe the apps' machines, volumes and secrets (another chain's
#                                     data) before launching $CHAIN_ID. Refused once $CHAIN_ID has
#                                     launched (its validators were deployed, even if the launch
#                                     then failed): a chain id is never reused, pick a new one
set -euo pipefail
cd "$(dirname "$0")/.."

ORG=${ORG:-personal}
PREFIX=${PREFIX:-blockula}
RPC_APP=${RPC_APP:-$PREFIX-testnet}
# Never reuse a chain id (MAINNET-DESIGN DEC-7): `blockula-testnet` was the v0.4 chain.
CHAIN_ID=${CHAIN_ID:-blockula-testnet-2}
ADDRESS_PREFIX=${ADDRESS_PREFIX:-tboo}
REGIONS=(${REGIONS:-iad sjc fra ams})   # one validator per region
RPC_REGION=${RPC_REGION:-iad}
EPOCH_BLOCKS=${EPOCH_BLOCKS:-3600}      # 1-hour epochs at 1 s blocks (DEC-12)
DAY=86400                               # blocks a day at 1 s blocks
FAUCET_FANG=${FAUCET_FANG:-2500000}     # per validator's consensus-key account (drips + fees)
SELF_BOND=${SELF_BOND:-10000}           # FANG each owner bonds (the default min_self_bond)
# Unbonding, and the evidence window with it: a week (mainnet: 21 days), so the weak-subjectivity
# period (half the evidence window) is 3.5 days, not hours, and unbonding is still a week's
# exercise. Below governance's 14-day floor on the evidence window, which a vote then can't lower
# further but can raise (params.rs `governance_band`; mainnet's genesis must start inside it).
UNBONDING_BLOCKS=${UNBONDING_BLOCKS:-$((7 * DAY))}
# The contribution pool, open from genesis.
POOL_FANG=${POOL_FANG:-100000}          # its opening balance, out of the endowment
POOL_REPOS=${POOL_REPOS:-"jackdishman/blockula:10"}   # owner/name:weight …
JURORS=${JURORS:-3}                     # stand-in founding jurors made on this laptop …
JUROR_SOUL=${JUROR_SOUL:-25}            # … with this much Soul each (5 sits on a jury, 10 vouches)
# JURORS_FILE: the founding jurors instead of stand-ins, one per line: `tboo1… <soul> <reason>`.
VALIDATOR_VM=${VALIDATOR_VM:-shared-cpu-2x}
VALIDATOR_MEMORY=${VALIDATOR_MEMORY:-2048mb}
RPC_VM=${RPC_VM:-shared-cpu-1x}
RPC_MEMORY=${RPC_MEMORY:-1024mb}
VOLUME_GB=${VOLUME_GB:-10}
RPC_VOLUME_GB=${RPC_VOLUME_GB:-$VOLUME_GB}
# Block windows: validators keep the node's default (~2 days) unless VALIDATOR_PRUNE_BLOCKS is
# set; the public node keeps 14 days (~1.2 GB idle with 4 validators, ~10 GB at a sustained
# 0.4 tx/s: docs/OPERATIONS.md "Disk sizing"), or everything with RPC_ARCHIVE=1 (size
# RPC_VOLUME_GB for it).
VALIDATOR_PRUNE_BLOCKS=${VALIDATOR_PRUNE_BLOCKS:-}
RPC_PRUNE_BLOCKS=${RPC_PRUNE_BLOCKS:-$((14 * DAY))}
RPC_ARCHIVE=${RPC_ARCHIVE:-}
P2P_PORT=30300
HTTP_PORT=7070
RPC_PORT=7171
CONF=$HOME/.config/blockula
# A rehearsal (GENESIS_ONLY, placeholder IPs) never shares a directory with the real launch
# (REHEARSAL_DIR picks another one for it).
NET=$CONF/$CHAIN_ID${GENESIS_ONLY:+-rehearsal}
if [ -n "${GENESIS_ONLY:-}" ] && [ -n "${REHEARSAL_DIR:-}" ]; then NET=$REHEARSAL_DIR; fi
N=${#REGIONS[@]}
RPC_URL=${RPC_URL:-https://$RPC_APP.fly.dev}
APPS=("$RPC_APP"); for i in $(seq 0 $((N - 1))); do APPS+=("$PREFIX-v$i"); done
source deploy/fly-config.sh
# The participation gate a restart waits for (PROGRESS_SECS between its checks, GATE_SECS at most).
source deploy/restart-gate.sh

fly_ok() { [ -z "${GENESIS_ONLY:-}" ]; }
if ! fly_ok && [ -n "${RESET:-}" ]; then echo "❌ RESET=1 with GENESIS_ONLY=1: a rehearsal makes no Fly calls, so it never resets anything" >&2; exit 1; fi
if fly_ok; then fly auth whoami >/dev/null || { echo "Run: fly auth login" >&2; exit 1; }; fi
mkdir -p "$CONF" && chmod 700 "$CONF"
[ -n "${SKIP_BUILD:-}" ] || cargo build --release --locked --manifest-path node/Cargo.toml -q
BIN=${BIN:-node/target/release/blockula}

if [ "${RESET:-}" = 1 ]; then
  [ ! -f "$NET/launched" ] || { echo "❌ $CHAIN_ID has launched already (its validators were deployed and may have signed blocks): a chain id is never reused. Rerun without RESET to finish this launch, or set CHAIN_ID to a new one (and RESET=1 to clear the apps)." >&2; exit 1; }
  echo "♻️  RESET: destroying the apps' machines, volumes and secrets before launching $CHAIN_ID"
  for app in "${APPS[@]}"; do
    fly machines list -a "$app" --json 2>/dev/null | jq -r '.[].id' | while read -r m; do fly machine destroy "$m" -a "$app" --force >/dev/null; done
    fly volumes list -a "$app" --json 2>/dev/null | jq -r '.[].id' | while read -r v; do fly volumes destroy "$v" -a "$app" --yes >/dev/null; done
    fly secrets list -a "$app" --json 2>/dev/null | jq -r '.[].name // .[].Name' | while read -r s; do fly secrets unset "$s" -a "$app" --stage >/dev/null; done
  done
  # Those secrets included any this chain staged on an earlier, failed attempt: stage them again.
  rm -f "$NET/secrets-staged"
fi

ensure_app() { fly apps list --org "$ORG" --json | jq -e --arg a "$1" '.[] | select(.Name == $a)' >/dev/null || fly apps create "$1" --org "$ORG" >/dev/null; }
ensure_volume() { # app region size [validator]
  local snapshots=true
  [ "${4:-}" = validator ] && snapshots=false
  if [ "$(fly volumes list -a "$1" --json | jq length)" -eq 0 ]; then
    fly volumes create data -a "$1" -r "$2" -s "$3" --scheduled-snapshots="$snapshots" --yes >/dev/null
  elif [ "$snapshots" = false ]; then
    # Volumes from before this script turned snapshots off: never keep restorable validator state.
    fly volumes list -a "$1" --json | jq -r '.[].id' | while read -r v; do fly volumes update "$v" -a "$1" --scheduled-snapshots=false >/dev/null; done
  fi
}
dedicated_v4() {
  local ip
  ip=$(fly ips list -a "$1" --json | jq -r '[.[] | select(.Type == "v4")][0].Address // empty')
  [ -n "$ip" ] || { fly ips allocate-v4 -a "$1" --yes >/dev/null; ip=$(fly ips list -a "$1" --json | jq -r '[.[] | select(.Type == "v4")][0].Address'); }
  echo "$ip"
}
has_secret() { fly secrets list -a "$1" --json | jq -e --arg s "$2" 'map(.name // .Name) | index($s)' >/dev/null; }
# Country of a Fly region, for the operator files' independence fields (DEC-14 display only).
country_of() {
  case "$1" in
    iad|ewr|ord|dfw|lax|sjc|sea|atl|bos|den|mia|phx) echo US ;; yyz|yul) echo CA ;; gru|gig) echo BR ;;
    fra) echo DE ;; ams) echo NL ;; lhr) echo GB ;; cdg) echo FR ;; arn) echo SE ;; waw) echo PL ;; mad) echo ES ;; otp) echo RO ;;
    sin) echo SG ;; nrt) echo JP ;; syd) echo AU ;; bom) echo IN ;; jnb) echo ZA ;; hkg) echo HK ;; *) echo "" ;;
  esac
}
toml_str() { local s=${1//\\/\\\\}; printf '"%s"' "${s//\"/\\\"}"; }

P2P=()
if fly_ok; then
  echo "🪦 Apps, volumes and IPs…"
  ensure_app "$RPC_APP"; ensure_volume "$RPC_APP" "$RPC_REGION" "$RPC_VOLUME_GB"
  for i in $(seq 0 $((N - 1))); do
    app=$PREFIX-v$i
    ensure_app "$app"; ensure_volume "$app" "${REGIONS[$i]}" "$VOLUME_GB" validator
    ip=$(dedicated_v4 "$app")
    P2P+=("$ip:$P2P_PORT")
    echo "   $app  ${REGIONS[$i]}  $ip"
  done
elif [ -n "${P2P_ADDRS:-}" ]; then
  read -r -a P2P <<< "$P2P_ADDRS"
  [ "${#P2P[@]}" -eq "$N" ] || { echo "❌ P2P_ADDRS lists ${#P2P[@]} addresses for $N validators" >&2; exit 1; }
else
  read -r -a IPS_ARR <<< "${IPS:-}"
  for i in $(seq ${#IPS_ARR[@]} $((N - 1))); do IPS_ARR+=("203.0.113.$((i + 1))"); done
  for i in $(seq 0 $((N - 1))); do P2P+=("${IPS_ARR[$i]}:$P2P_PORT"); done
fi

if [ ! -f "$NET/genesis.json" ]; then
  echo "🧬 Building the genesis of $CHAIN_ID ($N validators, prefix $ADDRESS_PREFIX)…"
  (umask 077; mkdir -p "$NET/operators")
  key() { [ -f "$1" ] || (umask 077; $BIN keygen --prefix "$ADDRESS_PREFIX" > "$1"); jq -r .address "$1"; }
  FOUNDER=$(key "$NET/founder.json"); GRANTS=$(key "$NET/grants.json")
  ops="" ; allocs=""
  for i in $(seq 0 $((N - 1))); do
    (umask 077; mkdir -p "$NET/v$i")
    CONSENSUS=$(key "$NET/v$i/consensus.json")
    [ -f "$NET/v$i/validator.key" ] || (umask 077; jq -r .seed "$NET/v$i/consensus.json" > "$NET/v$i/validator.key")
    OWNER=$(key "$NET/owner-$i.json")
    # Each validator publishes the public node as its HTTP endpoint: its sentry, as mainnet
    # operators publish theirs (`genesis operator --http-addr`); validators' own HTTP stays on
    # Fly's private network, where the other nodes reach it (PEER_HTTP).
    # No seat is flagged `--founder-or-foundation`: the four stand in for a mainnet genesis's
    # independent operators. The founder's stand-in account is excluded from the bootstrap
    # delegation as on mainnet; flagging the seats too would leave the
    # delegation nobody to back, and DEC-3's cap (fewer such seats than f: none in a set of 4)
    # would keep a seat that left the set out for good (relaunch problem 2; staking test
    # `testnet_seats_leave_rejoin_and_get_the_bootstrap_delegation`).
    $BIN genesis operator --chain-id "$CHAIN_ID" --key "$NET/v$i/validator.key" --owner "$OWNER" \
      --p2p-addr "${P2P[$i]}" --http-addr "$RPC_URL" --commission-bps 500 --self-bond "$SELF_BOND" \
      --name "Blockula testnet validator $i (${REGIONS[$i]})" --legal-entity "Blockula (testnet)" --entity-jurisdiction US \
      --hosting-provider Fly.io --asn 40509 --datacenter-country "$(country_of "${REGIONS[$i]}")" --datacenter "fly-${REGIONS[$i]}" \
      --out "$NET/operators/v$i.toml"
    ops="$ops  \"operators/v$i.toml\",\n"
    allocs="$allocs[[allocation]]\nbucket = \"operators\"\naddress = \"$OWNER\"\namount = \"1000000\"\nlabel = \"genesis operator: validator $i\"\n\n"
    allocs="$allocs[[allocation]]\nbucket = \"faucet\"\naddress = \"$CONSENSUS\"\namount = \"$FAUCET_FANG\"\nlabel = \"faucet: validator $i\"\n\n"
  done
  # Founding jurors: Soul in the genesis ledger (docs/TOKENOMICS.md: never the founder's).
  souls=""
  if [ -n "${JURORS_FILE:-}" ]; then
    [ -f "$JURORS_FILE" ] || { echo "❌ JURORS_FILE=$JURORS_FILE isn't a file" >&2; exit 1; }
    while read -r addr soul reason || [ -n "${addr:-}" ]; do
      case "$addr" in "" | \#*) continue ;; esac
      souls="$souls[[soul]]\naccount = \"$addr\"\nsoul = \"$soul\"\nreason = $(toml_str "${reason:-founding juror}")\n\n"
    done < "$JURORS_FILE"
  else
    for i in $(seq 0 $((JURORS - 1))); do
      souls="$souls[[soul]]\naccount = \"$(key "$NET/juror-$i.json")\"\nsoul = \"$JUROR_SOUL\"\nreason = \"founding juror (testnet stand-in $i)\"\n\n"
    done
  fi
  repos=""
  for r in $POOL_REPOS; do repos="$repos[[contribution.repo]]\nrepo = \"${r%:*}\"\nweight = ${r##*:}\n\n"; done
  GENESIS_MS=$(($(date +%s) * 1000))
  cat > "$NET/spec.toml" <<SPEC
# The $CHAIN_ID genesis spec, written by deploy/launch.sh. Rebuild it (byte-identically) with
#   blockula genesis build --spec spec.toml --out genesis.json
# Founder, grant and juror accounts are testnet stand-ins held by the launcher.
chain_id = "$CHAIN_ID"
address_prefix = "$ADDRESS_PREFIX"
genesis_time_ms = $GENESIS_MS
block_time_ms = 1000
epoch_blocks = $EPOCH_BLOCKS
faucet = true
validator_admins = []
total_supply = "$((1000000000 + N * FAUCET_FANG))"
strict = false
operators = [
$(printf "%b" "$ops")]
reserved_names = ["blockula", "boo", "foundation", "treasury", "community", "validator", "admin", "support", "security", "official"]
features = []

# Mainnet's parameters, except durations short enough to exercise on a testnet: a week of
# unbonding and of evidence (the weak-subjectivity period is half that), day-long votes, an
# hour's timelock, spends open from genesis.
[params]
unbonding_blocks = $UNBONDING_BLOCKS
evidence_max_age_epochs = $((UNBONDING_BLOCKS / EPOCH_BLOCKS))
voting_blocks = $DAY
timelock_blocks = 3600
governance_spend_height = 0
proposal_deposit = "100000000000"
$(printf "%b" "${EXTRA_PARAMS:-}")

# Proof of Contribution from genesis: the pool's repos, an opening balance out of the endowment,
# and days instead of weeks (Séances every 3 days, a 3-day revert window, 30-day vesting).
[contribution]
opening_balance = "$POOL_FANG"
seance_blocks = $((3 * DAY))
claim_maturity_blocks = $((3 * DAY))
vesting_blocks = $((30 * DAY))
vouch_lock_blocks = $((30 * DAY))
claim_ttl_blocks = $((3 * DAY))

$(printf "%b" "$repos")

$(printf "%b" "$souls")

[[allocation]]
bucket = "community"
amount = "320000000"

[[allocation]]
bucket = "treasury"
amount = "300000000"

[[allocation]]
bucket = "endowment"
amount = "$((150000000 - POOL_FANG))"

[[allocation]]
bucket = "bootstrap"
amount = "100000000"

[[allocation]]
bucket = "founder"
address = "$FOUNDER"
amount = "100000000"
label = "founder (testnet stand-in)"

[[allocation]]
bucket = "operators"
address = "$GRANTS"
amount = "$((30000000 - N * 1000000))"
label = "security grants (testnet stand-in)"

SPEC
  # Each operator's allocation (its self-bond comes out of it) and its validator's faucet float.
  printf "%b" "$allocs" >> "$NET/spec.toml"
  # Warnings are expected: a testnet breaks mainnet's independence policy (DEC-3, DEC-14) and
  # opens the pool early (deploy/README.md lists them).
  $BIN genesis build --spec "$NET/spec.toml" --out "$NET/genesis.json"
  for i in $(seq 0 $((N - 1))); do cp "$NET/genesis.json" "$NET/v$i/genesis.json"; done
  [ -f "$NET/forwarder-secret" ] || (umask 077; openssl rand -hex 32 > "$NET/forwarder-secret")
elif fly_ok && [ ! -f "$NET/launched" ]; then
  # A genesis an earlier, failed attempt built: its clock started then, and every hour it waits
  # is a deficit the chain never makes up (blocks are at least a block time apart), counted
  # against the weak-subjectivity period until a timed checkpoint exists.
  built=$(jq -r '.created_at_ms' "$NET/genesis.json")
  hours=$((($(date +%s) * 1000 - built) / 3600000))
  if [ "$hours" -ge "${GENESIS_MAX_AGE_HOURS:-24}" ]; then
    echo "❌ $NET/genesis.json was built $hours h ago and never launched. Rebuild it with a fresh clock (the keys stay): rm $NET/genesis.json $NET/spec.toml, then rerun. GENESIS_MAX_AGE_HOURS=$((hours + 1)) launches it as it is." >&2
    exit 1
  fi
fi

# The public node's `join` pins this genesis (CRY-2: joins without a pinned genesis are refused).
GENESIS_HASH=$($BIN checkpoint --genesis "$NET/genesis.json")
echo "   genesis hash: $GENESIS_HASH"

# Prometheus targets for deploy/monitoring, over Fly's private network (a WireGuard peer in the
# org: `fly wireguard create`). Fly also scrapes every app's /metrics itself ([metrics] below).
{
  echo "# $CHAIN_ID scrape targets (deploy/monitoring/prometheus/prometheus.yml, job \"blockula\")."
  echo "- targets: ['$RPC_APP.internal:$RPC_PORT']"
  echo "  labels: { node: '$RPC_APP' }"
  for i in $(seq 0 $((N - 1))); do
    echo "- targets: ['$PREFIX-v$i.internal:$HTTP_PORT']"
    echo "  labels: { node: '$PREFIX-v$i' }"
  done
} > "$NET/prometheus-targets.yml"

if [ -n "${GENESIS_ONLY:-}" ]; then
  echo "✓ $NET: keys, operators/, spec.toml, genesis.json, prometheus-targets.yml (GENESIS_ONLY: nothing deployed)"
  exit 0
fi

if [ ! -f "$NET/secrets-staged" ]; then
  # Secrets go through stdin, never argv (visible to other local users via ps). The machines get
  # them as environment variables; nothing in their configuration names VALIDATOR_KEY_B64.
  for i in $(seq 0 $((N - 1))); do
    printf 'VALIDATOR_KEY_B64=%s\nBLOCKULA_FORWARDER_SECRET=%s\n' "$(base64 < "$NET/v$i/validator.key" | tr -d '\n')" "$(cat "$NET/forwarder-secret")" \
      | fly secrets import -a "$PREFIX-v$i" --stage >/dev/null
  done
  printf 'BLOCKULA_FORWARDER_SECRET=%s\n' "$(cat "$NET/forwarder-secret")" | fly secrets import -a "$RPC_APP" --stage >/dev/null
  touch "$NET/secrets-staged"
fi

TAG=$(git rev-parse --short HEAD)$(git diff --quiet -- node deploy || echo -dirty)-$(date +%s)
IMAGE=registry.fly.io/$RPC_APP:$TAG
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT

printf 'app = "%s"\n' "$RPC_APP" > "$WORK/build.toml"
echo "🔨 Building $IMAGE on Fly's builders…"
fly deploy . -c "$WORK/build.toml" --dockerfile deploy/Dockerfile --depot="${DEPOT:-false}" \
  --build-arg GIT_SHA="$TAG" --build-only --push --image-label "$TAG" >/dev/null

# From here on validators run and sign: this genesis is live, and RESET is refused for it (a
# retry after any failure below finishes this launch, or takes a new CHAIN_ID).
touch "$NET/launched"

SEEDING=()
DEPLOYS=()
for i in $(seq 0 $((N - 1))); do
  app=$PREFIX-v$i
  # Exit codes 10/11/12 (halt height, upgrade required, slashing protection) make the
  # entrypoint stay down instead of exiting, so neither the restart policy nor the proxy
  # restarts a validator that stopped on purpose; crashes still restart.
  validator_toml "$i" > "$WORK/$app.toml"
  echo "🧛 Deploying $app (${REGIONS[$i]})…"
  # --ha=false: exactly one machine per validator. Never scale a validator app.
  fly deploy -c "$WORK/$app.toml" --image "$IMAGE" --ha=false >/dev/null &
  DEPLOYS+=($!)
done
failed=""
for i in $(seq 0 $((N - 1))); do wait "${DEPLOYS[$i]}" || failed="$failed $PREFIX-v$i"; done
[ -z "$failed" ] || { echo "❌ deploying$failed failed (fly logs -a <app>); rerun deploy/launch.sh, without RESET" >&2; exit 1; }
# Which validators (by index) still hold their key in Fly, asked once the deploys applied the
# staged secrets (a staged secret may not be listed before).
for i in $(seq 0 $((N - 1))); do
  if has_secret "$PREFIX-v$i" VALIDATOR_KEY_B64; then SEEDING+=("$i"); fi
done

rpc_toml > "$WORK/$RPC_APP.toml"
echo "🌐 Deploying public node ${RPC_APP}…"
fly deploy -c "$WORK/$RPC_APP.toml" --image "$IMAGE" --ha=false >/dev/null

# Keys are on their volumes now: delete them from Fly so no other machine can ever load them.
# Not staged, so every validator still holding its key restarts without the variable: all of them
# together, once. One at a time is what nearly stalled the blockula-testnet-2 launch (each restart
# waited out its peers' reconnection cooldowns, minutes before public-nat's were shortened, and a
# gate that didn't wait for links took two of four validators out at once: no quorum, then a crawl
# that jailed one). Even gated, it would cost a minute or two and a few missed slots per
# validator, with the keys left in Fly until the last restart. Together, on a chain nobody uses yet,
# the chain pauses once (slashing protection signs again once it sees the network standing still),
# each validator misses about a slot, and they relink in one round. Before it, the public node and
# each of them must answer (their process start times tell the gate their new processes from the
# old ones), or nothing restarts and the keys stay for a rerun; after it, the participation gate
# (deploy/restart-gate.sh).
REMOVE=()
for i in ${SEEDING[@]+"${SEEDING[@]}"}; do
  if fly ssh console -a "$PREFIX-v$i" -C "test -f /data/home/.seeded" >/dev/null 2>&1; then
    REMOVE+=("$i")
  else
    echo "⚠️  $PREFIX-v$i: couldn't confirm the key was seeded; VALIDATOR_KEY_B64 left in place" >&2
  fi
done
if [ ${#REMOVE[@]} -gt 0 ]; then
  names=""; for i in "${REMOVE[@]}"; do names="$names $PREFIX-v$i"; done
  echo "🔒 Removing the keys seeded on their volumes from Fly, restarting$names together…"
  gate_wait "before the restart" gate_answering "${REMOVE[@]}" \
    || { echo "❌ $GATE_WHY: nothing restarted, and the keys stay in Fly. Rerun deploy/launch.sh (without RESET) to remove them." >&2; exit 1; }
  STARTS=(); PIDS=()
  for i in "${REMOVE[@]}"; do STARTS+=("$i=${GATE_STARTED[$i]}"); done
  for i in "${REMOVE[@]}"; do fly secrets unset VALIDATOR_KEY_B64 -a "$PREFIX-v$i" >/dev/null & PIDS+=($!); done
  failed=""
  for k in $(seq 0 $((${#PIDS[@]} - 1))); do wait "${PIDS[$k]}" || failed="$failed $PREFIX-v${REMOVE[$k]}"; done
  [ -z "$failed" ] || { echo "❌ removing VALIDATOR_KEY_B64 from$failed failed (fly logs -a <app>): rerun deploy/launch.sh (without RESET)" >&2; exit 1; }
  echo "🔒$names: VALIDATOR_KEY_B64 removed from Fly secrets (restarted)"
  gate_wait "after the restart" gate_check "${STARTS[@]}" \
    || { echo "❌ the validators aren't back at full participation after the key removal: $GATE_WHY. The keys are off Fly; check fly logs -a <app> before anything else restarts a validator." >&2; exit 1; }
fi

# Informational only: a failure here (e.g. a stale local DNS cache) must not skip anything above.
for _ in $(seq 60); do curl -fsS "$RPC_URL/api/health" >/dev/null 2>&1 && break; sleep 5; done
curl -fsS "$RPC_URL/api/status" | jq '{chain_id, genesis_hash, height, syncing, active_validators, version}' || echo "⚠️  couldn't reach $RPC_URL from here yet (DNS can take a few minutes)"

echo "✓ $CHAIN_ID live: $RPC_URL  (explorer at /, API at /api/status)"
echo "  genesis hash: $GENESIS_HASH  (pin it: scripts/pin-network.sh $CHAIN_ID $GENESIS_HASH --genesis $NET/genesis.json, deploy/README.md \"Relaunch\")"
