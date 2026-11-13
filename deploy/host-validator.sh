#!/usr/bin/env bash
# Runs one more validator for an existing chain on Fly.io: a machine with a public IPv4 for p2p,
# so nothing at home is exposed. It's how the demo chain got its second validator, and the cloud
# path in docs/BECOME-A-VALIDATOR.md. One app, one machine, one volume, about $5 a month.
#
#   GENESIS=https://demo.blockula.app GENESIS_HASH=<64 hex> APP=my-validator \
#     deploy/host-validator.sh
#
# Re-run it to upgrade the node (a newer VERSION): it never makes a second key for an app.
# Settings (env):
#
#   GENESIS, GENESIS_HASH   required: a node of the chain (https://…) and its genesis hash, which
#                           pins the chain (ask its operator, or read its blockula.app listing)
#   APP                     required: the Fly app name; the validator advertises <app>.fly.dev
#   VERSION                 the release to run (default: the latest)
#   ORG, REGION             Fly org and region (default: personal, ord)
#   BOND                    the self-bond in coins (default: the chain's minimum)
#   VM_MEMORY, VOLUME_GB    default 512mb (plus 512 MB of swap) and 3
#   HOSTED_DIR              where its key lives on this machine
#                           (default ~/.config/blockula/hosted/<chain id>-<app>, mode 700)
#
# What it does, each step skipped once done:
#   1. checks the release archive's build provenance and pins its SHA256 (as host-chain.sh does);
#   2. `blockula validator init` into HOSTED_DIR, advertising <app>.fly.dev:30300 (p2p) and
#      <app>.fly.dev:7070 (HTTP): validator.key (its only other copy is the volume), genesis.json
#      and node.json;
#   3. funds the validator's account from the chain's faucet, if it has one (else it says what to
#      ask the chain's operator for, and stops before deploying);
#   4. creates the app, its volume and a dedicated IPv4, stages the key as the secret
#      VALIDATOR_KEY_B64 and deploys; once the node answers for the genesis, deletes the secret;
#   5. signs `blockula validator register` (the bond) once the node answers, and
#      prints how the chain lets it in: for a set of one to three, the command its operator runs.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
die() { echo "host-validator: $*" >&2; exit 1; }

: "${GENESIS:?GENESIS is required (a node of the chain, https://…)}" "${GENESIS_HASH:?GENESIS_HASH is required}" "${APP:?APP is required}"
GENESIS=${GENESIS%/}
ORG=${ORG:-personal}
REGION=${REGION:-ord}
VM_MEMORY=${VM_MEMORY:-512mb}
VOLUME_GB=${VOLUME_GB:-3}
REPO=jackdishman/blockula
ARCHIVE=blockula-x86_64-unknown-linux-gnu.tar.gz
for t in fly gh jq curl blockula; do command -v "$t" >/dev/null || die "needs $t"; done
echo "$GENESIS_HASH" | grep -qE '^[0-9a-f]{64}$' || die "GENESIS_HASH is 64 lowercase hex"
status=$(curl -fsS --max-time 10 "$GENESIS/api/status") || die "$GENESIS doesn't answer /api/status"
[ "$(echo "$status" | jq -r .genesis_hash)" = "$GENESIS_HASH" ] || die "$GENESIS reports another genesis than $GENESIS_HASH"
CHAIN_ID=$(echo "$status" | jq -r .chain_id)
HOSTED_DIR=${HOSTED_DIR:-$HOME/.config/blockula/hosted/$CHAIN_ID-$APP}
HOST=$APP.fly.dev

# 1. The release, verified.
VERSION=${VERSION:-$(gh release view -R "$REPO" --json tagName -q .tagName)}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
gh release download "$VERSION" -R "$REPO" -p "$ARCHIVE" -p checksums.txt -D "$work" >/dev/null
gh attestation verify "$work/$ARCHIVE" -R "$REPO" >/dev/null || die "$ARCHIVE in $VERSION has no valid build provenance"
SHA256=$(shasum -a 256 "$work/$ARCHIVE" | cut -d' ' -f1)
grep -q "^$SHA256  $ARCHIVE\$" "$work/checksums.txt" || die "$ARCHIVE doesn't match $VERSION's checksums.txt"
echo "✓ $VERSION: build provenance verified, $ARCHIVE $SHA256"

# 2. The validator's key and addresses: made once for this app.
if [ ! -f "$HOSTED_DIR/home/validator.key" ]; then
  install -d -m 700 "$HOSTED_DIR"
  blockula validator init --home "$HOSTED_DIR/home" --genesis "$GENESIS" --genesis-hash "$GENESIS_HASH" \
    --public-p2p-addr "$HOST:30300" --public-http-addr "$HOST:7070" \
    | grep -oE 'address \(account \+ validator id\): [a-z]+1[02-9ac-hj-np-z]+' | awk '{print $NF}' > "$HOSTED_DIR/address"
  chmod 600 "$HOSTED_DIR/home/validator.key"
  echo "✓ validator key made in $HOSTED_DIR (back it up; never run it in two places)"
fi
ADDRESS=$(cat "$HOSTED_DIR/address")
[ -n "$ADDRESS" ] || die "no address in $HOSTED_DIR/address (blockula validator init prints it)"

