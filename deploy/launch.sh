#!/usr/bin/env bash
# 🧛 Launches (or upgrades) a protocol v1 public testnet on Fly.io. Safe to re-run.
#   N validator apps: own dedicated IPv4 (p2p only) + volume each (scheduled snapshots off: a
#     restored validator volume can double-sign), spread across regions
#   1 public full node app with HTTPS at https://$RPC_APP.fly.dev: the API, the explorer, and the
#     sync source for anyone joining. It reaches validators over Fly's private network.
# The genesis is built with the genesis builder (`blockula genesis operator` + `genesis build`,
# docs/GENESIS_CEREMONY.md) from a spec this script writes once: the DEC-5 allocation with `tboo`
# addresses, one operator per validator with its own owner account (which holds the self-bond and
# pays the validator's system fees), no admin keys, and a faucet funded on the validators'
# consensus-key accounts. Keys are generated once on this laptop under
# ~/.config/blockula/$CHAIN_ID (mode 700): consensus keys are uploaded only to their validator,
# seeded onto its volume on first boot, then deleted from Fly; owner, founder and foundation keys
# never leave this machine. (A public testnet is still one operator's machines; independent
# operators come with the incentivized testnet, OPS-1.)
#
#   deploy/launch.sh                  launch, or upgrade the nodes of, $CHAIN_ID
#   GENESIS_ONLY=1 deploy/launch.sh   rehearsal: only build keys, operator files, spec and genesis
#                                     under ~/.config/blockula/$CHAIN_ID-rehearsal (placeholder IPs
#                                     unless IPS="ip ip …"); no Fly calls
#   RESET=1 deploy/launch.sh          wipe the apps' machines, volumes and secrets (another chain's
#                                     data) before launching $CHAIN_ID. Refused once $CHAIN_ID has
#                                     launched: a chain id is never reused, pick a new one
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
FAUCET_FANG=${FAUCET_FANG:-2500000}     # per validator's consensus-key account (drips + fees)
SELF_BOND=${SELF_BOND:-10000}           # FANG each owner bonds (the default min_self_bond)
VALIDATOR_VM=${VALIDATOR_VM:-shared-cpu-2x}
VALIDATOR_MEMORY=${VALIDATOR_MEMORY:-2048mb}
RPC_VM=${RPC_VM:-shared-cpu-1x}
RPC_MEMORY=${RPC_MEMORY:-1024mb}
VOLUME_GB=${VOLUME_GB:-10}
P2P_PORT=30300
HTTP_PORT=7070
CONF=$HOME/.config/blockula
# A rehearsal (GENESIS_ONLY, placeholder IPs) never shares a directory with the real launch.
NET=$CONF/$CHAIN_ID${GENESIS_ONLY:+-rehearsal}
N=${#REGIONS[@]}
RPC_URL=https://$RPC_APP.fly.dev
APPS=("$RPC_APP"); for i in $(seq 0 $((N - 1))); do APPS+=("$PREFIX-v$i"); done

fly_ok() { [ -z "${GENESIS_ONLY:-}" ]; }
if fly_ok; then fly auth whoami >/dev/null || { echo "Run: fly auth login" >&2; exit 1; }; fi
mkdir -p "$CONF" && chmod 700 "$CONF"
cargo build --release --locked --manifest-path node/Cargo.toml -q
BIN=node/target/release/blockula

if [ "${RESET:-}" = 1 ]; then
  [ ! -f "$NET/launched" ] || { echo "❌ $CHAIN_ID has launched already: a chain id is never reused. Set CHAIN_ID to a new one (and RESET=1 to clear the apps)." >&2; exit 1; }
  echo "♻️  RESET: destroying the apps' machines, volumes and secrets before launching $CHAIN_ID"
  for app in "${APPS[@]}"; do
    fly machines list -a "$app" --json 2>/dev/null | jq -r '.[].id' | while read -r m; do fly machine destroy "$m" -a "$app" --force >/dev/null; done
    fly volumes list -a "$app" --json 2>/dev/null | jq -r '.[].id' | while read -r v; do fly volumes destroy "$v" -a "$app" --yes >/dev/null; done
    fly secrets list -a "$app" --json 2>/dev/null | jq -r '.[].name // .[].Name' | while read -r s; do fly secrets unset "$s" -a "$app" --stage >/dev/null; done
  done
fi

ensure_app() { fly apps list --org "$ORG" --json | jq -e --arg a "$1" '.[] | select(.Name == $a)' >/dev/null || fly apps create "$1" --org "$ORG" >/dev/null; }
ensure_volume() { # app region [validator]
  local snapshots=true
  [ "${3:-}" = validator ] && snapshots=false
  if [ "$(fly volumes list -a "$1" --json | jq length)" -eq 0 ]; then
    fly volumes create data -a "$1" -r "$2" -s "$VOLUME_GB" --scheduled-snapshots="$snapshots" --yes >/dev/null
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

IPS_ARR=()
if fly_ok; then
  echo "🪦 Apps, volumes and IPs…"
  ensure_app "$RPC_APP"; ensure_volume "$RPC_APP" "$RPC_REGION"
  for i in $(seq 0 $((N - 1))); do
    app=$PREFIX-v$i
    ensure_app "$app"; ensure_volume "$app" "${REGIONS[$i]}" validator
    IPS_ARR+=("$(dedicated_v4 "$app")")
    echo "   $app  ${REGIONS[$i]}  ${IPS_ARR[$i]}"
  done
else
  read -r -a IPS_ARR <<< "${IPS:-}"
  for i in $(seq ${#IPS_ARR[@]} $((N - 1))); do IPS_ARR+=("203.0.113.$((i + 1))"); done
fi

if [ ! -f "$NET/genesis.json" ]; then
  echo "🧬 Building the genesis of $CHAIN_ID ($N validators, prefix $ADDRESS_PREFIX)…"
  (umask 077; mkdir -p "$NET/operators")
  key() { [ -f "$1" ] || (umask 077; $BIN keygen --prefix "$ADDRESS_PREFIX" > "$1"); jq -r .address "$1"; }
  FOUNDER=$(key "$NET/founder.json"); FOUNDATION=$(key "$NET/foundation.json"); GRANTS=$(key "$NET/grants.json")
  ops="" ; allocs=""
  for i in $(seq 0 $((N - 1))); do
    (umask 077; mkdir -p "$NET/v$i")
    CONSENSUS=$(key "$NET/v$i/consensus.json")
    [ -f "$NET/v$i/validator.key" ] || (umask 077; jq -r .seed "$NET/v$i/consensus.json" > "$NET/v$i/validator.key")
    OWNER=$(key "$NET/owner-$i.json")
    $BIN genesis operator --chain-id "$CHAIN_ID" --key "$NET/v$i/validator.key" --owner "$OWNER" \
      --p2p-addr "${IPS_ARR[$i]}:$P2P_PORT" --http-addr "$RPC_URL" --commission-bps 500 --self-bond "$SELF_BOND" \
      --name "Blockula testnet validator $i (${REGIONS[$i]})" --legal-entity "Blockula (testnet)" --entity-jurisdiction US \
      --hosting-provider Fly.io --asn 40509 --datacenter-country "$(country_of "${REGIONS[$i]}")" --datacenter "fly-${REGIONS[$i]}" \
      --founder-or-foundation --out "$NET/operators/v$i.toml"
    ops="$ops  \"operators/v$i.toml\",\n"
    allocs="$allocs[[allocation]]\nbucket = \"operators\"\naddress = \"$OWNER\"\namount = \"1000000\"\nlabel = \"genesis operator: validator $i\"\n\n"
    allocs="$allocs[[allocation]]\nbucket = \"faucet\"\naddress = \"$CONSENSUS\"\namount = \"$FAUCET_FANG\"\nlabel = \"faucet: validator $i\"\n\n"
  done
  GENESIS_MS=$(($(date +%s) * 1000))
  cat > "$NET/spec.toml" <<SPEC
# The $CHAIN_ID genesis spec, written by deploy/launch.sh. Rebuild it (byte-identically) with
#   blockula genesis build --spec spec.toml --out genesis.json
# Founder, foundation and grant accounts are testnet stand-ins held by the launcher.
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

# Mainnet's parameters, except durations short enough to exercise on a testnet: a day of
# unbonding (and of evidence), day-long votes, an hour's timelock, spends open from genesis.
[params]
unbonding_blocks = 86400
evidence_max_age_epochs = $((86400 / EPOCH_BLOCKS))
voting_blocks = 86400
timelock_blocks = 3600
governance_spend_height = 0
proposal_deposit = "100000000000"

[[allocation]]
bucket = "community"
amount = "320000000"

[[allocation]]
bucket = "treasury"
amount = "250000000"

[[allocation]]
bucket = "endowment"
amount = "150000000"

[[allocation]]
bucket = "bootstrap"
amount = "100000000"

[[allocation]]
bucket = "founder"
address = "$FOUNDER"
amount = "100000000"
label = "founder (testnet stand-in)"

[[allocation]]
bucket = "foundation"
address = "$FOUNDATION"
amount = "50000000"
label = "foundation (testnet stand-in)"

[[allocation]]
bucket = "operators"
address = "$GRANTS"
amount = "$((30000000 - N * 1000000))"
label = "security grants (testnet stand-in)"

SPEC
  # Each operator's allocation (its self-bond comes out of it) and its validator's faucet float.
  printf "%b" "$allocs" >> "$NET/spec.toml"
  # Warnings are expected: a testnet breaks mainnet's independence policy (DEC-3, DEC-14).
  $BIN genesis build --spec "$NET/spec.toml" --out "$NET/genesis.json"
  for i in $(seq 0 $((N - 1))); do cp "$NET/genesis.json" "$NET/v$i/genesis.json"; done
  (umask 077; openssl rand -hex 32 > "$NET/forwarder-secret")
fi

# The public node's `join` pins this genesis (CRY-2: joins without a pinned genesis are refused).
GENESIS_HASH=$($BIN checkpoint --genesis "$NET/genesis.json")
echo "   genesis hash: $GENESIS_HASH"

# Prometheus targets for deploy/monitoring, over Fly's private network (a WireGuard peer in the
# org: `fly wireguard create`). Fly also scrapes every app's /metrics itself ([metrics] below).
{
  echo "# $CHAIN_ID scrape targets (deploy/monitoring/prometheus/prometheus.yml, job \"blockula\")."
  echo "- targets: ['$RPC_APP.internal:7171']"
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
  # Secrets go through stdin, never argv (visible to other local users via ps).
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

SEEDING=()
for i in $(seq 0 $((N - 1))); do
  app=$PREFIX-v$i
  key_file=""
  if has_secret "$app" VALIDATOR_KEY_B64; then
    SEEDING+=("$app")
    key_file=$'[[files]]\n  guest_path = "/etc/blockula/validator.key"\n  secret_name = "VALIDATOR_KEY_B64"'
  fi
  halt=""
  [ -n "${HALT_HEIGHT:-}" ] && halt="  HALT_HEIGHT = \"$HALT_HEIGHT\""
  # Exit codes 10/11/12 (halt height, upgrade required, slashing protection) make the
  # entrypoint stay down instead of exiting, so neither the restart policy nor the proxy
  # restarts a validator that stopped on purpose; crashes still restart.
  cat > "$WORK/$app.toml" <<TOML
app = "$app"
primary_region = "${REGIONS[$i]}"
[env]
  ROLE = "validator"
  P2P_PRESET = "public-nat"
  BLOCKULA_LOG = "warn,blockula=info"
$halt
[mounts]
  source = "data"
  destination = "/data"
[[files]]
  guest_path = "/etc/blockula/genesis.json"
  local_path = "$NET/genesis.json"
$key_file
[[services]]
  internal_port = $P2P_PORT
  protocol = "tcp"
  auto_stop_machines = "off"
  auto_start_machines = false
  min_machines_running = 1
  [[services.ports]]
    port = $P2P_PORT
[[restart]]
  policy = "on-failure"
  retries = 10
[metrics]
  port = $HTTP_PORT
  path = "/metrics"
[[vm]]
  size = "$VALIDATOR_VM"
  memory = "$VALIDATOR_MEMORY"
TOML
  echo "🧛 Deploying $app (${REGIONS[$i]})…"
  # --ha=false: exactly one machine per validator. Never scale a validator app.
  fly deploy -c "$WORK/$app.toml" --image "$IMAGE" --ha=false >/dev/null &
done
wait

PEERS=""
for i in $(seq 0 $((N - 1))); do PEERS="$PEERS http://$PREFIX-v$i.internal:$HTTP_PORT"; done
cat > "$WORK/$RPC_APP.toml" <<TOML
app = "$RPC_APP"
primary_region = "$RPC_REGION"
[env]
  ROLE = "rpc"
  GENESIS_HASH = "$GENESIS_HASH"
  CLIENT_IP_HEADER = "fly-client-ip"
  PEER_HTTP = "${PEERS# }"
  BLOCKULA_LOG = "warn,blockula=info"
[mounts]
  source = "data"
  destination = "/data"
[[files]]
  guest_path = "/etc/blockula/genesis.json"
  local_path = "$NET/genesis.json"
[http_service]
  internal_port = 7171
  force_https = true
  auto_stop_machines = "off"
  min_machines_running = 1
  [[http_service.checks]]
    method = "GET"
    path = "/api/health"
    interval = "15s"
    timeout = "5s"
    grace_period = "60s"
[metrics]
  port = 7171
  path = "/metrics"
[[vm]]
  size = "$RPC_VM"
  memory = "$RPC_MEMORY"
TOML
echo "🌐 Deploying public node ${RPC_APP}…"
fly deploy -c "$WORK/$RPC_APP.toml" --image "$IMAGE" --ha=false >/dev/null

# Keys are on their volumes now: delete them from Fly so no other machine can ever load them.
for app in ${SEEDING[@]+"${SEEDING[@]}"}; do
  if fly ssh console -a "$app" -C "test -f /data/home/.seeded" >/dev/null 2>&1; then
    fly secrets unset VALIDATOR_KEY_B64 -a "$app" --stage >/dev/null && echo "🔒 $app: key seeded on its volume; removed from Fly secrets"
  else
    echo "⚠️  $app: couldn't confirm the key was seeded; VALIDATOR_KEY_B64 left in place" >&2
  fi
done
touch "$NET/launched"

# Informational only: a failure here (e.g. a stale local DNS cache) must not skip anything above.
for _ in $(seq 60); do curl -fsS "$RPC_URL/api/health" >/dev/null 2>&1 && break; sleep 5; done
curl -fsS "$RPC_URL/api/status" | jq '{chain_id, genesis_hash, height, syncing, active_validators, version}' || echo "⚠️  couldn't reach $RPC_URL from here yet (DNS can take a few minutes)"

echo "✓ $CHAIN_ID live: $RPC_URL  (explorer at /, API at /api/status)"
echo "  genesis hash: $GENESIS_HASH  (pin it in the MCP and wallet: deploy/README.md \"Relaunch\")"
