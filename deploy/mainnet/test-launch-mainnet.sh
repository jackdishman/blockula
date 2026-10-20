#!/usr/bin/env bash
# Offline tests of deploy/mainnet/launch-mainnet.sh (CI `ops` job), in the style of
# deploy/test-deploy.sh. The launcher runs from a throwaway checkout (this one's deploy scripts, a
# stand-in genesis/blockula-1 with its pin, tagged as releases v9.9.9 and v9.9.10) against the
# stand-in fly, curl and network (deploy/test-standins.sh: here stateful, with an outside genesis
# validator that proposes) and a stand-in `blockula` as its VERIFIER (release fetch with its
# checks, the genesis hash and rebuild, operator entries; keygen fails the test). Nothing here
# talks to Fly or GitHub.
#
#   bash deploy/mainnet/test-launch-mainnet.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
T=$(mktemp -d "${TMPDIR:-/tmp}/blockula-mainnet-test.XXXXXX")
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
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

# shellcheck source=deploy/test-standins.sh
source deploy/test-standins.sh
mkdir -p "$T/bin" "$T/fly" "$T/home"
write_standins "$T/bin"
# The VERIFIER: the release's own blockula, as far as the launcher uses it.
cat > "$T/bin/verifier" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$FAKE_FLY/verifier.log"
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
case "$1 ${2:-}" in
  "--version "*) echo "blockula ${FAKE_VERSION:-9.9.9}" ;;
  "keygen "*) exit 99 ;;
  # The stand-in network's genesis hash for the genesis this test pins, another for any other.
  "checkpoint --genesis") if cmp -s "$3" "$GOOD_GENESIS"; then printf 'ab%.0s' $(seq 32); else printf 'ee%.0s' $(seq 32); fi; echo ;;
  "genesis build")
    out=""; while [ $# -gt 0 ]; do [ "$1" != --out ] || out=$2; shift; done
    cp genesis.json "$out"; [ -z "${FAKE_REBUILD_DIFFERS:-}" ] || echo " " >> "$out" ;;
  # A key's public key and address: "pk" and "boo1" before the key file's first 16 characters.
  "genesis operator")
    key=""; while [ $# -gt 0 ]; do [ "$1" != --key ] || key=$2; shift; done
    k=$(cat "$key"); jq -cn --arg s "${k:0:16}" '{keys: {consensus_public_key: ("pk" + $s), consensus_address: ("boo1" + $s)}}' ;;
  # As the real one: the signature (here: a .sig present, FAKE_BAD_SIG fails it), the first line,
  # each file's SHA-256; only then the files land in --dir.
  "release fetch")
    tag=$3; shift 3; dir="" from="" files=()
    while [ $# -gt 0 ]; do case $1 in --dir) dir=$2; shift ;; --from) from=$2; shift ;; --file) files+=("$2"); shift ;; esac; shift; done
    src=${from:-$FAKE_RELEASES/$tag}
    [ -f "$src/checksums.txt.sig" ] || { echo "✗ the release has no checksums.txt.sig" >&2; exit 1; }
    [ -z "${FAKE_BAD_SIG:-}" ] || { echo "✗ signature check FAILED" >&2; exit 1; }
    [ "$(head -n 1 "$src/checksums.txt")" = "# blockula-release $tag" ] || { echo "✗ checksums.txt names another release" >&2; exit 1; }
    for f in "${files[@]}"; do
      [ "$(sha "$src/$f")" = "$(awk -v f="$f" '$2 == f { print $1 }' "$src/checksums.txt")" ] || { echo "✗ $f: checksum mismatch" >&2; exit 1; }
    done
    mkdir -p "$dir"; cp "$src/checksums.txt" "$src/checksums.txt.sig" "$dir/"
    for f in "${files[@]}"; do cp "$src/$f" "$dir/"; done ;;
  *) echo "verifier: $*?" >&2; exit 64 ;;
esac
STUB
chmod +x "$T/bin/verifier"
world() { FAKE_FLY="$T/fly" FAKE_PREFIX=bm "$T/bin/world" "$@"; }
wlog() { cat "$T/fly/world/log"; }
early() { grep -c 'while not back' "$T/fly/world/log" || true; }

# The throwaway checkout: the launcher and what it deploys, as a release ships them.
R=$T/repo
mkdir -p "$R/deploy/mainnet" "$R/node/checkpoints" "$R/genesis/blockula-1/operators"
cp deploy/mainnet/launch-mainnet.sh deploy/mainnet/Dockerfile "$R/deploy/mainnet/"
cp deploy/fly-config.sh deploy/restart-gate.sh deploy/key-removal.sh deploy/entrypoint.sh "$R/deploy/"
git -C "$R" init -q
git -C "$R" -c user.name=t -c user.email=t@example.test add -A
git -C "$R" -c user.name=t -c user.email=t@example.test commit -qm launcher
commit() { git -C "$R" -c user.name=t -c user.email=t@example.test add -A && git -C "$R" -c user.name=t -c user.email=t@example.test commit -qm "$1"; }

