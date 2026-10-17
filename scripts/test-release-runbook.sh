#!/usr/bin/env bash
# Tests the validator's release check (docs/OPERATIONS.md "Verifying a release", audit F49) with the
# real `blockula release sign|verify`: a release signed with the release key K, then the runbook's
# steps as written, on a PATH with only the stand-ins below and the system tools.
#   1. blockula release verify --key-id K --tag V --sig checksums.txt.sig checksums.txt
#   2. sha256sum --ignore-missing -c checksums.txt    (the archive and install.sh)
#   4. BLOCKULA_VERSION=V BLOCKULA_RELEASE_KEY_ID=K BLOCKULA_RELEASE_URL=file://$PWD BLOCKULA_REQUIRE_VERIFY=1 sh install.sh
# (step 3, gh attestation verify, is a stand-in that passes: it's what a compromised workflow can
# produce, so the runbook must hold without it). The genuine release installs. Each way the F49
# reviewer's adversary, who controls the release (install.sh included), gets past
# `gh attestation verify install.sh` is stopped by the step that should stop it:
#   - an install.sh with the attacker's key id built in, checksums re-signed with the attacker's key;
#   - an install.sh with the signature check taken out, likewise;
#   - a swapped install.sh under the genuine signed checksums;
#   - an older genuinely signed release served as the requested one.
#   BLOCKULA_BIN=node/target/release/blockula bash scripts/test-release-runbook.sh
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
BIN=${BLOCKULA_BIN:-$ROOT/node/target/release/blockula}
case $BIN in /*) ;; *) BIN=$ROOT/$BIN ;; esac
[ -x "$BIN" ] || { echo "✗ No blockula at $BIN: cargo build --release --locked --manifest-path node/Cargo.toml (or set BLOCKULA_BIN)" >&2; exit 1; }
"$BIN" release verify --help > /dev/null
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
pass=0
fail() { echo "✗ $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "✓ $1"; }
target=x86_64-unknown-linux-gnu

# Stand-ins: Linux x86_64, gh whose attestation check passes (whatever it's given), and the
# operator's installed blockula: the real binary.
mkdir -p "$T/bin"
printf '#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo x86_64 ;; *) echo Linux ;; esac\n' > "$T/bin/uname"
printf '#!/bin/sh\nexit 0\n' > "$T/bin/gh"
ln -s "$BIN" "$T/bin/blockula"
chmod +x "$T/bin/uname" "$T/bin/gh"
PATH_RUN="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin"
sha_check() { if command -v sha256sum > /dev/null; then sha256sum --ignore-missing -c checksums.txt; else shasum -a 256 --ignore-missing -c checksums.txt; fi; }

"$BIN" keygen > "$T/release-key.json" 2> /dev/null
"$BIN" keygen > "$T/attacker-key.json" 2> /dev/null
K=$("$BIN" release keyid --key "$T/release-key.json")
[[ $K =~ ^[0-9a-f]{64}$ ]] || fail "release keyid printed '$K'"

# release DIR TAG KEYFILE [INSTALL_SH]: a release as release.yml makes it (the archive, install.sh,
# checksums.txt naming TAG) signed with KEYFILE, as the offline machine does.
release() {
  local dir=$1 tag=$2 key=$3 script=${4:-$ROOT/scripts/install.sh}
  rm -rf "$dir" "$T/src" && mkdir -p "$dir" "$T/src/blockula-$target"
  printf '#!/bin/sh\necho "blockula %s"\n' "${tag#v}" > "$T/src/blockula-$target/blockula"
  chmod +x "$T/src/blockula-$target/blockula"
  tar -czf "$dir/blockula-$target.tar.gz" -C "$T/src" "blockula-$target"
  cp "$script" "$dir/install.sh"
  scripts/release-checksums.sh "$tag" "$dir" > /dev/null
  "$BIN" release sign --key "$key" "$dir/checksums.txt" > "$dir/checksums.txt.sig" 2> /dev/null
}
# runbook DIR VERSION: the operator's steps 1, 2 and 4 in DIR; prints the step that refused, or
# "installed".
runbook() {
  local dir=$1 V=$2
  rm -rf "$T/home"
  (
    cd "$dir"
    export PATH=$PATH_RUN HOME=$T/home
    blockula release verify --key-id "$K" --tag "$V" --sig checksums.txt.sig checksums.txt > "$T/step1" 2>&1 || { echo "step 1"; exit 0; }
    sha_check > "$T/step2" 2>&1 || { echo "step 2"; exit 0; }
    BLOCKULA_VERSION=$V BLOCKULA_RELEASE_KEY_ID=$K BLOCKULA_RELEASE_URL=file://$PWD BLOCKULA_REQUIRE_VERIFY=1 BLOCKULA_INSTALL_DIR=$T/home/bin sh install.sh > "$T/step4" 2>&1 || { echo "step 4"; exit 0; }
    [ "$("$T/home/bin/blockula" --version)" = "blockula ${V#v}" ] || { echo "step 4 installed the wrong binary"; exit 0; }
    echo installed
  )
}

release "$T/genuine" v9.9.9 "$T/release-key.json"
[ "$(runbook "$T/genuine" v9.9.9)" = installed ] || fail "the genuine release didn't install: $(cat "$T/step1" "$T/step2" "$T/step4" 2>/dev/null)"
grep -qF "release signature (ML-DSA-65 key" "$T/step4" || fail "install.sh didn't check the signature itself: $(cat "$T/step4")"
ok "the genuine release: verified (signature with K, hashes incl. install.sh) and installed"

# The attacker's install.sh, with their key id built in; checksums signed with their key.
AK=$("$BIN" release keyid --key "$T/attacker-key.json")
sed "s/^BUILTIN_KEY_ID=\"\"/BUILTIN_KEY_ID=\"$AK\"/" scripts/install.sh > "$T/install-attacker-key.sh"
cmp -s scripts/install.sh "$T/install-attacker-key.sh" && fail "couldn't build the attacker's install.sh (BUILTIN_KEY_ID line changed?)"
release "$T/attacker-key" v9.9.9 "$T/attacker-key.json" "$T/install-attacker-key.sh"
[ "$(runbook "$T/attacker-key" v9.9.9)" = "step 1" ] || fail "an install.sh trusting the attacker's key, signed by it, got past step 1"
grep -qF "key" "$T/step1" || fail "step 1 refused for another reason: $(cat "$T/step1")"
ok "an install.sh with the attacker's key id built in, signed with the attacker's key: stopped at step 1"
# Even run directly, that script refuses the operator's K (the key ids disagree).
out=$(cd "$T/attacker-key" && env PATH=$PATH_RUN HOME=$T/home BLOCKULA_VERSION=v9.9.9 BLOCKULA_RELEASE_KEY_ID=$K BLOCKULA_RELEASE_URL="file://$PWD" BLOCKULA_INSTALL_DIR="$T/home/bin" sh install.sh 2>&1) && fail "the attacker's install.sh installed with the operator's key id: $out"
printf '%s' "$out" | grep -qF "isn't the key id you gave" || fail "the attacker's install.sh refused for another reason: $out"
ok "that install.sh, run directly with the operator's key id: refuses (the key ids disagree)"

# The attacker's install.sh with the signature check replaced by a claim that it passed.
awk '/^# 3\. Release signature/{skip=1; print "unchecked_signature=\"\"; signature=1; verified=\"${verified:+$verified; }release signature\""} /^if \[ -n "\$verified" \]/{skip=0} !skip' scripts/install.sh > "$T/install-no-sig.sh"
grep -q "release verify --key-id" "$T/install-no-sig.sh" && fail "couldn't take step 3 out of install.sh (its layout changed?)"
release "$T/no-sig" v9.9.9 "$T/attacker-key.json" "$T/install-no-sig.sh"
# The reviewer's bypass: on its own (provenance passes, as a compromised workflow's would), that
# script installs under REQUIRE_VERIFY with the operator's key id, saying it verified the signature.
out=$(cd "$T/no-sig" && env PATH=$PATH_RUN HOME=$T/home BLOCKULA_VERSION=v9.9.9 BLOCKULA_RELEASE_KEY_ID=$K BLOCKULA_RELEASE_URL="file://$PWD" BLOCKULA_REQUIRE_VERIFY=1 BLOCKULA_INSTALL_DIR="$T/home/bin" sh install.sh 2>&1) || fail "the attacker's script didn't install on its own (so this case tests nothing): $out"
printf '%s' "$out" | grep -qF "Verified: build provenance" || fail "the attacker's script: $out"
ok "an install.sh with the signature check removed installs on its own, REQUIRE_VERIFY and all: the release can't vouch for itself"
[ "$(runbook "$T/no-sig" v9.9.9)" = "step 1" ] || fail "an install.sh without the signature check got past step 1"
ok "that release, signed with the attacker's key: stopped at step 1"

# A swapped install.sh under the genuine, signed checksums.txt.
release "$T/swapped" v9.9.9 "$T/release-key.json"
cp "$T/install-no-sig.sh" "$T/swapped/install.sh"
[ "$(runbook "$T/swapped" v9.9.9)" = "step 2" ] || fail "a swapped install.sh got past step 2"
grep -qE "install.sh: FAILED" "$T/step2" || fail "step 2 refused for another reason: $(cat "$T/step2")"
ok "a swapped install.sh under the genuine signed checksums: stopped at step 2"

# An older release, genuinely signed, served as the one asked for.
release "$T/older" v9.9.8 "$T/release-key.json"
[ "$(runbook "$T/older" v9.9.9)" = "step 1" ] || fail "an older signed release passed for v9.9.9"
grep -qF 'for release "v9.9.8", not v9.9.9' "$T/step1" || fail "step 1 refused the older release for another reason: $(cat "$T/step1")"
ok "an older genuinely signed release served as the requested one: stopped at step 1"

echo "✓ release runbook: $pass cases"
