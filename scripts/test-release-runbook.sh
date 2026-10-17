#!/usr/bin/env bash
# Runs the release runbooks in docs/OPERATIONS.md as an operator pastes them (audit F49, round 3).
# Every command block there marked "<!-- runbook-test: NAME -->" is taken from the file as written,
# its placeholders filled in, and run with bash like a paste: no `set -e` from here, so a block
# that doesn't stop at its first failure runs on. It runs on a PATH holding only the real
# `blockula` (BLOCKULA_BIN, where the block expects one), stand-ins for GitHub (`gh`), `sudo` and
# `uname`, and the system tools, against releases made and signed the way the release workflow and
# the offline signer make them.
#
# The genuine release installs. The F49 adversary controls everything GitHub serves (the archives,
# install.sh, checksums.txt, provenance) but not the offline release key or the operators'
# channel; each thing it can serve is refused with nothing from it run and nothing installed.
# Every block is also run with its `( set -euo pipefail … )` taken off, against an attack it
# refuses: the run must then fail open (a later step runs after a failed one), which proves the
# test would catch a block written without stop-on-failure.
#   BLOCKULA_BIN=node/target/release/blockula bash scripts/test-release-runbook.sh
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
DOC=${RUNBOOK_DOC:-$ROOT/docs/OPERATIONS.md}
BIN=${BLOCKULA_BIN:-$ROOT/node/target/release/blockula}
case $BIN in /*) ;; *) BIN=$ROOT/$BIN ;; esac
[ -x "$BIN" ] || { echo "✗ No blockula at $BIN: cargo build --release --locked --manifest-path node/Cargo.toml (or set BLOCKULA_BIN)" >&2; exit 1; }
"$BIN" release install --help > /dev/null
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
pass=0
fail() { echo "✗ $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "✓ $1"; }

# ---- the blocks ----
EXPECTED="first-install patch-install rotation upgrade validator-genesis"
names=$(sed -n 's/^<!-- runbook-test: \([a-z-]*\) -->$/\1/p' "$DOC" | sort | tr '\n' ' ' | sed 's/ $//')
[ "$names" = "$EXPECTED" ] || fail "$DOC marks the blocks '$names', expected '$EXPECTED' (a runbook lost its test marker, or a new one needs a case here)"
# block NAME: the fenced block right after NAME's marker.
block() {
  awk -v m="<!-- runbook-test: $1 -->" '
    $0 == m { found = 1; next }
    found && !open && /^```/ { open = 1; next }
    found && open && /^```/ { exit }
    found && open { print }' "$DOC"
}
for n in $EXPECTED; do
  b=$(block "$n")
  [ "$(printf '%s\n' "$b" | head -n 1)" = "( set -euo pipefail" ] || fail "block $n doesn't start with '( set -euo pipefail': pasted, it would run on after a failed check"
  [ "$(printf '%s\n' "$b" | tail -n 1)" = ")" ] || fail "block $n doesn't end with ')'"
  # Interactive zsh runs a '#' line as a command (no interactivecomments), and interactive bash
  # expands '!': a block must paste the same into both.
  if printf '%s\n' "$b" | grep -n '#'; then fail "block $n has a comment (above): interactive zsh would run it as a command; put it in the prose"; fi
  if printf '%s\n' "$b" | grep -nF '!'; then fail "block $n has a '!' (above): interactive bash would expand it"; fi
done
ok "the doc's $(echo "$EXPECTED" | wc -w | tr -d ' ') release blocks are each one '( set -euo pipefail … )'"
grep -nE '(^|[^a-z])sh install\.sh|bash install\.sh|\| *sh *$' "$DOC" | grep -v 'curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh' \
  && fail "$DOC runs a release's install.sh in a runbook: operators install with blockula release install" || true
ok "no runbook in the doc runs the release's install.sh"

# ---- the machine: stand-ins and a fake root ----
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) HOST_T=aarch64-apple-darwin ;;
  Darwin-x86_64) HOST_T=x86_64-apple-darwin ;;
  Linux-x86_64) HOST_T=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) HOST_T=aarch64-unknown-linux-gnu ;;
  *) fail "no release target for $(uname -s) $(uname -m)" ;;
esac
LINUX_T=x86_64-unknown-linux-gnu # what the stand-in uname makes the first-install block download
mkdir -p "$T/bin" "$T/tmp"
# gh: `release download` serves $MIRROR (the GitHub release), `attestation verify` passes unless
# FAKE_ATTEST=none (no attestation exists, as for a privately built patch: real gh says HTTP 404).
cat > "$T/bin/gh" << 'SH'
#!/usr/bin/env bash
echo "gh $*" >> "$LOG/gh.log"
case "$1 $2" in
  "auth status") exit 0 ;;
  "attestation verify")
    [ "${FAKE_ATTEST:-pass}" = pass ] && exit 0
    echo "Error: failed to fetch attestations: HTTP 404: Not Found" >&2; exit 1 ;;
  "release download")
    shift 3; dir=. pats=()
    while [ $# -gt 0 ]; do case $1 in --dir) dir=$2; shift 2 ;; --pattern) pats+=("$2"); shift 2 ;; *) shift ;; esac; done
    mkdir -p "$dir"
    for p in "${pats[@]}"; do for f in "$MIRROR"/$p; do [ -e "$f" ] && cp "$f" "$dir/"; done; done ;;
  *) echo "gh stand-in: unexpected: $*" >&2; exit 9 ;;
esac
SH
# sudo: runs the command, except as another user (validator init), which it only records.
cat > "$T/bin/sudo" << 'SH'
#!/bin/sh
echo "$*" >> "$LOG/sudo.log"
[ "$1" = -u ] && exit 0
exec "$@"
SH
printf '#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo x86_64 ;; *) echo Linux ;; esac\n' > "$T/bin/uname"
chmod +x "$T/bin/"*
ROOTFS=$T/root # stands in for / : usr/local/bin and var/lib/blockula
PATH_RUN="$T/bin:$ROOTFS/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

"$BIN" keygen > "$T/release-key.json" 2> /dev/null
"$BIN" keygen > "$T/attacker-key.json" 2> /dev/null
"$BIN" keygen > "$T/new-key.json" 2> /dev/null
K=$("$BIN" release keyid --key "$T/release-key.json")
AK=$("$BIN" release keyid --key "$T/attacker-key.json")
NK=$("$BIN" release keyid --key "$T/new-key.json")
[[ $K =~ ^[0-9a-f]{64}$ ]] || fail "release keyid printed '$K'"

# wrapper VERSION: a `blockula` that reports VERSION and is the real binary otherwise.
wrapper() { printf '#!/bin/sh\nif [ "$1" = --version ]; then echo "blockula %s"; exit 0; fi\nexec "%s" "$@"\n' "$1" "$BIN"; }
# release DIR TAG KEY [KIND]: a release as release.yml makes it (an archive for each target used
# here, install.sh, a genesis file, checksums.txt naming TAG) signed with KEY as the offline
# machine does. KIND attacker: the archived binary is the attacker's (it leaves a marker whenever
# it runs). The release's install.sh leaves a marker too, so a run that executes it shows.
release() {
  local dir=$1 tag=$2 key=$3 kind=${4:-genuine} t
  rm -rf "$dir" "$T/stage" && mkdir -p "$dir"
  for t in $(printf '%s\n' "$LINUX_T" "$HOST_T" | sort -u); do
    mkdir -p "$T/stage/blockula-$t"
    if [ "$kind" = attacker ]; then printf '#!/bin/sh\ntouch "%s/ATTACKER_RAN"\n[ "$1" = --version ] && echo "blockula %s"\nexit 0\n' "$T" "${tag#v}"
    else wrapper "${tag#v}"; fi > "$T/stage/blockula-$t/blockula"
    chmod 755 "$T/stage/blockula-$t/blockula"
    tar -czf "$dir/blockula-$t.tar.gz" -C "$T/stage" "blockula-$t"
  done
  printf '#!/bin/sh\ntouch "%s/INSTALL_SH_RAN"\n' "$T" > "$dir/install.sh"
  echo '{"chain_id":"blockula-1"}' > "$dir/genesis-blockula-1.json"
  scripts/release-checksums.sh "$tag" "$dir" > /dev/null
  "$BIN" release sign --key "$key" "$dir/checksums.txt" > "$dir/checksums.txt.sig" 2> /dev/null
}
sha() { if command -v sha256sum > /dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

# machine STATE: a fresh host (no blockula anywhere, nothing pinned), or one that ran VERSION
# from an earlier install and pinned the release key.
machine() {
  rm -rf "$ROOTFS" "$T/home" "$T/ATTACKER_RAN" "$T/INSTALL_SH_RAN"
  mkdir -p "$ROOTFS/usr/local/bin" "$ROOTFS/var/lib/blockula" "$T/home"
  if [ "$1" != fresh ]; then
    wrapper "$1" > "$ROOTFS/usr/local/bin/blockula" && chmod 755 "$ROOTFS/usr/local/bin/blockula"
    mkdir -p "$T/home/.blockula/bin" && cp "$ROOTFS/usr/local/bin/blockula" "$T/home/.blockula/bin/blockula"
    echo "$K" > "$T/home/.blockula/release-key-id"
  fi
}
installed() { "$ROOTFS/usr/local/bin/blockula" --version 2> /dev/null || echo none; }

# paste NAME [unchained]: block NAME with its placeholders filled in, run as a paste in a fresh
# directory (with the files in $WORKFILES, if any). "unchained" takes its subshell off.
paste() {
  local name=$1 mode=${2:-chained} f=$T/block-$1.sh
  block "$name" | sed \
    -e "s|^\( *\)V=vX\.Y\.Z|\1V=$TAG|" \
    -e "s|^\( *\)K=<release key id>.*|\1K=$K|" \
    -e "s|^\( *\)H=<.*|\1H=$ANNOUNCED|" \
    -e "s|^\( *\)NEW=<.*|\1NEW=$NK|" \
    -e "s|^\( *\)P=<.*|\1P=$PATCH|" \
    -e "s|<your-ip>|203.0.113.7|; s|<your sentry>|sentry.example.org|" \
    -e "s|/usr/local/bin/|$ROOTFS/usr/local/bin/|g; s|/var/lib/blockula|$ROOTFS/var/lib/blockula|g" > "$f"
  if grep -nE '<[a-zA-Z][^>]*>|vX\.Y\.Z' "$f"; then fail "block $name has a placeholder this test doesn't fill in (above): update the test"; fi
  if [ "$mode" = unchained ]; then sed -i.bak -e '1d' -e '$d' "$f"; fi
  rm -rf "$T/work" && mkdir -p "$T/work" && : > "$T/sudo.log" && : > "$T/gh.log"
  [ -z "${WORKFILES:-}" ] || cp -R "$WORKFILES"/. "$T/work/"
  set +e
  (cd "$T/work" && env -i PATH="$PATH_RUN" HOME="$T/home" TMPDIR="$T/tmp" LOG="$T" MIRROR="$MIRROR" FAKE_ATTEST="${FAKE_ATTEST:-pass}" \
    BLOCKULA_RELEASE_URL="file://$MIRROR" ${PASTE_SHELL:-bash} "$f" < /dev/null) > "$T/out" 2>&1
  rc=$?
  set -e
}
# verdict: what the paste did. FAIL-OPEN if anything from the release ran that the checks were
# meant to stop, or a step ran after one failed; else "installed <version>" or "refused".
verdict() {
  if [ -e "$T/ATTACKER_RAN" ]; then echo "FAIL-OPEN: the attacker's binary ran"; return; fi
  if [ -e "$T/INSTALL_SH_RAN" ]; then echo "FAIL-OPEN: the release's install.sh ran"; return; fi
  if grep -qE '^Error:|✗|FAILED|refusing|Refusing' "$T/out" && [ -s "$T/sudo.log" ]; then echo "FAIL-OPEN: a step ran after a failed one ($(head -n 1 "$T/sudo.log"))"; return; fi
  if [ $rc = 0 ]; then echo "installed $(installed)"; else echo refused; fi
}
# expect NAME VERDICT WHY [unchained]
expect() {
  local name=$1 want=$2 why=$3 got
  paste "$name" "${4:-chained}"
  got=$(verdict)
  [ "$got" = "$want" ] || fail "$name, $why: expected '$want', got '$got'. Output:
$(cat "$T/out")"
  ok "$name, $why: $want"
}
# fails_open NAME WHY: the same block without its subshell must fail open (the test is real).
fails_open() {
  paste "$1" unchained
  case $(verdict) in FAIL-OPEN:*) ok "$1 without '( set -euo pipefail … )', $2: fails open ($(verdict | cut -d: -f2- | sed 's/^ //')), so this test catches it" ;;
    *) fail "$1 without its subshell, $2: expected the run to fail open, got '$(verdict)'. Output:
$(cat "$T/out")" ;; esac
}

PATCH= WORKFILES=
GENUINE=$T/genuine && release "$GENUINE" v9.9.9 "$T/release-key.json"
ANNOUNCED=$(sha "$GENUINE/blockula-$LINUX_T.tar.gz") # what the signer announces in the channel

# ---- a first install (no blockula on the host; review-r2-supply-1) ----
TAG=v9.9.9
machine fresh && MIRROR=$GENUINE
env -i PATH="$PATH_RUN" sh -c 'command -v blockula' > /dev/null && fail "the fresh host has a blockula on PATH"
expect first-install "installed blockula 9.9.9" "the genuine release on a fresh host"
[ "$(cat "$T/home/.blockula/release-key-id")" = "$K" ] || fail "the first install didn't pin K"
grep -qF "release key $K (given; pinned once this release verifies)" "$T/out" || fail "the first install didn't check the signature: $(cat "$T/out")"
ok "first-install pinned the release key id it was given"
# GitHub serves the attacker's archive (re-signed checksums, its provenance): the announcement stops it.
EVIL=$T/evil && release "$EVIL" v9.9.9 "$T/attacker-key.json" attacker
machine fresh && MIRROR=$EVIL
expect first-install refused "an attacker's archive and signature (provenance passes)"
grep -qF "FAILED" "$T/out" || fail "refused for another reason: $(cat "$T/out")"
[ ! -e "$T/home/.blockula/release-key-id" ] || fail "a refused first install pinned a key"
fails_open first-install "an attacker's archive"
# The genuine archive, but checksums.txt re-signed by another key: the binary refuses its signature.
RESIGNED=$T/resigned && rm -rf "$RESIGNED" && cp -R "$GENUINE" "$RESIGNED"
"$BIN" release sign --key "$T/attacker-key.json" "$RESIGNED/checksums.txt" > "$RESIGNED/checksums.txt.sig" 2> /dev/null
machine fresh && MIRROR=$RESIGNED
expect first-install refused "the genuine archive under checksums re-signed with another key"
grep -qF "signed by key $AK, not the release key $K" "$T/out" || fail "refused for another reason: $(cat "$T/out")"
fails_open first-install "checksums re-signed with another key"

# ---- upgrades (the pinned key; F49's bypass: the release controls install.sh) ----
TAG=v9.9.10
NEXT=$T/next && release "$NEXT" v9.9.10 "$T/release-key.json"
machine 9.9.9 && MIRROR=$NEXT
expect upgrade "installed blockula 9.9.10" "the genuine next release"
grep -qF "release key $K (pinned)" "$T/out" || fail "the upgrade didn't check against the pin: $(cat "$T/out")"
grep -qF "✓ build provenance" "$T/out" || fail "the upgrade didn't check provenance: $(cat "$T/out")"
EVIL2=$T/evil2 && release "$EVIL2" v9.9.10 "$T/attacker-key.json" attacker
machine 9.9.9 && MIRROR=$EVIL2
expect upgrade refused "a release signed by another key, with its own install.sh (F49's adversary)"
[ "$(installed)" = "blockula 9.9.9" ] || fail "the refused upgrade changed the installed binary"
fails_open upgrade "a release signed by another key"
SWAPPED=$T/swapped && release "$SWAPPED" v9.9.10 "$T/release-key.json"
cp "$EVIL2/blockula-$HOST_T.tar.gz" "$SWAPPED/"
machine 9.9.9 && MIRROR=$SWAPPED
expect upgrade refused "an archive swapped under the genuine signature"
grep -qF "doesn't match the signed checksums.txt" "$T/out" || fail "refused for another reason: $(cat "$T/out")"
OLDER=$T/older && release "$OLDER" v9.9.8 "$T/release-key.json"
machine 9.9.9 && MIRROR=$OLDER
expect upgrade refused "an older genuinely signed release served as v9.9.10"
grep -qF 'for release "v9.9.8", not v9.9.10' "$T/out" || fail "refused for another reason: $(cat "$T/out")"
UNSIGNED=$T/unsigned && release "$UNSIGNED" v9.9.10 "$T/release-key.json" && rm "$UNSIGNED/checksums.txt.sig"
machine 9.9.9 && MIRROR=$UNSIGNED
expect upgrade refused "a release without checksums.txt.sig"
machine 9.9.9 && MIRROR=$NEXT
FAKE_ATTEST=none expect upgrade refused "a genuine signature but no provenance for a public release"

# The same blocks paste into zsh (macOS's shell), where installed.
if command -v zsh > /dev/null; then
  machine 9.9.9 && MIRROR=$NEXT
  PASTE_SHELL="zsh -f" expect upgrade "installed blockula 9.9.10" "pasted into zsh, the genuine next release"
  machine 9.9.9 && MIRROR=$EVIL2
  PASTE_SHELL="zsh -f" expect upgrade refused "pasted into zsh, a release signed by another key"
fi

# ---- the emergency patch (built in the private fork: no attestation; review-r2-supply-3) ----
TAG=v9.9.11
PATCH=$T/patch && release "$PATCH" v9.9.11 "$T/release-key.json" && rm "$PATCH/install.sh" "$PATCH/genesis-blockula-1.json"
rm "$PATCH/checksums.txt" && scripts/release-checksums.sh v9.9.11 "$PATCH" > /dev/null && "$BIN" release sign --key "$T/release-key.json" "$PATCH/checksums.txt" > "$PATCH/checksums.txt.sig" 2> /dev/null
machine 9.9.10 && MIRROR=$T/empty && mkdir -p "$MIRROR"
FAKE_ATTEST=none expect patch-install "installed blockula 9.9.11" "a signed patch with a logged-in gh and no attestation"
grep -qF "Not checked: build provenance" "$T/out" || fail "the patch install didn't say provenance wasn't checked: $(cat "$T/out")"
grep -q attestation "$T/gh.log" && fail "the patch install asked gh for provenance: $(cat "$T/gh.log")"
EVILPATCH=$T/evilpatch && release "$EVILPATCH" v9.9.11 "$T/attacker-key.json" attacker
machine 9.9.10 && PATCH=$EVILPATCH
FAKE_ATTEST=none expect patch-install refused "a patch signed by another key"
FAKE_ATTEST=none fails_open patch-install "a patch signed by another key"

# ---- the genesis file for a new validator ----
TAG=v9.9.10
machine 9.9.10 && MIRROR=$NEXT
expect validator-genesis "installed blockula 9.9.10" "the release's genesis file"
cmp -s "$ROOTFS/var/lib/blockula/genesis-blockula-1.json" "$NEXT/genesis-blockula-1.json" || fail "the genesis file wasn't installed"
grep -qF -- "-u blockula $ROOTFS/usr/local/bin/blockula validator init --home $ROOTFS/var/lib/blockula/val --genesis $ROOTFS/var/lib/blockula/genesis-blockula-1.json" "$T/sudo.log" || fail "validator init didn't get the verified genesis file: $(cat "$T/sudo.log")"
ok "validator-genesis: validator init got the verified genesis file"
BADGEN=$T/badgenesis && release "$BADGEN" v9.9.10 "$T/release-key.json" && echo '{"chain_id":"evil"}' > "$BADGEN/genesis-blockula-1.json"
machine 9.9.10 && MIRROR=$BADGEN
expect validator-genesis refused "a genesis file that isn't the signed one"
[ ! -e "$ROOTFS/var/lib/blockula/genesis-blockula-1.json" ] || fail "the swapped genesis file was installed"
fails_open validator-genesis "a swapped genesis file"

# ---- moving the pin to a new release key ----
printf '# blockula-release-key-rotation\nold %s\nnew %s\n' "$K" "$NK" > "$T/rotation.txt"
"$BIN" release sign --key "$T/new-key.json" "$T/rotation.txt" > "$T/rotation.txt.sig" 2> /dev/null
mkdir -p "$T/stmt" && cp "$T/rotation.txt" "$T/rotation.txt.sig" "$T/stmt/" && WORKFILES=$T/stmt
machine 9.9.10
expect rotation refused "a rotation statement signed by the new key itself"
[ "$(cat "$T/home/.blockula/release-key-id")" = "$K" ] || fail "a refused rotation moved the pin"
# (one command: nothing can run after it, so there's no unchained variant to check)
"$BIN" release sign --key "$T/release-key.json" "$T/rotation.txt" > "$T/stmt/rotation.txt.sig" 2> /dev/null
machine 9.9.10
expect rotation "installed blockula 9.9.10" "a rotation statement signed by the pinned key"
[ "$(cat "$T/home/.blockula/release-key-id")" = "$NK" ] || fail "the rotation didn't move the pin"
WORKFILES=
# After it, a release signed with the new key upgrades; the old key's don't.
TAG=v9.9.12
ROTATED=$T/rotated && release "$ROTATED" v9.9.12 "$T/new-key.json"
machine 9.9.10 && echo "$NK" > "$T/home/.blockula/release-key-id" && MIRROR=$ROTATED
expect upgrade "installed blockula 9.9.12" "a release signed by the new key, after the rotation"
OLDKEY=$T/oldkey && release "$OLDKEY" v9.9.12 "$T/release-key.json"
machine 9.9.10 && echo "$NK" > "$T/home/.blockula/release-key-id" && MIRROR=$OLDKEY
expect upgrade refused "a release signed by the old key, after the rotation"

echo "✓ release runbook: $pass cases"
