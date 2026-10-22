#!/usr/bin/env bash
# 🧛 Mainnet (blockula-1) on Fly: the founder's validators and two public RPC nodes, one pruning
# and one archive. The launch-day runbook is deploy/mainnet/README.md.
#
# Unlike the testnet's deploy/launch.sh, this script never makes a key, never writes a spec and
# never destroys anything (there is no RESET). It deploys a reviewed genesis (genesis/<chain id>/,
# built and reproduced by the genesis ceremony and pinned by the release, docs/GENESIS_CEREMONY.md)
# with consensus keys generated offline, and runs the signed release's own binary, never a source
# build.
#
#   deploy/mainnet/launch-mainnet.sh prepare   apps, volumes, IPs (and certificates): before the
#                                              operator files, whose proof of possession binds
#                                              each validator's p2p address
#   deploy/mainnet/launch-mainnet.sh image     verify RELEASE and build its image; deploys nothing
#   deploy/mainnet/launch-mainnet.sh launch    the first deploy, within LAUNCH_WINDOW_MINS of genesis
#                                              time: the RPC nodes, then the validators (each key
#                                              seeded on its volume, then removed from Fly), then
#                                              waits until the chain runs at full participation
#   deploy/mainnet/launch-mainnet.sh upgrade   RELEASE on every node, one at a time through the
#                                              participation gate (deploy/restart-gate.sh)
#
# Settings (environment). Required: CHAIN_ID (blockula-1, or a dry run's chain id), ORG (the Fly
# organization: mainnet's own, never `personal`), PREFIX (validator apps $PREFIX-v0 …), RPC_APP
# (the pruning RPC node), ARCHIVE_APP (the archive RPC node); and for image, launch and upgrade
# RELEASE (vX.Y.Z: this checkout must be that tag, with no changes) and VERIFIER (a `blockula` of
# that release you installed with `blockula release install`, default `blockula` on PATH: it
# checks the release's signature, rebuilds the genesis and matches the keys to it); for launch
# KEYS_DIR (v0/validator.key …: the consensus keys of the founder seats whose keys it stages, and
# nothing else).
# Optional: REGIONS ("iad sjc fra ams", one validator each), RPC_REGION (iad), ARCHIVE_REGION
# (ord), RPC_DOMAIN and ARCHIVE_DOMAIN (custom domains: prepare requests their certificates),
# OUTSIDE_VALIDATORS (the addresses of genesis validators other operators run: the gate can't read
# their machines, deploy/restart-gate.sh), GENESIS_DIR (a dry run's genesis, spec.toml and
# operators/; blockula-1's is always genesis/blockula-1), RELEASE_FROM (a directory or URL holding
# the release's files instead of GitHub), UNSIGNED_RELEASE=1 (a dry-run chain before the release
# key exists: the archive is checked against checksums.txt and build provenance only; refused for
# blockula-1), PROVENANCE=0 (with UNSIGNED_RELEASE, skip `gh attestation verify`),
# LAUNCH_WINDOW_MINS (60), STATE_DIR (~/.config/blockula/$CHAIN_ID-fly: the forwarder secret and
# markers, never a key), the sizes below, GATE_SECS and PROGRESS_SECS (the gate's).
#
# DRY_RUN=1 prints every fly command, and each deploy's configuration under it. It runs only the
# ones that read (auth whoami, apps, ips, volumes, machines, secrets and certs lists), so the plan
# matches what exists; it creates, allocates, stages, builds, deploys and restarts nothing, writes
# nothing under STATE_DIR, stages no key (with KEYS_DIR, it reads the keys only to check them
# against the genesis), and skips the waits. A dry run of launch also plans it before
# LAUNCH_WINDOW_MINS (saying when the launch itself may start) and without KEYS_DIR (then the keys
# aren't checked against the genesis), so it can be reviewed the day before, while the keys are
# still encrypted.
set -euo pipefail
START=$PWD
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
cd "$ROOT"
# A path in the settings is the caller's, relative to where the script was started.
abs() { case $1 in /* | "") printf '%s' "$1" ;; *) printf '%s/%s' "$START" "$1" ;; esac; }

die() { echo "❌ $*" >&2; exit 1; }
CMD=${1:-}
case $CMD in
  prepare | image | launch | upgrade) ;;
  *) die "usage: deploy/mainnet/launch-mainnet.sh prepare|image|launch|upgrade (deploy/mainnet/README.md)" ;;
esac

# ---- what must never reach mainnet ----

# There is no reset: nothing here destroys an app, a machine, a volume or a secret.
[ -z "${RESET+set}" ] || die "RESET is set: mainnet's launcher has no reset, and never destroys an app, machine, volume or secret. Unset it."
# Leftovers from the testnet's day-2 commands: a coordinated stop or a network-halted override
# deployed by accident halts the chain or overrides slashing protection (docs/OPERATIONS.md).
for v in HALT_HEIGHT NETWORK_HALTED_AT GENESIS_ONLY; do
  [ -z "${!v:-}" ] || die "$v is set in this shell (a testnet leftover?): this script never deploys it. Unset it; coordinated stops follow docs/OPERATIONS.md \"Coordinated restart at a height\"."
done

need() { [ -n "${!1:-}" ] || die "$1 is required: $2"; }
need CHAIN_ID "blockula-1 (or a dry run's chain id)"
need ORG "the Fly organization mainnet runs in (its own: fly orgs create)"
need PREFIX "validator apps are \$PREFIX-v0 …"
need RPC_APP "the pruning public RPC node's app"
need ARCHIVE_APP "the archive RPC node's app"
[[ $CHAIN_ID =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] || die "CHAIN_ID $CHAIN_ID: lowercase letters, digits and '-'"
case $CHAIN_ID in blockula-testnet*) die "$CHAIN_ID is a public testnet: it's deployed by deploy/launch.sh" ;; esac
[[ $ORG =~ ^[a-z0-9][a-z0-9-]*$ ]] || die "ORG $ORG isn't a Fly organization slug"
[ "$ORG" != personal ] || die "ORG=personal is where the testnet runs: mainnet gets an organization of its own (fly orgs create), with only its operators in it and 2FA on"
# The public testnet's apps are blockula-v0 … and blockula-testnet (deploy/launch.sh's defaults):
# no name here may be one of them, or deploy over one.
testnet_name() { case $1 in blockula | blockula-testnet* | blockula-v*) return 0 ;; esac; return 1; }
for v in PREFIX RPC_APP ARCHIVE_APP; do
  [[ ${!v} =~ ^[a-z][a-z0-9-]{1,40}$ ]] || die "$v ${!v}: an app name is lowercase letters, digits and '-'"
  ! testnet_name "${!v}" || die "$v=${!v} is (or may become) one of the public testnet's apps (blockula-v0 …, blockula-testnet): mainnet's apps take names of their own"
done
for v in RPC_APP ARCHIVE_APP; do
  case ${!v} in "$PREFIX"-v[0-9]*) die "$v=${!v} is one of the validators' app names ($PREFIX-v0 …)" ;; esac
done
[ "$RPC_APP" != "$ARCHIVE_APP" ] || die "RPC_APP and ARCHIVE_APP are one app: the pruning and the archive node are two"
# The pruning node, by a name nothing rebinds: upgrade_rpc points the gate's RPC_APP at whichever
# RPC node it restarts, the archive included.
PUBLIC_APP=$RPC_APP
MAINNET=""; [ "$CHAIN_ID" != blockula-1 ] || MAINNET=1

read -r -a REGIONS <<< "${REGIONS:-iad sjc fra ams}"
N=${#REGIONS[@]}
[ "$N" -ge 1 ] || die "REGIONS names no region"
RPC_REGION=${RPC_REGION:-iad}
ARCHIVE_REGION=${ARCHIVE_REGION:-ord}
P2P_PORT=30300
HTTP_PORT=7070
RPC_PORT=7171
# Sizes (deploy/mainnet/README.md "Fly sizing and cost"; docs/OPERATIONS.md "Reference hardware"):
# dedicated CPUs and 16 GB everywhere, Fly's largest volumes. Validators keep 12 hours of blocks,
# so a flood of full blocks fits their volume (OPERATIONS.md "Disk sizing"); the public node keeps
# 14 days; the archive keeps everything.
VALIDATOR_VM=${VALIDATOR_VM:-performance-4x}
VALIDATOR_MEMORY=${VALIDATOR_MEMORY:-16gb}
VOLUME_GB=${VOLUME_GB:-500}
VALIDATOR_PRUNE_BLOCKS=${VALIDATOR_PRUNE_BLOCKS:-43200}
RPC_VM=${RPC_VM:-performance-4x}
RPC_MEMORY=${RPC_MEMORY:-16gb}
RPC_VOLUME_GB=${RPC_VOLUME_GB:-250}
RPC_PRUNE_BLOCKS=${RPC_PRUNE_BLOCKS:-1209600}
ARCHIVE_VOLUME_GB=${ARCHIVE_VOLUME_GB:-500}
for v in VOLUME_GB VALIDATOR_PRUNE_BLOCKS RPC_VOLUME_GB RPC_PRUNE_BLOCKS ARCHIVE_VOLUME_GB; do
  [[ ${!v} =~ ^[1-9][0-9]*$ ]] || die "$v must be a whole number (got '${!v}')"
done
if [ -n "$MAINNET" ]; then
  # No shared or burstable vCPUs on mainnet (docs/OPERATIONS.md "Reference hardware").
  for v in VALIDATOR_VM RPC_VM; do
    case ${!v} in performance-*) ;; *) die "$v=${!v}: mainnet runs on dedicated CPUs (performance-4x or larger)" ;; esac
  done
fi
LAUNCH_WINDOW_MINS=${LAUNCH_WINDOW_MINS:-60}
[[ $LAUNCH_WINDOW_MINS =~ ^[0-9]+$ ]] || die "LAUNCH_WINDOW_MINS must be a whole number of minutes"
STATE_DIR=$(abs "${STATE_DIR:-$HOME/.config/blockula/$CHAIN_ID-fly}")
KEYS_DIR=$(abs "${KEYS_DIR:-}")
case ${RELEASE_FROM:-} in http://* | https://* | "") ;; *) RELEASE_FROM=$(abs "$RELEASE_FROM") ;; esac
DRY=""; case ${DRY_RUN:-} in 1 | true) DRY=1 ;; "" | 0 | false) ;; *) die "DRY_RUN must be 1 or 0" ;; esac
REPO=jackdishman/blockula
TARGET=x86_64-unknown-linux-gnu # Fly machines
ARCHIVE_FILE=blockula-$TARGET.tar.gz
RPC_URL=https://$RPC_APP.fly.dev
VERIFIER=${VERIFIER:-blockula}
case $VERIFIER in */*) VERIFIER=$(abs "$VERIFIER") ;; esac
KEY_RERUN="deploy/mainnet/launch-mainnet.sh launch"
APPS=(); for i in $(seq 0 $((N - 1))); do APPS+=("$PREFIX-v$i"); done
WORK=$(mktemp -d "${TMPDIR:-/tmp}/blockula-mainnet.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

# The machine configurations (fly-config.sh: NET is where genesis.json is), the participation gate
# and the keys' removal from Fly, as the testnet uses them.
GENESIS_DIR=$(abs "${GENESIS_DIR:-$ROOT/genesis/$CHAIN_ID}")
NET=$GENESIS_DIR
# shellcheck source=deploy/fly-config.sh
source "$ROOT/deploy/fly-config.sh"
# shellcheck source=deploy/restart-gate.sh
source "$ROOT/deploy/restart-gate.sh"
# shellcheck source=deploy/key-removal.sh
source "$ROOT/deploy/key-removal.sh"

# ---- fly, and DRY_RUN ----

# Every fly command goes through here. With DRY_RUN it's printed, a deploy's configuration (-c)
# with it, and only reads run.
fly() {
  if [ -z "$DRY" ]; then command fly "$@"; return; fi
  local args=("$@") k cfg="" plan="DRY_RUN fly $*"
  for ((k = 0; k < ${#args[@]} - 1; k++)); do [ "${args[k]}" != -c ] || cfg=${args[k + 1]}; done
  if [ "$1" = deploy ] && [ -f "$cfg" ]; then plan+=$'\n'$(sed 's/^/    | /' "$cfg"); fi
  # One write: the validators' deploys run in parallel.
  printf '%s\n' "$plan" >&2
  case "$1 ${2:-}" in
    "auth whoami" | "apps list" | "ips list" | "volumes list" | "machines list" | "secrets list" | "certs list") command fly "$@" ;;
    "secrets import") cat > /dev/null ;; # the values never print
    *) return 0 ;;
  esac
}
# A read that answers JSON: in a dry run, a read about an app that doesn't exist yet reads as empty.
fly_json() {
  if [ -z "$DRY" ]; then command fly "$@"; return; fi
  echo "DRY_RUN fly $*" >&2
  command fly "$@" 2>/dev/null || echo '[]'
}
# Writes a file under STATE_DIR (never in a dry run).
mark() { [ -n "$DRY" ] || { (umask 077; mkdir -p "$STATE_DIR"); printf '%s\n' "$2" > "$STATE_DIR/$1"; }; }

utc() { date -u -r "$1" '+%Y-%m-%d %H:%M UTC' 2>/dev/null || date -u -d "@$1" '+%Y-%m-%d %H:%M UTC'; }
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

# ---- Fly apps, volumes and IPs ----

app_exists() { fly_json apps list --org "$ORG" --json | jq -e --arg a "$1" '.[] | select(.Name == $a)' >/dev/null; }
ensure_app() { app_exists "$1" || fly apps create "$1" --org "$ORG" >/dev/null || die "creating $1 in $ORG failed: the name may be taken in another organization (fly apps list)"; }
require_app() { app_exists "$1" || die "$1 isn't an app in $ORG: run deploy/mainnet/launch-mainnet.sh prepare first"; }
volume_count() { fly_json volumes list -a "$1" --json | jq length; }
# One volume per app. A validator's never has scheduled snapshots: a restored snapshot is an old
# copy of its signing state, and starting from it can double-sign (deploy/README.md, Caveats).
ensure_volume() { # app region size [validator]
  local count
  count=$(volume_count "$1")
  if [ "$count" = 0 ]; then
    if [ "${4:-}" = validator ]; then
      fly volumes create data -a "$1" -r "$2" -s "$3" --scheduled-snapshots=false --yes >/dev/null
    else
      fly volumes create data -a "$1" -r "$2" -s "$3" --yes >/dev/null
    fi
  elif [ "$count" != 1 ]; then
    die "$1 has $count volumes: an app here has exactly one (never fork or clone a validator's volume)"
  fi
  [ "${4:-}" != validator ] || validator_snapshots_off "$1"
}
validator_snapshots_off() { fly_json volumes list -a "$1" --json | jq -r '.[].id' | while read -r v; do fly volumes update "$v" -a "$1" --scheduled-snapshots=false >/dev/null; done; }
v4_of() { fly_json ips list -a "$1" --json | jq -r '[.[] | select(.Type == "v4")][0].Address // empty'; }
# A validator's dedicated IPv4: p2p is raw TCP, which Fly's shared IPv4 doesn't carry.
dedicated_v4() {
  local ip
  ip=$(v4_of "$1")
  if [ -z "$ip" ]; then
    fly ips allocate-v4 -a "$1" --yes >/dev/null
    ip=$(v4_of "$1")
  fi
  echo "${ip:-<allocated by this run>}"
}
# An RPC node's public addresses for HTTPS (and its custom domain's DNS records).
public_ips() {
  local ips
  ips=$(fly_json ips list -a "$1" --json)
  jq -e 'any(.[]; .Type == "v6")' >/dev/null <<<"$ips" || fly ips allocate-v6 -a "$1" >/dev/null
  jq -e 'any(.[]; .Type == "v4" or .Type == "shared_v4")' >/dev/null <<<"$ips" || fly ips allocate-v4 --shared -a "$1" --yes >/dev/null
}
ensure_cert() { # app domain
  fly_json certs list -a "$1" --json | jq -e --arg d "$2" 'any(.[]; .Hostname == $d)' >/dev/null || fly certs add "$2" -a "$1" >/dev/null
}
machine_count() { fly_json machines list -a "$1" --json | jq length; }

# ---- the checkout, the release and the genesis ----

# The configuration this script deploys (fly-config.sh, entrypoint.sh, the Dockerfile, the
# genesis) is the reviewed release's: this checkout must be the tag, with nothing changed or added.
check_checkout() {
  [[ ${RELEASE:-} =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "RELEASE must be the release to run, vX.Y.Z (got '${RELEASE:-}')"
  git rev-parse --git-dir >/dev/null 2>&1 || die "$ROOT isn't a git checkout"
  [ -z "$(git status --porcelain)" ] || die "this checkout has changes (git status): mainnet is deployed from the release's tag with nothing changed or added"
  local head tag
  head=$(git rev-parse HEAD)
  tag=$(git rev-parse -q --verify "refs/tags/$RELEASE^{commit}") || die "no tag $RELEASE in this checkout: git fetch --tags && git checkout $RELEASE"
  [ "$head" = "$tag" ] || die "this checkout is $head, not $RELEASE ($tag): git checkout $RELEASE"
}

# The release's Linux archive, checked, and its binary: BINARY, BINARY_SHA.
verify_release() {
  local rel=$WORK/release got want
  local version="blockula ${RELEASE#v}"
  got=$("$VERIFIER" --version 2>/dev/null) || die "VERIFIER ($VERIFIER) doesn't run: install $RELEASE with \`blockula release install $RELEASE\` (it checks the signature), or point VERIFIER at it"
  [ "$got" = "$version" ] || die "VERIFIER ($VERIFIER) is $got, not $version: the release's own binary checks it, rebuilds the genesis and matches the keys (blockula release install $RELEASE)"
  local files=(--file "$ARCHIVE_FILE")
  # A chain the release ships a genesis for (genesis/<chain id>/): the release's copy too.
  if [ -f "$ROOT/genesis/$CHAIN_ID/genesis.json" ]; then files+=(--file "genesis-$CHAIN_ID.json"); fi
  if [ "${UNSIGNED_RELEASE:-}" = 1 ]; then
    [ -z "$MAINNET" ] || die "UNSIGNED_RELEASE=1: blockula-1 runs signed releases only (the release key ceremony comes first, docs/OPERATIONS.md \"The release key\")"
    echo "⚠️  UNSIGNED_RELEASE: checking $RELEASE against its checksums.txt and build provenance, without a signature (a dry run before the release key exists)" >&2
    mkdir -p "$rel"
    local f names=(checksums.txt)
    for f in "${files[@]}"; do [ "$f" = --file ] || names+=("$f"); done
    for f in "${names[@]}"; do
      case ${RELEASE_FROM:-} in
        "") ;;
        http://* | https://*) curl -fsSL -o "$rel/$f" "${RELEASE_FROM%/}/$f" || die "couldn't download $f from $RELEASE_FROM" ;;
        *) cp "${RELEASE_FROM%/}/$f" "$rel/$f" 2>/dev/null || die "no $f in $RELEASE_FROM" ;;
      esac
    done
    if [ -z "${RELEASE_FROM:-}" ]; then
      local patterns=(); for f in "${names[@]}"; do patterns+=(--pattern "$f"); done
      gh release download "$RELEASE" --repo "$REPO" --dir "$rel" "${patterns[@]}" || die "couldn't download $RELEASE's files (gh release download)"
    fi
    [ "$(head -n 1 "$rel/checksums.txt")" = "# blockula-release $RELEASE" ] || die "$RELEASE's checksums.txt doesn't name $RELEASE on its first line"
    for f in "${names[@]}"; do
      [ "$f" != checksums.txt ] || continue
      want=$(awk -v f="$f" '$2 == f { print $1 }' "$rel/checksums.txt")
      [[ $want =~ ^[0-9a-f]{64}$ ]] || die "$RELEASE's checksums.txt lists $f not exactly once"
      [ "$(sha256 "$rel/$f")" = "$want" ] || die "$f doesn't match $RELEASE's checksums.txt"
    done
    if [ "${PROVENANCE:-1}" != 0 ]; then
      gh attestation verify "$rel/$ARCHIVE_FILE" --repo "$REPO" --signer-workflow "$REPO/.github/workflows/release.yml" --source-ref "refs/tags/$RELEASE" >/dev/null \
        || die "$ARCHIVE_FILE has no build provenance from $RELEASE's tag (gh attestation verify)"
    fi
  else
    # The signature against the pinned release key id, checksums.txt's first line, each file's
    # SHA-256; only what matches lands in the directory (docs/OPERATIONS.md "To an offline machine").
    "$VERIFIER" release fetch "$RELEASE" --dir "$rel" "${files[@]}" ${RELEASE_FROM:+--from "$RELEASE_FROM"} >&2 \
      || die "$RELEASE didn't verify against the release key $VERIFIER trusts (above): nothing deployed"
  fi
  if [ -f "$ROOT/genesis/$CHAIN_ID/genesis.json" ]; then
    cmp -s "$rel/genesis-$CHAIN_ID.json" "$ROOT/genesis/$CHAIN_ID/genesis.json" || die "$RELEASE's genesis-$CHAIN_ID.json isn't this checkout's genesis/$CHAIN_ID/genesis.json"
  fi
  tar -xzf "$rel/$ARCHIVE_FILE" -C "$rel" "blockula-$TARGET/blockula" || die "$ARCHIVE_FILE holds no blockula-$TARGET/blockula"
  BINARY=$rel/blockula-$TARGET/blockula
  BINARY_SHA=$(sha256 "$BINARY")
  echo "✓ $RELEASE verified: $ARCHIVE_FILE, blockula sha256 $BINARY_SHA"
}

# The image: the verified binary and the release's entrypoint on a digest-pinned base
# (deploy/mainnet/Dockerfile, which checks the binary's SHA-256 and version again). IMAGE.
build_image() {
  local ctx=$WORK/image label
  mkdir -p "$ctx"
  cp "$BINARY" "$ctx/blockula"
  cp "$ROOT/deploy/entrypoint.sh" "$ctx/entrypoint.sh"
  cp "$ROOT/deploy/mainnet/Dockerfile" "$ctx/Dockerfile"
  printf 'app = "%s"\n' "$RPC_APP" > "$WORK/build.toml"
  label=$RELEASE-${BINARY_SHA:0:12}
  IMAGE=registry.fly.io/$RPC_APP:$label
  echo "🔨 Building $IMAGE from the verified $RELEASE binary…"
  fly deploy "$ctx" -c "$WORK/build.toml" --dockerfile "$ctx/Dockerfile" --depot=false \
    --build-arg BLOCKULA_SHA256="$BINARY_SHA" --build-arg BLOCKULA_VERSION="${RELEASE#v}" \
    --build-only --push --image-label "$label" >/dev/null || die "the image build failed (above): nothing deployed"
}

# The genesis this release pins: GENESIS (its file), GENESIS_HASH, GENESIS_MS (genesis time).
check_genesis() {
  GENESIS_DIR=$(cd "$GENESIS_DIR" 2>/dev/null && pwd) || die "no genesis directory $GENESIS_DIR"
  NET=$GENESIS_DIR
  [ -z "$MAINNET" ] || [ "$GENESIS_DIR" = "$ROOT/genesis/blockula-1" ] || die "blockula-1's genesis is the one the release ships, genesis/blockula-1: unset GENESIS_DIR"
  GENESIS=$GENESIS_DIR/genesis.json
  [ -f "$GENESIS" ] && [ -f "$GENESIS_DIR/spec.toml" ] || die "$GENESIS_DIR holds no genesis.json and spec.toml"
  local chain prefix pin pinned
  chain=$(jq -r .chain_id "$GENESIS")
  [ "$chain" = "$CHAIN_ID" ] || die "$GENESIS is $chain's, not $CHAIN_ID's"
  prefix=$(jq -r .address_prefix "$GENESIS")
  [ -z "$MAINNET" ] || [ "$prefix" = boo ] || die "$GENESIS uses $prefix addresses: blockula-1's are boo"
  GENESIS_HASH=$("$VERIFIER" checkpoint --genesis "$GENESIS") || die "$VERIFIER couldn't hash $GENESIS"
  [[ $GENESIS_HASH =~ ^[0-9a-f]{64}$ ]] || die "$VERIFIER printed '$GENESIS_HASH' for $GENESIS's hash"
  pin=$ROOT/node/checkpoints/$CHAIN_ID.json
  if [ -f "$pin" ]; then
    pinned=$(jq -r '.genesis_hash // ""' "$pin")
    [ -n "$pinned" ] || die "$CHAIN_ID isn't pinned in this release ($pin has no genesis_hash): its binary refuses the chain. The launch release pins it (scripts/pin-network.sh --mainnet)"
    [ "$pinned" = "$GENESIS_HASH" ] || die "$GENESIS hashes to $GENESIS_HASH, but this release pins $CHAIN_ID to $pinned"
  else
    [ -z "$MAINNET" ] || die "no $pin: this release doesn't know blockula-1"
    echo "⚠️  $CHAIN_ID isn't pinned in this release: the RPC nodes join it by GENESIS_HASH $GENESIS_HASH"
  fi
  # Byte for byte from its public inputs, as every genesis builder did (GENESIS_CEREMONY.md, section 5).
  (cd "$GENESIS_DIR" && "$VERIFIER" genesis build --spec spec.toml --out "$WORK/rebuilt.json" >/dev/null 2>&1) && cmp -s "$GENESIS" "$WORK/rebuilt.json" \
    || die "$GENESIS_DIR/spec.toml doesn't rebuild $GENESIS byte for byte with $VERIFIER"
  GENESIS_MS=$(jq -r .created_at_ms "$GENESIS")
  [[ $GENESIS_MS =~ ^[0-9]+$ ]] || die "$GENESIS has no created_at_ms"
  echo "✓ genesis $GENESIS_HASH ($CHAIN_ID, $prefix addresses, genesis time $(utc $((GENESIS_MS / 1000))))"
}

# KEYS_DIR holds the founder seats' consensus keys and nothing else: v0/validator.key …, one per
# app, mode 600 in directories of mode 700. An owner key, the release key or a backup found there
# is refused: they never go near an online machine (docs/GENESIS_CEREMONY.md, section 2). It must
# hold the key of each seat whose key the run stages (the indexes given; a seat re-seeded on day 2
# needs only its own), and any other key in it is checked all the same (match_keys).
check_keys_dir() { # index…
  need KEYS_DIR "v0/validator.key …: the founder seats' consensus keys, generated offline"
  [ -d "$KEYS_DIR" ] || die "KEYS_DIR $KEYS_DIR isn't a directory"
  local f open
  open=$(find "$KEYS_DIR" -perm -004 -o -perm -040 -o -perm -002 -o -perm -020 | head -n 1)
  [ -z "$open" ] || die "$open is readable or writable by others: KEYS_DIR is mode 700, its keys 600"
  while IFS= read -r f; do
    case ${f#"$KEYS_DIR"/} in
      v[0-9] | v[0-9][0-9]) [ -d "$f" ] && [ "${f##*/v}" -lt "$N" ] || die "KEYS_DIR has $f: it holds v0 … v$((N - 1)) (one per validator app) and nothing else" ;;
      v[0-9]/validator.key | v[0-9][0-9]/validator.key) [ -f "$f" ] || die "$f isn't a file" ;;
      *) die "KEYS_DIR has $f: it holds the founder seats' consensus keys (vN/validator.key) and nothing else. Owner keys, the release key and backups stay offline." ;;
    esac
  done < <(find "$KEYS_DIR" -mindepth 1)
  for i in "$@"; do
    f=$KEYS_DIR/v$i/validator.key
    [ -f "$f" ] || die "no $f: $PREFIX-v$i is deployed with its consensus key"
  done
  for i in $(seq 0 $((N - 1))); do
    f=$KEYS_DIR/v$i/validator.key
    [ -f "$f" ] || continue
    grep -qxE '[0-9a-f]{64}' "$f" && [ "$(wc -l < "$f")" -le 1 ] || die "$f isn't a validator.key (one line: the 64-hex-digit seed)"
  done
}