# lm CMD [VAR=value …]: runs the launcher (its fly calls in a fresh log), prints its exit code.
# Mainnet's settings by default; later VAR=value arguments override them (VAR= empties one).
lm() {
  local cmd=$1; shift
  : > "$T/fly/log"
  (cd "$R" && env PATH="$T/bin:$PATH" HOME="$T/home" FAKE_FLY="$T/fly" FAKE_FLY_STATEFUL=1 FAKE_PREFIX=bm FAKE_OUTSIDER=1 FAKE_OUTSIDER_PROPOSES=1 \
    GOOD_GENESIS="$T/good-genesis.json" FAKE_RELEASES="$T/releases" PROGRESS_SECS=0 GATE_ROUNDS=20 \
    CHAIN_ID=blockula-1 ORG=blockula-main PREFIX=bm RPC_APP=bm-rpc ARCHIVE_APP=bm-archive RPC_DOMAIN=rpc.blockula.test \
    RELEASE=v9.9.9 VERIFIER="$T/bin/verifier" KEYS_DIR="$T/keys" OUTSIDE_VALIDATORS=tboo1outsider "$@" \
    bash deploy/mainnet/launch-mainnet.sh "$cmd") > "$T/out" 2>&1 && code=0 || code=$?
  cat "$T/fly/log" >> "$T/fly/all.log"
  echo "$code"
}
out() { cat "$T/out"; }
flylog() { cat "$T/fly/log"; }
# The fly calls of the last run that aren't reads.
writes() { grep -vE '^(auth whoami|apps list|volumes list|ips list|certs list|machines list|secrets list)' "$T/fly/log" || true; }

# ---- what it refuses before calling fly at all ----

refused() { # what code needle
  if [ "$2" != 0 ] && [ ! -s "$T/fly/log" ] && grep -qF -- "$3" "$T/out"; then ok "refused: $1"; else bad "$1: exited $2, fly: $(flylog), out: $(out)"; fi
}
refused "no subcommand" "$(lm "")" "usage: deploy/mainnet/launch-mainnet.sh prepare|image|launch|upgrade"
refused "RESET, set to anything" "$(lm prepare RESET=1)" "has no reset"
refused "RESET, even empty" "$(lm prepare RESET=)" "has no reset"
refused "no ORG" "$(lm prepare ORG=)" "ORG is required"
refused "ORG=personal (the testnet's)" "$(lm prepare ORG=personal)" "where the testnet runs"
refused "PREFIX=blockula (testnet-2's validators)" "$(lm prepare PREFIX=blockula)" "PREFIX=blockula is (or may become) one of the public testnet's apps"
refused "PREFIX=blockula-v… " "$(lm prepare PREFIX=blockula-vx)" "PREFIX=blockula-vx is (or may become) one of the public testnet's apps"
refused "RPC_APP=blockula-testnet (testnet-2's public node)" "$(lm prepare RPC_APP=blockula-testnet)" "RPC_APP=blockula-testnet is (or may become)"
refused "ARCHIVE_APP=blockula-v3" "$(lm prepare ARCHIVE_APP=blockula-v3)" "ARCHIVE_APP=blockula-v3 is (or may become)"
refused "an RPC app named like a validator" "$(lm prepare RPC_APP=bm-v2)" "RPC_APP=bm-v2 is one of the validators' app names"
refused "one app for both RPC nodes" "$(lm prepare ARCHIVE_APP=bm-rpc)" "the pruning and the archive node are two"
refused "a testnet chain id" "$(lm prepare CHAIN_ID=blockula-testnet-2)" "deployed by deploy/launch.sh"
refused "a leftover HALT_HEIGHT" "$(lm prepare HALT_HEIGHT=500)" "HALT_HEIGHT is set in this shell"
refused "a leftover NETWORK_HALTED_AT" "$(lm launch NETWORK_HALTED_AT=499)" "NETWORK_HALTED_AT is set in this shell"
refused "shared CPUs on mainnet" "$(lm prepare VALIDATOR_VM=shared-cpu-2x)" "mainnet runs on dedicated CPUs"

# ---- prepare ----

code=$(lm prepare DRY_RUN=1)
[ "$code" = 0 ] && [ -z "$(writes)" ] && [ ! -s "$T/fly/apps" ] && ok "prepare, DRY_RUN: runs only reads, creates nothing" || bad "prepare DRY_RUN exited $code: writes $(writes): $(out)"
expect "prepare, DRY_RUN: prints the app it would create" "$(out)" "DRY_RUN fly apps create bm-v0 --org blockula-main"
expect "prepare, DRY_RUN: a validator volume of 500 GB without snapshots" "$(out)" "DRY_RUN fly volumes create data -a bm-v3 -r ams -s 500 --scheduled-snapshots=false --yes"
expect "prepare, DRY_RUN: a dedicated IPv4" "$(out)" "DRY_RUN fly ips allocate-v4 -a bm-v1 --yes"
expect "prepare, DRY_RUN: the RPC domain's certificate" "$(out)" "DRY_RUN fly certs add rpc.blockula.test -a bm-rpc"
code=$(lm prepare)
[ "$code" = 0 ] || bad "prepare exited $code: $(out)"
[ "$(awk '{ print $1 "@" $2 }' "$T/fly/apps" | sort | tr '\n' ' ')" = "bm-archive@blockula-main bm-rpc@blockula-main bm-v0@blockula-main bm-v1@blockula-main bm-v2@blockula-main bm-v3@blockula-main " ] \
  && ok "prepare: six apps, all in mainnet's org" || bad "prepare: apps $(cat "$T/fly/apps")"