# 3. The bond. A validator already registered has bonded (its bond is stake, not balance): an
# upgrade re-run asks the faucet for nothing.
validators=$(curl -fsS "$GENESIS/api/validators")
min_bond=$(jq -r .min_bond <<<"$validators")
registered=$(jq -r --arg a "$ADDRESS" '[.active, .joining, .candidates, .jailed, .leaving] | map(. // []) | add | map(.validator.address, .owner.address) | index($a) != null' <<<"$validators")
balance=$(curl -fsS "$GENESIS/api/account/$ADDRESS" | jq -r '.balance // "0"')
if [ "$registered" != true ] && [ "$balance" -lt "$min_bond" ] 2>/dev/null; then
  if [ "$(curl -fsS "$GENESIS/api/genesis" | jq -r .faucet)" = true ]; then
    blockula wallet faucet --to "$ADDRESS" --rpc "$GENESIS" >/dev/null
    echo "✓ funded $ADDRESS from the faucet"
  else
    die "$ADDRESS needs at least $min_bond drops plus fees: ask the chain's operator to send them, then run this again"
  fi
fi

# 4. The app.
fly apps list --org "$ORG" --json | jq -e --arg a "$APP" '.[] | select(.Name == $a)' >/dev/null \
  || { fly apps create "$APP" --org "$ORG" >/dev/null; echo "✓ created app $APP"; }
# flyctl's volume listing names the field `name`; older ones printed `Name`. Missing it made a
# volume on every re-run, each left unattached beside the one the machine mounts.
if ! fly volumes list -a "$APP" --json | jq -e '.[] | select((.name // .Name) == "data")' >/dev/null; then
  fly volumes create data -a "$APP" -r "$REGION" -s "$VOLUME_GB" --yes >/dev/null
  echo "✓ volume data ($VOLUME_GB GB, $REGION)"
fi
ips=$(fly ips list -a "$APP" --json)
echo "$ips" | jq -e '.[] | select(.Type == "v4")' >/dev/null || fly ips allocate-v4 -a "$APP" --yes >/dev/null
echo "$ips" | jq -e '.[] | select(.Type == "v6")' >/dev/null || fly ips allocate-v6 -a "$APP" >/dev/null
IPV4=$(fly ips list -a "$APP" --json | jq -r '[.[] | select(.Type == "v4")][0].Address')

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
  PEER_HTTP = "$GENESIS"
  BLOCKULA_LOG = "warn,blockula=info"
[mounts]
  source = "data"
  destination = "/data"
[[files]]
  guest_path = "/etc/blockula/genesis.json"
  local_path = "$HOSTED_DIR/home/genesis.json"
[[files]]
  guest_path = "/etc/blockula/node.json"
  local_path = "$HOSTED_DIR/home/node.json"
# Its HTTP API on 7070, the address it publishes (other nodes sync from it), and HTTPS.
[[services]]
  internal_port = 7070
  protocol = "tcp"
  auto_stop_machines = "off"
  auto_start_machines = false
  min_machines_running = 1
  [[services.ports]]
    port = 7070
    handlers = ["http"]
  [[services.ports]]
    port = 443
    handlers = ["tls", "http"]
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

ctx=$work/context
install -d "$ctx/deploy/hosted"
cp "$repo/deploy/entrypoint.sh" "$ctx/deploy/entrypoint.sh"
cp "$here/hosted/Dockerfile" "$ctx/deploy/hosted/Dockerfile"
fly deploy "$ctx" -c "$work/fly.toml" --dockerfile "$ctx/deploy/hosted/Dockerfile" --ha=false --depot=false --yes

url="http://$IPV4:7070"
got=""
for _ in $(seq 1 60); do
  got=$(curl -fsS --max-time 5 "$url/api/status" 2>/dev/null | jq -r '.genesis_hash // empty' || true)
  [ -n "$got" ] && break
  sleep 5
done
[ "$got" = "$GENESIS_HASH" ] || die "the node didn't answer for $GENESIS_HASH at $url (fly logs -a $APP)"
if [ -n "$first_boot" ]; then
  fly secrets unset VALIDATOR_KEY_B64 -a "$APP" >/dev/null
  touch "$HOSTED_DIR/key-on-volume"
  echo "✓ the validator key is on the volume only; the Fly secret is gone"
fi

# 5. Registered (once). The node caught up over HTTP (PEER_HTTP) as it started; past that it
# stands still until it's registered, because the other validators' p2p handshakes let in only
# the keys of their set and its candidates. It follows over p2p from then on.
if ! curl -fsS "$GENESIS/api/validators" | jq -e --arg a "$ADDRESS" '[(.active // [])[], (.candidates // [])[], (.joining // [])[]] | any(.validator.address == $a)' >/dev/null; then
  blockula validator register --home "$HOSTED_DIR/home" --rpc "$GENESIS" --node-rpc "$url" ${BOND:+--bond "$BOND"}
fi
cat <<EOF

🦇 Validator $ADDRESS runs on Fly app $APP ($HOST, IPv4 $IPV4).
   Check it: curl -s $GENESIS/api/validators | jq '.active[].validator.address'
   Logs: fly logs -a $APP     Key backup: $HOSTED_DIR/home/validator.key (never run it twice)
   If the chain has one to three validators, send its operator your address to admit:
     blockula validator admit $ADDRESS --key <their validator.key> --rpc $GENESIS
EOF