# Each validator app's genesis validator is the one whose p2p address is that app's IPv4, and its
# key in KEYS_DIR, if there, is that validator's: the key's public key and address, rebuilt by
# VERIFIER (`genesis operator`, which reads the key and signs nothing that leaves this machine),
# are the genesis's (a dry run without KEYS_DIR checks no key). Every other genesis validator is
# in OUTSIDE_VALIDATORS. OURS: our validators' addresses, by index.
match_keys() {
  local i ip v pk addr http owner got theirs want
  OURS=()
  for i in $(seq 0 $((N - 1))); do
    ip=$(v4_of "$PREFIX-v$i")
    [ -n "$ip" ] || die "$PREFIX-v$i has no dedicated IPv4: run deploy/mainnet/launch-mainnet.sh prepare (before the operator files)"
    v=$(jq -c --arg p "$ip:$P2P_PORT" '[.validators[] | select(.p2p_addr == $p)]' "$GENESIS")
    [ "$(jq length <<<"$v")" = 1 ] || die "$PREFIX-v$i's IPv4 is $ip, but no genesis validator's p2p address is $ip:$P2P_PORT (one per app: its operator file took the address prepare printed)"
    pk=$(jq -r '.[0].public_key' <<<"$v"); addr=$(jq -r '.[0].address' <<<"$v"); http=$(jq -r '.[0].http_addr' <<<"$v")
    owner=$(jq -r --arg k "$pk" '[.operators[] | select(.consensus_key == $k)][0].owner // empty' "$GENESIS")
    [ -n "$owner" ] || die "$GENESIS has no operator entry for $addr"
    if [ -n "$KEYS_DIR" ] && [ -f "$KEYS_DIR/v$i/validator.key" ]; then
      got=$("$VERIFIER" genesis operator --chain-id "$CHAIN_ID" --key "$KEYS_DIR/v$i/validator.key" --owner "$owner" --p2p-addr "$ip:$P2P_PORT" --http-addr "$http" --json 2>/dev/null) \
        || die "$VERIFIER couldn't read $KEYS_DIR/v$i/validator.key"
      [ "$(jq -r .keys.consensus_public_key <<<"$got")" = "$pk" ] && [ "$(jq -r .keys.consensus_address <<<"$got")" = "$addr" ] \
        || die "$KEYS_DIR/v$i/validator.key isn't the key of $PREFIX-v$i's genesis validator ($addr, p2p $ip:$P2P_PORT): a key on the wrong app can't link to anyone. Check which seat each key is."
    fi
    OURS+=("$addr")
    echo "   $PREFIX-v$i  ${REGIONS[$i]}  $ip  $addr"
    [ "$http" = "https://${RPC_DOMAIN:-$RPC_APP.fly.dev}" ] || echo "⚠️  $addr publishes $http as its HTTP endpoint, not https://${RPC_DOMAIN:-$RPC_APP.fly.dev}: set RPC_DOMAIN to it (or change it later with an owner's update_validator)"
  done
  theirs=$(jq -r --arg ours "${OURS[*]}" '[.validators[].address] - ($ours | split(" ")) | sort | join(" ")' "$GENESIS")
  want=$(printf '%s\n' ${OUTSIDE_VALIDATORS:-} | sort | paste -sd ' ' -)
  [ "$theirs" = "$want" ] || die "the genesis's other validators are '${theirs}', but OUTSIDE_VALIDATORS is '${OUTSIDE_VALIDATORS:-}': name every validator other operators run (the gate counts on them)"
}