[ "$(jq -r '"\(.size_gb) \(.auto_backup_enabled) \(.region)"' "$T/fly/bm-v2.volumes")" = "500 false fra" ] && ok "prepare: a validator's volume: 500 GB, no scheduled snapshots, its region" || bad "prepare: bm-v2's volume $(cat "$T/fly/bm-v2.volumes")"
[ "$(jq -r .size_gb "$T/fly/bm-rpc.volumes") $(jq -r .size_gb "$T/fly/bm-archive.volumes") $(jq -r .region "$T/fly/bm-archive.volumes")" = "250 500 ord" ] && ok "prepare: the RPC nodes' volumes (250 GB pruning, 500 GB archive in ord)" || bad "prepare: RPC volumes $(cat "$T/fly/bm-rpc.volumes" "$T/fly/bm-archive.volumes")"
regions=(iad sjc fra ams)
for i in 0 1 2 3; do expect "prepare: bm-v$i's p2p address for its operator file" "$(out)" "bm-v$i ${regions[$i]} 198.51.100.$((i + 1)):30300"; done
expect "prepare: the HTTP address the operator files publish" "$(out)" "--http-addr https://rpc.blockula.test"
[ "$(cut -d' ' -f1 "$T/fly/bm-rpc.ips" | sort | tr '\n' ' ')" = "shared_v4 v6 " ] && grep -qx rpc.blockula.test "$T/fly/bm-rpc.certs" && ok "prepare: the public node gets IPv6, a shared IPv4 and its certificate" || bad "prepare: bm-rpc ips $(cat "$T/fly/bm-rpc.ips")"
# Rerun: nothing new; and a validator volume that has snapshots on gets them turned off.
jq -c '.auto_backup_enabled = true' "$T/fly/bm-v1.volumes" > "$T/v" && mv "$T/v" "$T/fly/bm-v1.volumes"
code=$(lm prepare)
[ "$code" = 0 ] && ! grep -qE '^(apps create|volumes create|ips allocate)' "$T/fly/log" && ok "prepare: a rerun creates and allocates nothing more" || bad "prepare rerun ($code): $(writes)"
[ "$(jq -r .auto_backup_enabled "$T/fly/bm-v1.volumes")" = false ] && ok "prepare: turns a validator volume's scheduled snapshots off" || bad "prepare: bm-v1 snapshots $(cat "$T/fly/bm-v1.volumes")"

# ---- the genesis, its pin and the releases, as the ceremony and the release leave them ----

GENESIS_MS=$((($(date +%s) + 600) * 1000)) # ten minutes from now
seed() { printf "c$1%.0s" $(seq 32); }
mkdir -p "$T/keys"
for i in 0 1 2 3; do mkdir -p "$T/keys/v$i"; seed "$i" > "$T/keys/v$i/validator.key"; done
chmod 700 "$T/keys" "$T/keys"/v*; chmod 600 "$T/keys"/v*/validator.key
{
  for i in 0 1 2 3; do s=$(seed "$i"); jq -cn --arg s "${s:0:16}" --arg p "198.51.100.$((i + 1)):30300" '{address: ("boo1" + $s), public_key: ("pk" + $s), p2p_addr: $p, http_addr: "https://rpc.blockula.test", owner: ("boo1owner" + $s)}'; done
  jq -cn '{address: "tboo1outsider", public_key: "pkoutsider", p2p_addr: "192.0.2.9:30300", http_addr: "https://them.example", owner: "boo1them"}'
} | jq -s --argjson t "$GENESIS_MS" '{chain_id: "blockula-1", address_prefix: "boo", created_at_ms: $t, validators: map({address, public_key, p2p_addr, http_addr}), operators: map({owner, consensus_key: .public_key, p2p_addr})}' > "$R/genesis/blockula-1/genesis.json"
cp "$R/genesis/blockula-1/genesis.json" "$T/good-genesis.json"
echo 'chain_id = "blockula-1"' > "$R/genesis/blockula-1/spec.toml"
echo '# operator' > "$R/genesis/blockula-1/operators/founder-a.toml"
jq -n --arg h "$(printf 'ab%.0s' $(seq 32))" '{chain_id: "blockula-1", genesis_hash: $h, checkpoints: []}' > "$R/node/checkpoints/blockula-1.json"
commit "blockula-1 pinned"
git -C "$R" tag v9.9.9 && git -C "$R" tag v9.9.10
# release DIR TAG: the release's files, as release.yml publishes them (and a signature).
release() {
  local d=$1 tag=$2 b=$T/build/blockula-x86_64-unknown-linux-gnu
  mkdir -p "$d" "$b"
  printf '#!/bin/sh\necho "blockula %s"\n' "${tag#v}" > "$b/blockula"; chmod +x "$b/blockula"
  tar -czf "$d/blockula-x86_64-unknown-linux-gnu.tar.gz" -C "$T/build" blockula-x86_64-unknown-linux-gnu
  cp "$R/genesis/blockula-1/genesis.json" "$d/genesis-blockula-1.json"
  { echo "# blockula-release $tag"; for f in blockula-x86_64-unknown-linux-gnu.tar.gz genesis-blockula-1.json; do echo "$(sha "$d/$f")  $f"; done; } > "$d/checksums.txt"
  echo '{"format":"blockula-release-signature-v1"}' > "$d/checksums.txt.sig"
  cp "$b/blockula" "$d/.binary"
}
release "$T/releases/v9.9.9" v9.9.9
release "$T/releases/v9.9.10" v9.9.10

# ---- image ----

