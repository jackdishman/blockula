#!/usr/bin/env bash
# 🧛 Launches (or upgrades) the Blockula testnet on Fly.io. Safe to re-run.
#   4 validator apps: own dedicated IPv4 (p2p only) + volume each, spread across regions
#   1 public full node app with HTTPS at https://$RPC_APP.fly.dev: the API, the explorer, and the
#     sync source for anyone joining. It reaches validators over Fly's private network.
# Genesis and validator keys are generated once on this laptop under ~/.config/blockula/testnet
# (mode 700). Each key is uploaded only to its own validator, seeded onto its volume on first
# boot, then deleted from Fly so no other machine can ever start with it.
#
#   RESET=1 deploy/launch.sh   wipe chain data + genesis and start a brand-new testnet
set -euo pipefail
cd "$(dirname "$0")/.."

ORG=${ORG:-personal}
PREFIX=${PREFIX:-blockula}
RPC_APP=${RPC_APP:-$PREFIX-testnet}
CHAIN_ID=${CHAIN_ID:-blockula-testnet}
REGIONS=(${REGIONS:-iad sjc fra ams})   # one validator per region
RPC_REGION=${RPC_REGION:-iad}
P2P_PORT=30300
HTTP_PORT=7070
CONF=$HOME/.config/blockula
NET=$CONF/testnet
N=${#REGIONS[@]}
RPC_URL=https://$RPC_APP.fly.dev
APPS=("$RPC_APP"); for i in $(seq 0 $((N - 1))); do APPS+=("$PREFIX-v$i"); done

fly auth whoami >/dev/null || { echo "Run: fly auth login" >&2; exit 1; }
mkdir -p "$CONF" && chmod 700 "$CONF"
cargo build --release --locked --manifest-path node/Cargo.toml -q
BIN=node/target/release/blockula

if [ "${RESET:-}" = 1 ]; then
  echo "♻️  RESET: destroying machines + volumes and the local genesis/keys for $CHAIN_ID"
  for app in "${APPS[@]}"; do
    fly machines list -a "$app" --json 2>/dev/null | jq -r '.[].id' | while read -r m; do fly machine destroy "$m" -a "$app" --force >/dev/null; done
    fly volumes list -a "$app" --json 2>/dev/null | jq -r '.[].id' | while read -r v; do fly volumes destroy "$v" -a "$app" --yes >/dev/null; done
    fly secrets list -a "$app" --json 2>/dev/null | jq -r '.[].name // .[].Name' | while read -r s; do fly secrets unset "$s" -a "$app" --stage >/dev/null; done
  done
  rm -rf "$NET"
fi

# Season 0 admin key: approves/removes validators. Lives only on this laptop.
[ -f "$CONF/admin.json" ] || { (umask 077; $BIN keygen > "$CONF/admin.json"); echo "🔑 Admin key: $CONF/admin.json (back it up)"; }
ADMIN=$(jq -r .address "$CONF/admin.json")

ensure_app() { fly apps list --org "$ORG" --json | jq -e --arg a "$1" '.[] | select(.Name == $a)' >/dev/null || fly apps create "$1" --org "$ORG" >/dev/null; }
ensure_volume() { # app region
  [ "$(fly volumes list -a "$1" --json | jq length)" -gt 0 ] || fly volumes create data -a "$1" -r "$2" -s 1 --yes >/dev/null
}
dedicated_v4() {
  local ip
  ip=$(fly ips list -a "$1" --json | jq -r '[.[] | select(.Type == "v4")][0].Address // empty')
  [ -n "$ip" ] || { fly ips allocate-v4 -a "$1" --yes >/dev/null; ip=$(fly ips list -a "$1" --json | jq -r '[.[] | select(.Type == "v4")][0].Address'); }
  echo "$ip"
}
has_secret() { fly secrets list -a "$1" --json | jq -e --arg s "$2" 'map(.name // .Name) | index($s)' >/dev/null; }

echo "🪦 Apps, volumes and IPs…"
ensure_app "$RPC_APP"; ensure_volume "$RPC_APP" "$RPC_REGION"
IPS=()
for i in $(seq 0 $((N - 1))); do
  app=$PREFIX-v$i
  ensure_app "$app"; ensure_volume "$app" "${REGIONS[$i]}"
  IPS+=("$(dedicated_v4 "$app")")
  echo "   $app  ${REGIONS[$i]}  ${IPS[$i]}"
done

if [ ! -f "$NET/genesis.json" ]; then
  echo "🧬 Summoning genesis for $CHAIN_ID ($N validators)…"
  (umask 077; $BIN summon "$CHAIN_ID" --validators "$N" --home "$NET" --admin "$ADMIN" >/dev/null)
  # Validators advertise their Fly IPv4 for p2p. Their HTTP isn't public: joiners sync from the
  # public node, which forwards txs to validators privately.
  jq --argjson ips "$(printf '%s\n' "${IPS[@]}" | jq -R . | jq -s .)" --arg p2p "$P2P_PORT" --arg http "$RPC_URL" \
    '.validators |= [to_entries[] | .value + {p2p_addr: "\($ips[.key]):\($p2p)", http_addr: $http}]' \
    "$NET/v0/genesis.json" > "$NET/genesis.json"
  for i in $(seq 0 $((N - 1))); do cp "$NET/genesis.json" "$NET/v$i/genesis.json"; done
  (umask 077; openssl rand -hex 32 > "$NET/forwarder-secret")
  # Secrets go through stdin, never argv (visible to other local users via ps).
  for i in $(seq 0 $((N - 1))); do
    printf 'VALIDATOR_KEY_B64=%s\nBLOCKULA_FORWARDER_SECRET=%s\n' "$(base64 < "$NET/v$i/validator.key" | tr -d '\n')" "$(cat "$NET/forwarder-secret")" \
      | fly secrets import -a "$PREFIX-v$i" --stage >/dev/null
  done
  printf 'BLOCKULA_FORWARDER_SECRET=%s\n' "$(cat "$NET/forwarder-secret")" | fly secrets import -a "$RPC_APP" --stage >/dev/null
fi

# The public node's `join` pins this genesis (CRY-2: joins without a pinned genesis are refused).
GENESIS_HASH=$($BIN checkpoint --genesis "$NET/genesis.json")

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
  cat > "$WORK/$app.toml" <<TOML
app = "$app"
primary_region = "${REGIONS[$i]}"
[env]
  ROLE = "validator"
  P2P_PRESET = "public-nat"
  BLOCKULA_LOG = "warn,blockula=info"
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
  min_machines_running = 1
  [[services.ports]]
    port = $P2P_PORT
[[vm]]
  size = "shared-cpu-1x"
  memory = "512mb"
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
[[vm]]
  size = "shared-cpu-1x"
  memory = "512mb"
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

# Informational only: a failure here (e.g. a stale local DNS cache) must not skip anything above.
for _ in $(seq 60); do curl -fsS "$RPC_URL/api/health" >/dev/null 2>&1 && break; sleep 5; done
curl -fsS "$RPC_URL/api/status" | jq '{chain_id, height, syncing, active_validators, version}' || echo "⚠️  couldn't reach $RPC_URL from here yet (DNS can take a few minutes)"

echo "✓ Testnet live: $RPC_URL  (explorer at /, API at /api/status)"
echo "  Wallet: https://blockula-wallet.vercel.app"