# ---- deploys ----

validator_config() { validator_toml "$1" > "$WORK/$PREFIX-v$1.toml"; echo "$WORK/$PREFIX-v$1.toml"; }
# The RPC nodes' configurations: the pruning one (PUBLIC_APP) and the archive (ARCHIVE_APP), which
# replays the chain from genesis (deploy/entrypoint.sh) and so also syncs from the pruning node:
# the validators keep only VALIDATOR_PRUNE_BLOCKS blocks, the pruning node RPC_PRUNE_BLOCKS. Each
# is the same whatever RPC_APP is bound to when it's made (launch, or upgrade_rpc).
rpc_config() { # app
  if [ "$1" = "$ARCHIVE_APP" ]; then
    (RPC_SYNC_PEERS="http://$PUBLIC_APP.internal:$RPC_PORT" RPC_APP=$ARCHIVE_APP RPC_REGION=$ARCHIVE_REGION RPC_ARCHIVE=1; rpc_toml) > "$WORK/$1.toml"
  else
    (RPC_APP=$PUBLIC_APP RPC_ARCHIVE=""; rpc_toml) > "$WORK/$1.toml"
  fi
  echo "$WORK/$1.toml"
}
rpc_url_of() { echo "https://$1.fly.dev"; }
forwarder_secret() {
  # Authenticates the RPC nodes' forwarded transactions to the validators (client IPs for rate
  # limits): a shared HMAC secret, not a key of the chain's.
  local f=$STATE_DIR/forwarder-secret
  if [ -n "$DRY" ]; then echo "<forwarder secret>"; return; fi
  [ -f "$f" ] || { (umask 077; mkdir -p "$STATE_DIR"; openssl rand -hex 32 > "$f"); }
  cat "$f"
}
# Stages app $1's secrets through stdin (never argv, which other local users can read): the
# validator key (base64 of KEYS_DIR/v$2/validator.key) for a validator, and the forwarder secret.
# A dry run reads no key.
stage_secrets() { # app [index]
  local fwd key="<base64 of KEYS_DIR/v${2:-}/validator.key>"
  fwd=$(forwarder_secret)
  if [ -n "${2:-}" ]; then
    [ -n "$DRY" ] || key=$(base64 < "$KEYS_DIR/v$2/validator.key" | tr -d '\n')
    printf '%s=%s\n' VALIDATOR_KEY_B64 "$key" BLOCKULA_FORWARDER_SECRET "$fwd" | fly secrets import -a "$1" --stage >/dev/null
  else
    printf '%s=%s\n' BLOCKULA_FORWARDER_SECRET "$fwd" | fly secrets import -a "$1" --stage >/dev/null
  fi
  mark "staged-$1" "$(date -u +%FT%TZ)"
}
# An RPC node's first deploy. Its Fly health check (/api/health) passes only once it serves a fresh
# head: not before genesis time, nor while it replays the chain (the archive) or catches up, and
# flyctl would wait for it (--wait-timeout, 5 minutes) and then call the deploy failed. So flyctl
# doesn't wait (--detach), the machine must exist, and the participation gate waits for the
# public node's head.
deploy_rpc() { # app
  echo "🌐 Deploying ${1}…"
  fly deploy -c "$(rpc_config "$1")" --image "$IMAGE" --ha=false --detach >/dev/null || die "deploying $1 failed (fly logs -a $1): rerun $KEY_RERUN"
  [ -n "$DRY" ] || [ "$(machine_count "$1")" = 1 ] || die "$1 has no machine after its deploy (fly logs -a $1): rerun $KEY_RERUN"
}
# A validator about to be deployed whose key was staged once (STATE_DIR/staged-<app>) and has
# since left Fly was seeded onto a volume, and needs that volume. One created after the staging
# (the old one lost) holds no key: a machine on it would refuse to start, and staging the key again
# is an operator's decision, never a rerun's (deploy/mainnet/README.md, Day 2).
check_staged() { # index
  local app=$PREFIX-v$1 staged vol
  [ -f "$STATE_DIR/staged-$app" ] || return 0
  ! has_secret "$app" VALIDATOR_KEY_B64 || return 0
  staged=$(cat "$STATE_DIR/staged-$app")
  vol=$(fly_json volumes list -a "$app" --json | jq -r '.[0].created_at // ""')
  # As UTC digits, YYYYMMDDhhmmss (Fly's created_at and the marker are both ISO 8601 in UTC).
  [ "$(printf '%s' "${vol:0:19}" | tr -dc 0-9)" -gt "$(printf '%s' "${staged:0:19}" | tr -dc 0-9)" ] 2>/dev/null || return 0
  die "$app's volume was created $vol, after its key was staged ($staged) and removed from Fly once seeded: it holds no key, and a machine on it would refuse to start. If the volume the key was seeded onto is gone for good and nothing anywhere runs this seat's key, delete $STATE_DIR/staged-$app and rerun with KEYS_DIR: the key is staged again (deploy/mainnet/README.md, Day 2)."
}
# Prometheus targets for deploy/monitoring, over Fly's private network (a WireGuard peer in the org).
write_targets() {
  [ -z "$DRY" ] || return 0
  {
    echo "# $CHAIN_ID scrape targets (deploy/monitoring/prometheus/prometheus.yml, job \"blockula\")."
    echo "- targets: ['$RPC_APP.internal:$RPC_PORT']"
    echo "  labels: { node: '$RPC_APP' }"
    echo "- targets: ['$ARCHIVE_APP.internal:$RPC_PORT']"
    echo "  labels: { node: '$ARCHIVE_APP' }"
    for i in $(seq 0 $((N - 1))); do
      echo "- targets: ['$PREFIX-v$i.internal:$HTTP_PORT']"
      echo "  labels: { node: '$PREFIX-v$i' }"
    done
  } > "$STATE_DIR/prometheus-targets.yml"
}