touch "$R/stray"
refused "a checkout with an untracked file" "$(lm image)" "this checkout has changes"
rm "$R/stray"
echo "# edited" >> "$R/deploy/entrypoint.sh"
refused "a checkout with a change" "$(lm image)" "this checkout has changes"
git -C "$R" checkout -q deploy/entrypoint.sh
refused "a release that isn't this checkout's tag" "$(lm image RELEASE=v9.9.8)" "no tag v9.9.8 in this checkout"
refused "no release" "$(lm image RELEASE=)" "RELEASE must be the release to run"
refused "a VERIFIER of another release" "$(lm image FAKE_VERSION=9.9.8)" "is blockula 9.9.8, not blockula 9.9.9"
refused "a signature that doesn't verify" "$(lm image FAKE_BAD_SIG=1)" "didn't verify against the release key"
expect "… and says why" "$(out)" "signature check FAILED"
cp -R "$T/releases/v9.9.9" "$T/tampered"; echo x >> "$T/tampered/blockula-x86_64-unknown-linux-gnu.tar.gz"
refused "an archive that isn't the signed one" "$(lm image RELEASE_FROM="$T/tampered")" "checksum mismatch"
refused "an unsigned release on blockula-1" "$(lm image UNSIGNED_RELEASE=1)" "blockula-1 runs signed releases only"
# A signed release whose genesis-blockula-1.json isn't the checkout's (every file as its checksums.txt says).
cp -R "$T/releases/v9.9.9" "$T/othergen"
jq -c '.created_at_ms += 1' "$T/releases/v9.9.9/genesis-blockula-1.json" > "$T/othergen/genesis-blockula-1.json"
{ echo "# blockula-release v9.9.9"; for f in blockula-x86_64-unknown-linux-gnu.tar.gz genesis-blockula-1.json; do echo "$(sha "$T/othergen/$f")  $f"; done; } > "$T/othergen/checksums.txt"
refused "a release whose genesis isn't this checkout's" "$(lm image RELEASE_FROM="$T/othergen")" "v9.9.9's genesis-blockula-1.json isn't this checkout's genesis/blockula-1/genesis.json"
code=$(lm image)
[ "$code" = 0 ] || bad "image exited $code: $(out)"
B=$T/fly/build
[ "$(ls "$B/context" | sort | tr '\n' ' ')" = "Dockerfile blockula entrypoint.sh " ] && ok "image: the context is the binary, the entrypoint and the Dockerfile, nothing else" || bad "image: context $(ls "$B/context")"
cmp -s "$B/context/blockula" "$T/releases/v9.9.9/.binary" && cmp -s "$B/context/entrypoint.sh" deploy/entrypoint.sh && cmp -s "$B/context/Dockerfile" deploy/mainnet/Dockerfile \
  && ok "image: the release's own binary, the tag's entrypoint and Dockerfile" || bad "image: context files differ"
args=$(cat "$B/args")
expect "image: the build checks the binary's SHA-256" "$args" "BLOCKULA_SHA256=$(sha "$T/releases/v9.9.9/.binary")"
expect "image: and its version" "$args" "BLOCKULA_VERSION=9.9.9"
expect "image: built and pushed, deployed nowhere" "$args" "--build-only"
[ -z "$(grep -vE '^(auth whoami|deploy )' "$T/fly/log" || true)" ] && ! grep -q -- '--image ' "$T/fly/log" && ok "image: no app deployed" || bad "image: fly calls $(flylog)"
expect "image: names the image" "$(out)" "registry.fly.io/bm-rpc:v9.9.9-"
[ "$(sed -nE 's/^FROM (debian[^ ]+).*/\1/p' deploy/Dockerfile)" = "$(sed -nE 's/^FROM (debian[^ ]+).*/\1/p' deploy/mainnet/Dockerfile)" ] && ok "image: the same digest-pinned base as the testnet image's runtime" || bad "image: the bases differ"

# ---- launch: what it refuses before anything is staged or deployed ----

untouched() { # what code needle
  if [ "$2" != 0 ] && ! grep -qE '^(secrets import|deploy )' "$T/fly/log" && grep -qF -- "$3" "$T/out"; then ok "launch refuses $1"; else bad "launch, $1: exited $2: $(out)"; fi
}
untouched "more than LAUNCH_WINDOW_MINS before genesis time" "$(lm launch LAUNCH_WINDOW_MINS=5)" "validators start at most 5 min before it"
# A dry run plans the launch any time before it (the runbook's, the day before and at T − 2 h).
code=$(lm launch DRY_RUN=1 LAUNCH_WINDOW_MINS=5)
[ "$code" = 0 ] && [ -z "$(writes)" ] && grep -qF "DRY_RUN: the launch itself refuses until then" "$T/out" && [ "$(grep -c '^DRY_RUN fly deploy -c' "$T/out")" = 6 ] \
  && ok "launch, DRY_RUN: plans a launch before the window, saying the launch itself waits for it" || bad "launch DRY_RUN before the window exited $code: writes $(writes): $(out)"
# The day before, the keys are still encrypted: a dry run plans the launch without them.
before=$(wc -l < "$T/fly/verifier.log")
code=$(lm launch DRY_RUN=1 KEYS_DIR=)
[ "$code" = 0 ] && [ -z "$(writes)" ] && [ "$(grep -c '^DRY_RUN fly deploy -c' "$T/out")" = 6 ] && ! grep -q 'genesis operator' < <(tail -n +"$((before + 1))" "$T/fly/verifier.log") \
  && expect "launch, DRY_RUN without KEYS_DIR: plans the six deploys, reading no key, and says so" "$(out)" "DRY_RUN without KEYS_DIR: no key is read" || bad "launch DRY_RUN without KEYS_DIR exited $code: writes $(writes): $(out)"
