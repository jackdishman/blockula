#!/usr/bin/env bash
# Hosts one small chain on Fly.io: a single validator that also serves the chain's explorer and
# passkey wallet over HTTPS at a domain of your own, and takes outside validators on its p2p port.
# It's how blockula.app's demo chain runs (Blockula, at https://demo.blockula.app), and the way to
# host a chain for another project: one app, one machine, one volume, about $5 a month.
#
#   NAME=Blockula TOKEN=FANG CHAIN_ID=blockula-demo DOMAIN=demo.blockula.app \
#     DESCRIPTION="Blockula's demo chain." deploy/host-chain.sh
#
# Re-run it to upgrade the node (a newer VERSION) or apply a changed setting: it never makes a
# second genesis or key for a chain id. Settings (env):
#
#   NAME, TOKEN, CHAIN_ID, DOMAIN   required. The chain id is never reused: a new chain takes a new one
#   DESCRIPTION                     one sentence (shown by wallets and the explorer)
#   VERSION                         the release to run (default: the latest)
#   APP                             Fly app (default: blockula-<chain id>)
#   ORG, REGION                     Fly org and region (default: personal, iad)
#   BLOCK_TIME_MS                   default 2000
#   NO_FAUCET=1                     no faucet: the validator holds the starting supply
#   VM_MEMORY                       default 512mb (plus 512 MB of swap)
#   VOLUME_GB                       default 3 (a validator keeps about two days of blocks)
#   HOSTED_DIR                      where the chain's genesis and key live on this machine
#                                   (default ~/.config/blockula/hosted/<chain id>, mode 700)
#
# What it does, each step skipped once done:
#   1. checks the release archive's build provenance (gh attestation verify) and pins its SHA256;
#   2. makes the chain with `blockula create --reach internet --host DOMAIN --no-start` into
#      HOSTED_DIR: genesis.json and validator.key (the key's only other copy is the volume);
#   3. creates the app, its volume and a dedicated IPv4 (p2p is raw TCP, which Fly's shared IPv4
#      doesn't carry), stages the key as the secret VALIDATOR_KEY_B64 and deploys;
#   4. once the node answers for the genesis, deletes the secret (the key now lives on the volume
#      only: deploy/entrypoint.sh refuses to seed a second volume, so a clone can't double-sign);
#   5. asks Fly for DOMAIN's certificate and prints the DNS records to add and the chain directory
#      listing (chains/<chain id>.json).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
die() { echo "host-chain: $*" >&2; exit 1; }

: "${NAME:?NAME is required}" "${TOKEN:?TOKEN is required}" "${CHAIN_ID:?CHAIN_ID is required}" "${DOMAIN:?DOMAIN is required}"
DESCRIPTION=${DESCRIPTION:-}
APP=${APP:-blockula-$CHAIN_ID}
ORG=${ORG:-personal}
REGION=${REGION:-iad}
BLOCK_TIME_MS=${BLOCK_TIME_MS:-2000}
VM_MEMORY=${VM_MEMORY:-512mb}
VOLUME_GB=${VOLUME_GB:-3}
HOSTED_DIR=${HOSTED_DIR:-$HOME/.config/blockula/hosted/$CHAIN_ID}
REPO=jackdishman/blockula
ARCHIVE=blockula-x86_64-unknown-linux-gnu.tar.gz
for t in fly gh jq curl blockula; do command -v "$t" >/dev/null || die "needs $t"; done
case "$DOMAIN" in *://* | */* | *:*) die "DOMAIN is a bare host name, like chain.example.org" ;; esac