# ---- prepare ----

prepare() {
  fly auth whoami >/dev/null || die "fly auth login"
  echo "🪦 Apps, volumes and IPs in ${ORG}…"
  for app in "${APPS[@]}" "$RPC_APP" "$ARCHIVE_APP"; do ensure_app "$app"; done
  ensure_volume "$RPC_APP" "$RPC_REGION" "$RPC_VOLUME_GB"
  ensure_volume "$ARCHIVE_APP" "$ARCHIVE_REGION" "$ARCHIVE_VOLUME_GB"
  public_ips "$RPC_APP"
  public_ips "$ARCHIVE_APP"
  [ -z "${RPC_DOMAIN:-}" ] || ensure_cert "$RPC_APP" "$RPC_DOMAIN"
  [ -z "${ARCHIVE_DOMAIN:-}" ] || ensure_cert "$ARCHIVE_APP" "$ARCHIVE_DOMAIN"
  local addrs="" ip
  for i in $(seq 0 $((N - 1))); do
    ensure_volume "$PREFIX-v$i" "${REGIONS[$i]}" "$VOLUME_GB" validator
    ip=$(dedicated_v4 "$PREFIX-v$i")
    addrs="$addrs$PREFIX-v$i ${REGIONS[$i]} $ip:$P2P_PORT"$'\n'
  done
  mark p2p-addrs "$addrs"
  echo "✓ Each founder seat's operator file (docs/GENESIS_CEREMONY.md, section 2, step 4) takes its app's p2p address:"
  printf '%s' "$addrs" | sed 's/^/   /'
  echo "  and --http-addr https://${RPC_DOMAIN:-$RPC_APP.fly.dev} (the public RPC node: validators' HTTP stays on Fly's private network)."
  [ -z "${RPC_DOMAIN:-}${ARCHIVE_DOMAIN:-}" ] || echo "  DNS for the custom domains: fly certs show <domain> -a <app> says which records to set."
}