expect "launch, DRY_RUN without KEYS_DIR: still matches each app to its genesis validator" "$(out)" "bm-v3  ams  198.51.100.4  boo1c3c3c3c3c3c3c3c3"
untouched "without KEYS_DIR" "$(lm launch KEYS_DIR=)" "KEYS_DIR is required"
untouched "a genesis validator nobody named, in a dry run without KEYS_DIR too" "$(lm launch DRY_RUN=1 KEYS_DIR= OUTSIDE_VALIDATORS=)" "the genesis's other validators are 'tboo1outsider'"
# Two machines in a validator app (a scale, a clone): two copies of its key.
touch "$T/fly/bm-v2.machine" "$T/fly/bm-v2.machine2"
untouched "a validator app with two machines" "$(lm launch)" "bm-v2 has more than one machine: two copies of its key would double-sign"
rm "$T/fly/bm-v2.machine" "$T/fly/bm-v2.machine2"
# The genesis the checkout holds must be the one this release pins.
pinned=$(git -C "$R" rev-parse HEAD)
repin() { # genesis_hash (JSON): commits it as the release's pin, tagged v9.9.9
  jq --argjson h "$1" '.genesis_hash = $h' "$R/node/checkpoints/blockula-1.json" > "$T/pin" && mv "$T/pin" "$R/node/checkpoints/blockula-1.json"
  commit "another pin" && git -C "$R" tag -f v9.9.9 >/dev/null
}
repin "\"$(printf 'cd%.0s' $(seq 32))\""
refused "a genesis this release pins to another hash" "$(lm launch)" "hashes to $(printf 'ab%.0s' $(seq 32)), but this release pins blockula-1 to $(printf 'cd%.0s' $(seq 32))"
repin null
refused "a release that doesn't pin blockula-1" "$(lm launch)" "blockula-1 isn't pinned in this release"
git -C "$R" reset -q --hard "$pinned" && git -C "$R" tag -f v9.9.9 "$pinned" >/dev/null
cp "$T/keys/v0/validator.key" "$T/owner.json"; cp "$T/owner.json" "$T/keys/owner.json"; chmod 600 "$T/keys/owner.json"
untouched "a KEYS_DIR holding another key" "$(lm launch)" "KEYS_DIR has $T/keys/owner.json"
rm "$T/keys/owner.json"
chmod 640 "$T/keys/v2/validator.key"
untouched "a key others can read" "$(lm launch)" "$T/keys/v2/validator.key is readable or writable by others"
chmod 600 "$T/keys/v2/validator.key"
mv "$T/keys/v3" "$T/v3"
untouched "a seat without its key" "$(lm launch)" "no $T/keys/v3/validator.key"
mv "$T/v3" "$T/keys/v3"
mv "$T/keys/v0/validator.key" "$T/k"; mv "$T/keys/v1/validator.key" "$T/keys/v0/validator.key"; mv "$T/k" "$T/keys/v1/validator.key"
untouched "keys on the wrong apps" "$(lm launch)" "validator.key isn't the key of bm-v0's genesis validator"
mv "$T/keys/v0/validator.key" "$T/k"; mv "$T/keys/v1/validator.key" "$T/keys/v0/validator.key"; mv "$T/k" "$T/keys/v1/validator.key"
untouched "a genesis validator nobody named" "$(lm launch OUTSIDE_VALIDATORS=)" "the genesis's other validators are 'tboo1outsider', but OUTSIDE_VALIDATORS is ''"
untouched "a spec that doesn't rebuild the genesis" "$(lm launch FAKE_REBUILD_DIFFERS=1)" "doesn't rebuild"
untouched "another genesis directory for blockula-1" "$(lm launch GENESIS_DIR="$T")" "blockula-1's genesis is the one the release ships"
[ ! -e "$T/home/.config/blockula/blockula-1-fly/launched" ] && ok "launch: none of those marked the chain launched" || bad "launch: marked launched"

# ---- launch ----

# (Paths in the settings are the caller's: these are relative to the checkout it runs from.)
code=$(lm launch DRY_RUN=1 KEYS_DIR=../keys VERIFIER=../bin/verifier)
[ "$code" = 0 ] && [ -z "$(writes)" ] && [ -z "$(ls "$T/home/.config/blockula/blockula-1-fly" | grep -v '^p2p-addrs$' || true)" ] && ok "launch, DRY_RUN: reads only, writes nothing" || bad "launch DRY_RUN exited $code: writes $(writes): $(out)"
[ "$(grep -c '^DRY_RUN fly deploy -c' "$T/out")" = 6 ] && ok "launch, DRY_RUN: prints the six deploys" || bad "launch DRY_RUN deploys: $(out)"
expect "launch, DRY_RUN: and each key's staging, by command only" "$(out)" "DRY_RUN fly secrets import -a bm-v0 --stage"
refute "launch, DRY_RUN: no key in its output" "$(out)" "$(seed 0)"
expect "launch, DRY_RUN: each deploy's configuration under it (validators on performance-4x)" "$(out)" '    |   size = "performance-4x"'
expect "launch, DRY_RUN: … keeping 43,200 blocks" "$(out)" '    |   PRUNE_BLOCKS = "43200"'
expect "launch, DRY_RUN: … the archive keeping every block" "$(out)" '    |   ARCHIVE = "1"'
refute "launch, DRY_RUN: … and no halt" "$(out)" "HALT_HEIGHT"

# The RPC nodes deploy first: one that fails stops the launch before any key is staged, and so
# does one that leaves no machine.
code=$(lm launch FAKE_FLY_DEPLOY_FAIL=bm-rpc)
[ "$code" != 0 ] && ! grep -qE '^secrets import -a bm-v' "$T/fly/log" && ! grep -qE '^deploy .*bm-v[0-9]\.toml' "$T/fly/log" \
  && expect "launch: a failed RPC deploy stops it before any key is staged" "$(out)" "deploying bm-rpc failed" || bad "launch with a failed RPC deploy ($code): $(writes): $(out)"
code=$(lm launch FAKE_FLY_NO_MACHINE=bm-rpc)
[ "$code" != 0 ] && ! grep -qE '^secrets import -a bm-v' "$T/fly/log" \
  && expect "launch: an RPC deploy that leaves no machine stops it before any key is staged" "$(out)" "bm-rpc has no machine after its deploy" || bad "launch with an RPC deploy and no machine ($code): $(writes): $(out)"