# 1. The release, verified.
VERSION=${VERSION:-$(gh release view -R "$REPO" --json tagName -q .tagName)}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
gh release download "$VERSION" -R "$REPO" -p "$ARCHIVE" -p checksums.txt -D "$work" >/dev/null
gh attestation verify "$work/$ARCHIVE" -R "$REPO" >/dev/null || die "$ARCHIVE in $VERSION has no valid build provenance"
SHA256=$(shasum -a 256 "$work/$ARCHIVE" | cut -d' ' -f1)
grep -q "^$SHA256  $ARCHIVE\$" "$work/checksums.txt" || die "$ARCHIVE doesn't match $VERSION's checksums.txt"
echo "✓ $VERSION: build provenance verified, $ARCHIVE $SHA256"

# 2. The chain: made once, never again for this chain id.
if [ ! -f "$HOSTED_DIR/genesis.json" ]; then
  install -d -m 700 "$HOSTED_DIR"
  # `create` since v1.0.3; `launch` is its older name.
  sub=create; blockula create --help >/dev/null 2>&1 || sub=launch
  blockula "$sub" --name "$NAME" --token "$TOKEN" --chain-id "$CHAIN_ID" ${DESCRIPTION:+--description "$DESCRIPTION"} \
    --reach internet --host "$DOMAIN" --block-time-ms "$BLOCK_TIME_MS" ${NO_FAUCET:+--no-faucet} \
    --home "$HOSTED_DIR/home" --no-start --yes | grep -oE 'genesis-hash [0-9a-f]{64}' | cut -d' ' -f2 > "$HOSTED_DIR/genesis-hash"
  [ -s "$HOSTED_DIR/genesis-hash" ] || die "blockula $sub printed no genesis hash"
  cp "$HOSTED_DIR/home/genesis.json" "$HOSTED_DIR/genesis.json"
  chmod 600 "$HOSTED_DIR/home/validator.key"
  echo "✓ made $NAME ($CHAIN_ID) in $HOSTED_DIR"
fi
[ -f "$HOSTED_DIR/home/validator.key" ] || die "$HOSTED_DIR/home/validator.key is missing"
GENESIS_HASH=$(cat "$HOSTED_DIR/genesis-hash")

# 3. The app.
fly apps list --org "$ORG" --json | jq -e --arg a "$APP" '.[] | select(.Name == $a)' >/dev/null \
  || { fly apps create "$APP" --org "$ORG" >/dev/null; echo "✓ created app $APP"; }
if ! fly volumes list -a "$APP" --json | jq -e '.[] | select(.Name == "data")' >/dev/null; then
  fly volumes create data -a "$APP" -r "$REGION" -s "$VOLUME_GB" --yes >/dev/null
  echo "✓ volume data ($VOLUME_GB GB, $REGION)"
fi
ips=$(fly ips list -a "$APP" --json)
echo "$ips" | jq -e '.[] | select(.Type == "v4")' >/dev/null || fly ips allocate-v4 -a "$APP" --yes >/dev/null
echo "$ips" | jq -e '.[] | select(.Type == "v6")' >/dev/null || fly ips allocate-v6 -a "$APP" >/dev/null
IPV4=$(fly ips list -a "$APP" --json | jq -r '[.[] | select(.Type == "v4")][0].Address')
IPV6=$(fly ips list -a "$APP" --json | jq -r '[.[] | select(.Type == "v6")][0].Address')

first_boot=""
if [ ! -f "$HOSTED_DIR/key-on-volume" ]; then
  first_boot=1
  printf '%s=%s\n' VALIDATOR_KEY_B64 "$(base64 < "$HOSTED_DIR/home/validator.key" | tr -d '\n')" \
    | fly secrets import -a "$APP" --stage >/dev/null
fi

cat > "$work/fly.toml" <<TOML
app = "$APP"
primary_region = "$REGION"
swap_size_mb = 512
[build]
  [build.args]
    VERSION = "$VERSION"
    SHA256 = "$SHA256"
[env]
  ROLE = "validator"
  HTTP_PORT = "7070"
  P2P_PRESET = "public-nat"
  CLIENT_IP_HEADER = "fly-client-ip"
  BLOCKULA_LOG = "warn,blockula=info"
[mounts]
  source = "data"
  destination = "/data"