# ---- launch ----

launch() {
  check_checkout
  check_genesis
  local now_ms i k app deploy_v=() deploy_r=() stage_v=() pids=() failed="" first finished
  fly auth whoami >/dev/null || die "fly auth login"
  for app in "${APPS[@]}" "$RPC_APP" "$ARCHIVE_APP"; do require_app "$app"; done
  # One machine and one volume per validator app, ever: two copies of a key double-sign.
  for i in $(seq 0 $((N - 1))); do
    app=$PREFIX-v$i
    [ "$(volume_count "$app")" = 1 ] || die "$app has $(volume_count "$app") volumes, not 1: run prepare (never fork or clone a validator's volume)"
    validator_snapshots_off "$app"
    case $(machine_count "$app") in
      0) deploy_v+=("$i") ;;
      1) ;;
      *) die "$app has more than one machine: two copies of its key would double-sign. Stop all but one (fly machines list -a $app) before anything else." ;;
    esac
  done
  for app in "$RPC_APP" "$ARCHIVE_APP"; do
    [ "$(volume_count "$app")" = 1 ] || die "$app has $(volume_count "$app") volumes, not 1: run prepare"
    [ "$(machine_count "$app")" != 0 ] || deploy_r+=("$app")
  done
  # The launch finished once its gate passed (STATE_DIR/finished): a validator deployed after that
  # (its machine gone, or its volume lost and its key staged again: README.md, Day 2) joins a
  # running chain.
  finished=$(cat "$STATE_DIR/finished" 2>/dev/null || true)
  # Validators still to deploy: within the window before genesis time, and their keys (an RPC
  # node's rebuild needs neither).
  if [ ${#deploy_v[@]} -gt 0 ]; then
    now_ms=$(($(date +%s) * 1000))
    if [ -n "$finished" ]; then
      echo "🧛 The launch finished (with $finished): deploying$(for i in "${deploy_v[@]}"; do printf ' %s' "$PREFIX-v$i"; done) into the running chain with $RELEASE, which must be the release the other nodes run"
    elif [ "$now_ms" -lt $((GENESIS_MS - LAUNCH_WINDOW_MINS * 60000)) ]; then
      local early
      early="genesis time is $(utc $((GENESIS_MS / 1000))), $(((GENESIS_MS - now_ms) / 60000)) min away: validators start at most $LAUNCH_WINDOW_MINS min before it (they wait for it: consensus proposes no block before its parent's time plus a block time). Run this from $(utc $(((GENESIS_MS - LAUNCH_WINDOW_MINS * 60000) / 1000))); prepare and image can run any time."
      # A dry run plans the launch any time before it (deploy/mainnet/README.md, the day before and T − 2 h).
      if [ -n "$DRY" ]; then echo "DRY_RUN: the launch itself refuses until then: $early" >&2; else die "$early"; fi
    elif [ "$now_ms" -gt "$GENESIS_MS" ]; then
      echo "⚠️  genesis time passed $(((now_ms - GENESIS_MS) / 60000)) min ago: the chain starts once its validators are up, that far behind its clock for good"
    fi
    # The keys of the seats whose key this run stages. One staged before (STATE_DIR/staged-<app>)
    # needs none: its key is in Fly still, or on its volume (check_staged).
    for i in "${deploy_v[@]}"; do [ -f "$STATE_DIR/staged-$PREFIX-v$i" ] || stage_v+=("$i"); done
    if [ -n "$KEYS_DIR" ] || { [ ${#stage_v[@]} -gt 0 ] && [ -z "$DRY" ]; }; then
      check_keys_dir ${stage_v[@]+"${stage_v[@]}"}
      echo "🔑 The keys in $KEYS_DIR against the genesis, by each app's IPv4:"
    else
      # The day before, the keys are still encrypted: a dry run plans without them.
      [ ${#stage_v[@]} -eq 0 ] || echo "⚠️  DRY_RUN without KEYS_DIR: no key is read or checked against the genesis (the launch itself needs KEYS_DIR)" >&2
      echo "🔑 Each app's genesis validator, by its IPv4:"
    fi
    match_keys
    for i in "${deploy_v[@]}"; do check_staged "$i"; done
  fi
  # The genesis validators all start with the release the launch started with (an RPC node
  # rebuilt later, or a validator once the launch finished, runs whatever release the rest do).
  first=$(cat "$STATE_DIR/launched" 2>/dev/null || true)
  if [ -n "$first" ] && [ -z "$finished" ] && [ "$first" != "$RELEASE" ] && [ ${#deploy_v[@]} -gt 0 ]; then
    die "this launch started with $first: finish it with RELEASE=$first, then upgrade"
  fi

  # The image first: a release that doesn't verify leaves nothing deployed and no key staged.
  if [ $((${#deploy_v[@]} + ${#deploy_r[@]})) -gt 0 ]; then
    verify_release
    build_image
  fi
  # The RPC nodes before any key is staged: a deploy of theirs that fails leaves no key in Fly.
  for app in ${deploy_r[@]+"${deploy_r[@]}"}; do
    [ -f "$STATE_DIR/staged-$app" ] || stage_secrets "$app"
    deploy_rpc "$app"
  done
  if [ ${#deploy_v[@]} -gt 0 ]; then
    for i in "${deploy_v[@]}"; do
      [ -f "$STATE_DIR/staged-$PREFIX-v$i" ] || stage_secrets "$PREFIX-v$i" "$i"
    done
    # From here validators run: the launch is finished with this release, never redone.
    mark launched "$RELEASE"
    for i in "${deploy_v[@]}"; do
      app=$PREFIX-v$i
      echo "🧛 Deploying $app (${REGIONS[$i]})…"
      # --ha=false: exactly one machine per validator. Never scale a validator app.
      fly deploy -c "$(validator_config "$i")" --image "$IMAGE" --ha=false >/dev/null &
      pids+=($!)
    done
    for k in $(seq 0 $((${#pids[@]} - 1))); do wait "${pids[$k]}" || failed="$failed $PREFIX-v${deploy_v[$k]}"; done
    [ -z "$failed" ] || die "deploying$failed failed (fly logs -a <app>): rerun $KEY_RERUN"
  fi
  # Which validators still hold their key in Fly, once the deploys applied the staged secrets.
  keys_in_fly
  write_targets
  if [ -n "$DRY" ]; then
    echo "DRY_RUN: then VALIDATOR_KEY_B64 comes off Fly from each validator holding it once its volume shows it seeded (one restart of them together, deploy/key-removal.sh), and the run waits until every validator signs, links and proposes (deploy/restart-gate.sh)"
    return 0
  fi
  # Before genesis time the restart pauses nothing; the gate after it waits for genesis too.
  now_ms=$(($(date +%s) * 1000))
  [ "$now_ms" -ge "$GENESIS_MS" ] || GATE_SECS=$((GATE_SECS + (GENESIS_MS - now_ms) / 1000))
  remove_seeded_keys || exit 1
  gate_wait "the chain at full participation" gate_check || die "$GATE_WHY: check fly logs -a <app> (deploy/mainnet/README.md, launch day)"
  [ -n "$finished" ] || mark finished "$RELEASE"
  # The gate read the pruning node; the archive replays from genesis at its own pace.
  for app in "$RPC_APP" "$ARCHIVE_APP"; do
    curl -fsS -m 10 "$(rpc_url_of "$app")/api/status" | jq -c --arg app "$app" '{app: $app, chain_id, genesis_hash, height, syncing, version}' \
      || echo "⚠️  couldn't reach $(rpc_url_of "$app") from here (DNS can take a few minutes; fly logs -a $app)"
  done
  echo "✓ $CHAIN_ID runs: $RPC_URL and $(rpc_url_of "$ARCHIVE_APP") (genesis $GENESIS_HASH). Next: deploy/mainnet/README.md, after genesis."
}

# ---- upgrade ----

# One RPC node, on its own: the gate's public node is that node while it restarts.
upgrade_rpc() { # app
  local start
  RPC_APP=$1 RPC_URL=$(rpc_url_of "$1")
  gate_wait "before restarting $1" gate_answering rpc || die "$GATE_WHY: $1 not restarted"
  start=$GATE_RPC_STARTED
  echo "🌐 Upgrading ${1}…"
  fly deploy -c "$(rpc_config "$1")" --image "$IMAGE" --ha=false >/dev/null || die "deploying $1 failed (fly logs -a $1)"
  gate_wait "after restarting $1" gate_public_back "$start" || die "$1 isn't back: $GATE_WHY. Check fly logs -a $1."
}

upgrade() {
  check_checkout
  check_genesis
  local i app start
  fly auth whoami >/dev/null || die "fly auth login"
  for app in "${APPS[@]}" "$RPC_APP" "$ARCHIVE_APP"; do
    require_app "$app"
    [ "$(machine_count "$app")" = 1 ] || die "$app has $(machine_count "$app") machines, not 1: finish the launch first ($KEY_RERUN); never scale an app here"
  done
  keys_in_fly
  if [ ${#SEEDING[@]} -gt 0 ]; then
    local held=""
    for i in "${SEEDING[@]}"; do held="$held $PREFIX-v$i"; done
    die "VALIDATOR_KEY_B64 is still in Fly for$held: finish the launch first ($KEY_RERUN removes it)"
  fi
  verify_release
  build_image
  if [ -n "$DRY" ]; then
    for i in $(seq 0 $((N - 1))); do echo "DRY_RUN: through the gate, then fly deploy -c $PREFIX-v$i.toml --image $IMAGE --ha=false"; done
    echo "DRY_RUN: then $RPC_APP and $ARCHIVE_APP, each through the gate"
    return 0
  fi
  gate_wait "before the first restart" gate_check || die "the network isn't at full participation: $GATE_WHY. Nothing restarted."
  for i in $(seq 0 $((N - 1))); do
    app=$PREFIX-v$i
    start=${GATE_STARTED[$i]:-}
    [ -n "$start" ] || die "no process start time for $app: nothing more restarted"
    echo "🧛 Upgrading $app to ${RELEASE}…"
    fly deploy -c "$(validator_config "$i")" --image "$IMAGE" --ha=false >/dev/null || die "deploying $app failed (fly logs -a $app): the validators after it still run the old release"
    gate_wait "after restarting $app" gate_check "$i=$start" \
      || die "$app isn't back: $GATE_WHY. Stopping before the next app (the rest are untouched): check fly logs -a $app, then rerun."
  done
  (upgrade_rpc "$RPC_APP") || exit 1
  (upgrade_rpc "$ARCHIVE_APP") || exit 1
  echo "✓ $RELEASE on $N validators, $RPC_APP and $ARCHIVE_APP"
}

case $CMD in
  prepare) prepare ;;
  image) check_checkout; verify_release; build_image; echo "✓ $IMAGE (deployed nowhere)" ;;
  launch) launch ;;
  upgrade) upgrade ;;
esac