[ ! -e "$T/home/.config/blockula/blockula-1-fly/launched" ] && ok "launch: neither marked the chain launched" || bad "launch: marked launched after a failed RPC deploy"
world init
code=$(lm launch)
[ "$code" = 0 ] || bad "launch exited $code: $(out)"
for i in 0 1 2 3; do expect "launch: bm-v$i deployed with its key staged" "$(cat "$T/fly/deploys")" "bm-v$i key=yes"; done
first_key=$(grep -nE '^secrets import -a bm-v' "$T/fly/log" | head -n 1 | cut -d: -f1)
last_rpc=$(grep -nE '^deploy -c .*bm-(rpc|archive)\.toml' "$T/fly/log" | tail -n 1 | cut -d: -f1)
[ -n "$first_key" ] && [ -n "$last_rpc" ] && [ "$last_rpc" -lt "$first_key" ] && ok "launch: both RPC nodes deploy before any key is staged" || bad "launch: RPC deploys (line $last_rpc) and the first key staged (line $first_key): $(flylog)"
for app in bm-rpc bm-archive; do expect "launch: $app's deploy doesn't wait for its health check (503 until it serves a fresh head)" "$(cat "$T/fly/$app.args")" "--detach"; done
refute "launch: a validator's deploy waits for its machine" "$(cat "$T/fly/bm-v0.args")" "--detach"
expect "launch: the pruning RPC node deployed" "$(cat "$T/fly/deploys")" "bm-rpc key=no"
expect "launch: the archive deployed" "$(cat "$T/fly/deploys")" "bm-archive key=no"
v=$(cat "$T/fly/bm-v1.toml")
expect "launch: validators on performance-4x" "$v" 'size = "performance-4x"'
expect "launch: with 16 GB" "$v" 'memory = "16gb"'
expect "launch: keeping 43,200 blocks" "$v" 'PRUNE_BLOCKS = "43200"'
expect "launch: the committed genesis" "$v" "local_path = \"$(cd "$R" && pwd)/genesis/blockula-1/genesis.json\""
refute "launch: no key in a validator's configuration" "$v" "VALIDATOR_KEY"
expect "launch: one machine per validator" "$(cat "$T/fly/bm-v1.args")" "--ha=false"
expect "launch: the verified image" "$(cat "$T/fly/bm-v1.args")" "registry.fly.io/bm-rpc:v9.9.9-"
a=$(cat "$T/fly/bm-archive.toml")
expect "launch: the archive keeps every block" "$a" 'ARCHIVE = "1"'
refute "launch: the archive has no window" "$a" "PRUNE_BLOCKS"
expect "launch: the archive in its region" "$a" 'primary_region = "ord"'
expect "launch: the archive syncs from the pruning node too (validators keep 12 hours of blocks)" "$a" 'PEER_HTTP = "http://bm-v0.internal:7070 http://bm-v1.internal:7070 http://bm-v2.internal:7070 http://bm-v3.internal:7070 http://bm-rpc.internal:7171"'
expect "launch: the public node keeps 14 days" "$(cat "$T/fly/bm-rpc.toml")" 'PRUNE_BLOCKS = "1209600"'
expect "launch: the public node pins the genesis" "$(cat "$T/fly/bm-rpc.toml")" "GENESIS_HASH = \"$(printf 'ab%.0s' $(seq 32))\""
held=""; for app in bm-v0 bm-v1 bm-v2 bm-v3; do ! grep -qx VALIDATOR_KEY_B64 "$T/fly/$app.secrets" || held="$held $app"; done
[ -z "$held" ] && ok "launch: every key removed from Fly once seeded" || bad "launch: keys left in Fly:$held"
for app in bm-v0 bm-v3 bm-rpc bm-archive; do grep -qx BLOCKULA_FORWARDER_SECRET "$T/fly/$app.secrets" || bad "launch: $app has no forwarder secret"; done
together=$(awk '/^restart bm-v[0-9]+ \(secrets\)/ { if (!first) first = NR; last = NR } /^status / { s[NR] = 1 } END { if (!first) { print "none"; exit } for (i = first; i <= last; i++) if (s[i]) { print "no"; exit } print "yes" }' "$T/fly/world/log")
[ "$together" = yes ] && [ "$(grep -c '^restart bm-v[0-9]* (secrets)' "$T/fly/world/log")" = 4 ] && ok "launch: the key removal restarts the validators once, together" || bad "launch: key-removal restarts ($together): $(wlog | grep '^restart')"
expect "launch: then every validator signs, links and proposes, and the outside one proposes" "$(out)" "outside validators that proposed in them: tboo1outsider"
for f in "$T/fly/all.log" "$T/out"; do
  refute "launch: no key in fly's arguments or the output (hex)" "$(cat "$f")" "$(seed 2)"
  refute "launch: no key in fly's arguments or the output (base64)" "$(cat "$f")" "$(seed 2 | base64 | tr -d '\n')"