[[files]]
  guest_path = "/etc/blockula/genesis.json"
  local_path = "$HOSTED_DIR/genesis.json"
# The explorer and wallet: HTTPS at the domain, and plain HTTP on 7070, the address the genesis
# publishes for nodes that sync from this one.
[[services]]
  internal_port = 7070
  protocol = "tcp"
  auto_stop_machines = "off"
  auto_start_machines = false
  min_machines_running = 1
  [[services.ports]]
    port = 443
    handlers = ["tls", "http"]
  [[services.ports]]
    port = 80
    handlers = ["http"]
    force_https = true
  [[services.ports]]
    port = 7070
    handlers = ["http"]
  [[services.http_checks]]
    path = "/api/status"
    interval = "30s"
    timeout = "5s"
    grace_period = "60s"
[[services]]
  internal_port = 30300
  protocol = "tcp"
  auto_stop_machines = "off"
  auto_start_machines = false
  min_machines_running = 1
  [[services.ports]]
    port = 30300
[[restart]]
  policy = "on-failure"
  retries = 10
[metrics]
  port = 7070
  path = "/metrics"
[[vm]]
  size = "shared-cpu-1x"
  memory = "$VM_MEMORY"
TOML

# The image needs two files from the repo, so the build context is those two alone (never the
# working tree: private material and fuzz findings stay on this machine).
ctx=$work/context
install -d "$ctx/deploy/hosted"
cp "$repo/deploy/entrypoint.sh" "$ctx/deploy/entrypoint.sh"
cp "$here/hosted/Dockerfile" "$ctx/deploy/hosted/Dockerfile"
fly deploy "$ctx" -c "$work/fly.toml" --dockerfile "$ctx/deploy/hosted/Dockerfile" --ha=false --depot=false --yes

# 4. The key leaves Fly's secrets once the node runs from the volume.
url="http://$IPV4:7070"
for _ in $(seq 1 60); do
  got=$(curl -fsS --max-time 5 "$url/api/status" 2>/dev/null | jq -r '.genesis_hash // empty' || true)
  [ -n "$got" ] && break
  sleep 5
done
[ -n "$got" ] || die "the node didn't answer at $url (fly logs -a $APP)"
[ "$got" = "$GENESIS_HASH" ] || die "the node reports genesis $got, not $GENESIS_HASH"
if [ -n "$first_boot" ]; then
  fly secrets unset VALIDATOR_KEY_B64 -a "$APP" >/dev/null
  touch "$HOSTED_DIR/key-on-volume"
  echo "✓ the validator key is on the volume only; the Fly secret is gone"
fi

# 5. HTTPS at the domain, and what to tell DNS and the chain directory.
fly certs show "$DOMAIN" -a "$APP" >/dev/null 2>&1 || fly certs add "$DOMAIN" -a "$APP" >/dev/null
cat <<EOF

🦇 $NAME runs at $url (genesis $GENESIS_HASH).

DNS for $DOMAIN (then Fly issues its certificate: fly certs check $DOMAIN -a $APP):
  A     $DOMAIN  $IPV4
  AAAA  $DOMAIN  $IPV6

Its wallet: https://$DOMAIN/wallet/   Explorer: https://$DOMAIN/
Their own node: blockula join --genesis https://$DOMAIN --genesis-hash $GENESIS_HASH

chains/$CHAIN_ID.json, to list it on blockula.app:
$(jq -n --arg name "$NAME" --arg id "$CHAIN_ID" --arg token "$TOKEN" --arg d "$DESCRIPTION" --arg rpc "https://$DOMAIN" \
  --arg g "$GENESIS_HASH" --arg p2p "$DOMAIN:30300" --arg day "$(date -u +%F)" \
  '{name: $name, chain_id: $id, token: $token, description: $d, rpc: $rpc, genesis_hash: $g, validators_wanted: true, p2p: $p2p, contact: "<your GitHub handle>", listed_at: $day}')
EOF