done
expect "launch: Prometheus targets for every node" "$(cat "$T/home/.config/blockula/blockula-1-fly/prometheus-targets.yml")" "bm-archive.internal:7171"
[ "$(ls -l "$T/home/.config/blockula/blockula-1-fly/forwarder-secret" | cut -c1-10)" = "-rw-------" ] && ok "launch: the forwarder secret is mode 600" || bad "launch: forwarder secret mode"
ST=$T/home/.config/blockula/blockula-1-fly
[ "$(cat "$ST/finished" 2>/dev/null)" = v9.9.9 ] && ok "launch: marks the launch finished once its gate passed" || bad "launch: no finished marker: $(ls "$ST")"
# Until then, a validator that never got deployed (its machine gone) is the launch's release's alone.
mv "$T/fly/bm-v3.machine" "$T/v3.machine"; mv "$ST/finished" "$T/finished"
untouched "a validator deployed with another release than the launch's, before the launch finished" "$(lm launch RELEASE=v9.9.10 FAKE_VERSION=9.9.10)" "this launch started with v9.9.9"
mv "$T/finished" "$ST/finished"
# Once it finished, a validator whose machine is gone and whose volume holds its key (staged once,
# off Fly since) gets a machine with the release the nodes run now, and no key is read or staged.
: > "$T/fly/deploys"
world init
code=$(lm launch KEYS_DIR= RELEASE=v9.9.10 FAKE_VERSION=9.9.10)
[ "$code" = 0 ] && [ "$(cat "$T/fly/deploys")" = "bm-v3 key=no" ] && ! grep -qE '^secrets (import|unset)' "$T/fly/log" \
  && ok "launch: a validator's machine rebuilt on its volume, after the launch, with no key at hand" || bad "launch, bm-v3's machine rebuilt ($code): $(cat "$T/fly/deploys"): $(out)"
expect "launch: … with the release the nodes run now" "$(cat "$T/fly/bm-v3.args")" "registry.fly.io/bm-rpc:v9.9.10-"
# A rerun deploys nothing again: the chain is live, and upgrades go through the gate.
world init
code=$(lm launch)
[ "$code" = 0 ] && ! grep -qE '^(secrets import|deploy )' "$T/fly/log" && ok "launch: a rerun deploys and stages nothing" || bad "launch rerun ($code): $(writes): $(out)"
code=$(lm launch RELEASE=v9.9.10 FAKE_VERSION=9.9.10)
[ "$code" = 0 ] && ! grep -qE '^(secrets import|deploy )' "$T/fly/log" && ok "launch: a rerun with a newer release still deploys nothing (that's upgrade's)" || bad "launch rerun with another release ($code): $(out)"

# A validator whose volume was lost and made again (its key seeded on the old one, and off Fly):
# launch refuses to deploy a machine that would have no key, until the operator deletes the
# staging marker; then it stages that seat's key again (KEYS_DIR holding that key alone), with the
# release the nodes run now, and removes it once seeded.
rm "$T/fly/bm-v1.machine"
jq -c --arg t "$(date -u -v+1d +%FT%T.000Z 2>/dev/null || date -u -d tomorrow +%FT%T.000Z)" '.created_at = $t' "$T/fly/bm-v1.volumes" > "$T/v" && mv "$T/v" "$T/fly/bm-v1.volumes"
world init
untouched "a validator whose volume was made after its key was staged" "$(lm launch)" "bm-v1's volume was created"
expect "… and says how to stage its key again" "$(out)" "delete $ST/staged-bm-v1 and rerun with KEYS_DIR"
rm "$ST/staged-bm-v1"
mkdir -p "$T/keys1/v1"; cp "$T/keys/v1/validator.key" "$T/keys1/v1/"; chmod 700 "$T/keys1" "$T/keys1/v1"; chmod 600 "$T/keys1/v1/validator.key"
mkdir -p "$T/keys0/v0"; cp "$T/keys/v0/validator.key" "$T/keys0/v0/"; chmod 700 "$T/keys0" "$T/keys0/v0"; chmod 600 "$T/keys0/v0/validator.key"
untouched "re-seeding a seat without its key" "$(lm launch KEYS_DIR="$T/keys0" RELEASE=v9.9.10 FAKE_VERSION=9.9.10)" "no $T/keys0/v1/validator.key: bm-v1 is deployed with its consensus key"
: > "$T/fly/deploys"
world init
code=$(lm launch KEYS_DIR="$T/keys1" RELEASE=v9.9.10 FAKE_VERSION=9.9.10)
[ "$code" = 0 ] && [ "$(cat "$T/fly/deploys")" = "bm-v1 key=yes" ] && ! grep -qx VALIDATOR_KEY_B64 "$T/fly/bm-v1.secrets" \
  && ok "launch: then stages that seat's key again (the only key at hand), deploys it alone and removes the key once seeded" || bad "launch, re-staging bm-v1 ($code): $(cat "$T/fly/deploys"): $(out)"
expect "launch: … with the release the nodes run now" "$(cat "$T/fly/bm-v1.args")" "registry.fly.io/bm-rpc:v9.9.10-"
expect "launch: … into the running chain" "$(out)" "The launch finished (with v9.9.9): deploying bm-v1 into the running chain with v9.9.10"

# An RPC node rebuilt (its machine gone): launch deploys it alone, with no keys at hand, from the
# release the nodes run now.
rm "$T/fly/bm-archive.machine"
world init
code=$(lm launch KEYS_DIR= RELEASE=v9.9.10 FAKE_VERSION=9.9.10)
[ "$code" = 0 ] && [ "$(grep '^deploy ' "$T/fly/log" | grep -c -- '--image registry')" = 1 ] && grep -q '^deploy -c .*bm-archive.toml' "$T/fly/log" \
  && ok "launch: rebuilds a lost RPC node alone, without KEYS_DIR" || bad "launch, an RPC node rebuilt ($code): $(writes): $(out)"

# ---- upgrade ----

world init
code=$(lm upgrade RELEASE=v9.9.10 FAKE_VERSION=9.9.10 DRY_RUN=1)
[ "$code" = 0 ] && [ -z "$(writes)" ] && [ -z "$(grep '^restart' "$T/fly/world/log" || true)" ] && ok "upgrade, DRY_RUN: restarts nothing" || bad "upgrade DRY_RUN ($code): $(writes): $(out)"
world init
rm -f "$T/fly/bm-rpc.toml" "$T/fly/bm-archive.toml" # what the upgrade's deploys configure, not the launch's
code=$(lm upgrade RELEASE=v9.9.10 FAKE_VERSION=9.9.10)
[ "$code" = 0 ] || bad "upgrade exited $code: $(out)"
[ "$(sed -n 's/^restart \([^ ]*\) (deploy).*/\1/p' "$T/fly/world/log" | tr '\n' ' ')" = "bm-v0 bm-v1 bm-v2 bm-v3 bm-rpc bm-archive " ] \
  && ok "upgrade: one node at a time, the validators first" || bad "upgrade: restarts $(wlog | grep '^restart')"
[ "$(early)" = 0 ] && ok "upgrade: none restarts while a validator isn't back" || bad "upgrade: restarted early: $(wlog | grep 'while not back')"
expect "upgrade: the new release's image" "$(cat "$T/fly/bm-v3.args")" "registry.fly.io/bm-rpc:v9.9.10-"
a=$(cat "$T/fly/bm-archive.toml" 2>/dev/null || true)
expect "upgrade: the archive keeps being an archive" "$a" 'ARCHIVE = "1"'
# The gate reads the RPC node being restarted as its public node; the archive's configuration
# still names the pruning node as its sync peer, never the archive itself.
expect "upgrade: the archive still syncs from the pruning node" "$a" 'PEER_HTTP = "http://bm-v0.internal:7070 http://bm-v1.internal:7070 http://bm-v2.internal:7070 http://bm-v3.internal:7070 http://bm-rpc.internal:7171"'
refute "upgrade: … and never from itself" "$(grep PEER_HTTP <<<"$a" || true)" "bm-archive"
expect "upgrade: … in its own app" "$a" 'app = "bm-archive"'
expect "upgrade: … and region" "$a" 'primary_region = "ord"'
p=$(cat "$T/fly/bm-rpc.toml" 2>/dev/null || true)
expect "upgrade: the public node keeps its 14 days" "$p" 'PRUNE_BLOCKS = "1209600"'
expect "upgrade: … in its own app" "$p" 'app = "bm-rpc"'
expect "upgrade: … and region" "$p" 'primary_region = "iad"'
expect "upgrade: … syncing from the validators" "$p" 'PEER_HTTP = "http://bm-v0.internal:7070 http://bm-v1.internal:7070 http://bm-v2.internal:7070 http://bm-v3.internal:7070"'
refute "upgrade: … and never from an archive" "$p" "ARCHIVE"
world init
code=$(lm upgrade RELEASE=v9.9.10 FAKE_VERSION=9.9.10 FAKE_OUTSIDER_DOWN=1 GATE_ROUNDS=3)
[ "$code" != 0 ] && [ -z "$(grep '^restart' "$T/fly/world/log" || true)" ] && expect "upgrade: an outside validator that doesn't propose: nothing restarts" "$(out)" "outside validator tboo1outsider proposed none of the last 20 blocks" \
  || bad "upgrade with the outside validator down ($code): $(out)"
world init
code=$(lm upgrade RELEASE=v9.9.10 FAKE_VERSION=9.9.10 OUTSIDE_VALIDATORS= GATE_ROUNDS=3)
[ "$code" != 0 ] && [ -z "$(grep '^restart' "$T/fly/world/log" || true)" ] && expect "upgrade: a validator in the set it can't account for: nothing restarts, at once" "$(out)" "the active set has 5 validators, more than the 4 this script runs" \
  || bad "upgrade with an unnamed outside validator ($code): $(out)"
world init
code=$(lm upgrade RELEASE=v9.9.10 FAKE_VERSION=9.9.10 FAKE_DOWN_AFTER_RESTART=bm-v0 GATE_ROUNDS=4)
[ "$code" != 0 ] && [ "$(sed -n 's/^restart \([^ ]*\) (deploy).*/\1/p' "$T/fly/world/log" | tr '\n' ' ')" = "bm-v0 " ] \
  && expect "upgrade: a validator that doesn't come back stops it, the rest untouched" "$(out)" "bm-v0 isn't back" || bad "upgrade with bm-v0 down ($code): $(out)"
touch "$T/fly/bm-v3.machine2"
world init
code=$(lm upgrade RELEASE=v9.9.10 FAKE_VERSION=9.9.10)
[ "$code" != 0 ] && ! grep -q '^deploy ' "$T/fly/log" && expect "upgrade: refused while a validator app has two machines" "$(out)" "bm-v3 has 2 machines, not 1" || bad "upgrade with two machines ($code): $(out)"
rm "$T/fly/bm-v3.machine2"
echo VALIDATOR_KEY_B64 >> "$T/fly/bm-v2.secrets"
world init
code=$(lm upgrade RELEASE=v9.9.10 FAKE_VERSION=9.9.10)
[ "$code" != 0 ] && ! grep -q '^deploy ' "$T/fly/log" && expect "upgrade: refused while a key is still in Fly" "$(out)" "VALIDATOR_KEY_B64 is still in Fly for bm-v2" || bad "upgrade with a key in Fly ($code): $(out)"

# ---- never ----

refute "nothing ever destroyed an app, machine, volume or secret" "$(cat "$T/fly/all.log")" "destroy"
! grep -q '^keygen' "$T/fly/verifier.log" && ok "no key was ever generated" || bad "keygen ran: $(grep keygen "$T/fly/verifier.log")"
refute "the launcher has no reset path" "$(grep -v '^#' deploy/mainnet/launch-mainnet.sh)" "fly apps destroy"

if [ $fails -gt 0 ]; then echo "✗ $fails failed" >&2; exit 1; fi
echo "✓ mainnet launcher behaves"
