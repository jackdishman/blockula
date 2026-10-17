#!/usr/bin/env bash
# Tests scripts/install.sh's verification policy offline (audit F49, supply-chain-B-2): fake
# releases served from file:// mirrors (BLOCKULA_RELEASE_URL) or through a stand-in `curl` that
# plays GitHub (the latest-release redirect and release downloads), and stand-ins for `uname`, `gh`
# and an installed `blockula` that can verify release signatures, on a PATH with nothing else of
# yours on it.
#   bash scripts/test-install.sh
#   INSTALL_SH=/path/to/an/older/install.sh KEEP_GOING=1 bash scripts/test-install.sh   (what it misses)
#   TEST_SH=/bin/dash bash scripts/test-install.sh   (another POSIX shell; CI's /bin/sh is dash)
set -euo pipefail
cd "$(dirname "$0")/.."
INSTALL=${INSTALL_SH:-$PWD/scripts/install.sh}
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
KEY=$(printf 'ab%.0s' $(seq 32))
OTHER_KEY=$(printf 'cd%.0s' $(seq 32))
target=x86_64-unknown-linux-gnu
REPO_URL=https://github.com/jackdishman/blockula

# The stand-ins. The fake gh's behaviour comes from FAKE_GH (ok, fail, logged-out); like the real
# one, it refuses when --source-ref names another ref than the one the attestation was made on
# (FAKE_GH_REF, default the v9.9.9 tag), and it logs its arguments to GH_LOG. The verifier
# accepts a signature file containing "good" for $KEY, whatever it signs (so an older release's
# genuine signature verifies too, as it would).
mkdir -p "$T/bin" "$T/verifier"
cat > "$T/bin/uname" <<'SH'
#!/bin/sh
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; *) echo Linux ;; esac
SH
cat > "$T/bin/gh" <<'SH'
#!/bin/sh
case "$1 ${FAKE_GH:-ok}" in
  "auth logged-out") exit 1 ;;
  "auth "*) exit 0 ;;
  "attestation ok")
    echo "$*" >> "$GH_LOG"
    ref=""; prev=""
    for a in "$@"; do [ "$prev" = --source-ref ] && ref=$a; prev=$a; done
    if [ -n "$ref" ] && [ "$ref" != "${FAKE_GH_REF:-refs/tags/v9.9.9}" ]; then echo "source ref $ref doesn't match the attestation's" >&2; exit 1; fi
    exit 0 ;;
  "attestation "*) echo "no attestation" >&2; exit 1 ;;
esac
exit 2
SH
cat > "$T/verifier/blockula" <<SH
#!/bin/sh
[ "\$1 \$2" = "release verify" ] || exit 2
[ "\$3" = --help ] && exit 0
[ "\$4" = "$KEY" ] && grep -q good "\$6"
SH
# GitHub: where /releases/latest redirects to (FAKE_LATEST), and the v9.9.9 release's files. No
# other network.
cat > "$T/bin/curl" <<SH
#!/usr/bin/env bash
args=()
for a in "\$@"; do
  case \$a in
    $REPO_URL/releases/latest) printf '%s' "\${FAKE_LATEST:-$REPO_URL/releases/tag/v9.9.9}"; exit 0 ;;
    $REPO_URL/releases/download/v9.9.9/*) args+=("file://$T/release/\${a##*/}") ;;
    http://* | https://*) echo "curl: (22) not in this test: \$a" >&2; exit 22 ;;
    *) args+=("\$a") ;;
  esac
done
exec /usr/bin/curl "\${args[@]}"
SH
chmod +x "$T/bin/"* "$T/verifier/blockula"

# release DIR TAG PRINTS [no-tag-line]: an archive whose blockula prints PRINTS for --version, and
# checksums.txt as release.yml writes it (scripts/release-checksums.sh), or without its first line
# naming the release, as before v0.5.0.
release() {
  local dir=$1 tag=$2 prints=$3 tagline=${4:-}
  rm -rf "$dir" "$T/src"
  mkdir -p "$T/src/blockula-$target" "$dir"
  printf '#!/bin/sh\necho "%s"\n' "$prints" > "$T/src/blockula-$target/blockula"
  chmod +x "$T/src/blockula-$target/blockula"
  tar -czf "$dir/blockula-$target.tar.gz" -C "$T/src" "blockula-$target"
  scripts/release-checksums.sh "$tag" "$dir" > /dev/null
  [ "$(head -n 1 "$dir/checksums.txt")" = "# blockula-release $tag" ] || { echo "✗ release-checksums.sh didn't name $tag first" >&2; exit 1; }
  if [ "$tagline" = no-tag-line ]; then sed -i.bak 1d "$dir/checksums.txt" && rm "$dir/checksums.txt.bak"; fi
}
# sig DIR none|CONTENT: the release's checksums.txt.sig.
sig() { rm -f "$1/checksums.txt.sig"; [ "$2" = none ] || echo "$2" > "$1/checksums.txt.sig"; }

release "$T/release" v9.9.9 "blockula 9.9.9"
# An older release, genuinely built, attested (on its own tag) and signed: what a mirror or a proxy
# would replay as the version you asked for.
release "$T/old" v9.9.8 "blockula 9.9.8"
sig "$T/old" good

pass=0
failed=0
# A failed case stops the test, or with KEEP_GOING=1 is counted and the rest still run.
failure() {
  printf '%s\n' "$1" >&2
  [ "${KEEP_GOING:-}" = 1 ] || exit 1
  failed=$((failed + 1))
}
script=$INSTALL
# run NAME EXPECT(installs|refuses) PATTERN [VAR=value…]: runs install.sh in a clean environment,
# by default from the v9.9.9 mirror (later VAR=value arguments override, VAR= unsets).
run() {
  local name=$1 expect=$2 pattern=$3; shift 3
  local dir=$T/install-$pass out code=0
  rm -rf "$dir"
  : > "$T/gh.log"
  out=$(env -i HOME="$T/home" PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" GH_LOG="$T/gh.log" BLOCKULA_INSTALL_DIR="$dir" \
    BLOCKULA_VERSION=v9.9.9 BLOCKULA_RELEASE_URL="file://$T/release" "$@" "${TEST_SH:-/bin/sh}" "$script" 2>&1) || code=$?
  local installed=no
  [ -x "$dir/blockula" ] && installed=yes
  if { [ "$expect" = installs ] && { [ $code -ne 0 ] || [ $installed = no ]; }; } || { [ "$expect" = refuses ] && { [ $code -eq 0 ] || [ $installed = yes ]; }; }; then
    failure "$(printf '✗ %s: expected it %s (exit %s, installed: %s)\n%s' "$name" "$expect" "$code" "$installed" "$out")"
    return
  fi
  if ! printf '%s' "$out" | grep -qF -- "$pattern"; then
    failure "$(printf '✗ %s: output lacks "%s"\n%s' "$name" "$pattern" "$out")"
    return
  fi
  pass=$((pass + 1))
  echo "✓ $name"
}
# gh_saw TEXT: the last run's `gh attestation verify` call included TEXT.
gh_saw() {
  grep -qF -- "$1" "$T/gh.log" || failure "$(printf '✗ gh attestation verify was not called with "%s":\n%s' "$1" "$(cat "$T/gh.log")")"
}
WITH_VERIFIER="PATH=$T/bin:$T/verifier:/usr/bin:/bin:/usr/sbin:/sbin"

# Before the key ceremony (no key id): provenance is the check.
sig "$T/release" none
run "no key id: provenance verifies" installs "Verified: build provenance"
gh_saw "--source-ref refs/tags/v9.9.9"
run "no key id, no gh: checksum only, with a warning" installs "ONLY THE CHECKSUM WAS VERIFIED" FAKE_GH=logged-out
run "no key id, no gh, REQUIRE_VERIFY: refused" refuses "build provenance wasn't checked" FAKE_GH=logged-out BLOCKULA_REQUIRE_VERIFY=1
run "failed provenance: refused" refuses "provenance check FAILED" FAKE_GH=fail

# The version you asked for is the version you get (supply-chain-B-2): the signed checksums.txt
# names its tag, provenance must come from that tag, and the binary must say it's that version.
run "an older release replayed as the requested one (mirror, signature, provenance): refused" refuses "checksums.txt is for v9.9.8, not v9.9.9" \
  BLOCKULA_RELEASE_URL="file://$T/old" BLOCKULA_RELEASE_KEY_ID="$KEY" FAKE_GH_REF=refs/tags/v9.9.8 "$WITH_VERIFIER"
run "an older release replayed, REQUIRE_VERIFY: refused" refuses "checksums.txt is for v9.9.8, not v9.9.9" \
  BLOCKULA_RELEASE_URL="file://$T/old" BLOCKULA_RELEASE_KEY_ID="$KEY" FAKE_GH_REF=refs/tags/v9.9.8 BLOCKULA_REQUIRE_VERIFY=1 "$WITH_VERIFIER"
run "an older release replayed, checksum only: refused" refuses "checksums.txt is for v9.9.8, not v9.9.9" BLOCKULA_RELEASE_URL="file://$T/old" FAKE_GH=logged-out
release "$T/untagged" v9.9.8 "blockula 9.9.8" no-tag-line
run "checksums.txt naming no release (before v0.5): refused" refuses "names no release" BLOCKULA_RELEASE_URL="file://$T/untagged"
run "provenance from another ref (a branch build of release.yml): refused" refuses "provenance check FAILED" FAKE_GH_REF=refs/heads/release-yml-on-a-branch
gh_saw "--source-ref refs/tags/v9.9.9"
release "$T/mislabelled" v9.9.9 "blockula 9.9.8"
run "a binary that isn't the requested version: refused, not installed" refuses "is blockula 9.9.8, not 9.9.9" BLOCKULA_RELEASE_URL="file://$T/mislabelled" FAKE_GH=logged-out
run "a mirror without BLOCKULA_VERSION: refused" refuses "set BLOCKULA_VERSION" BLOCKULA_VERSION=
run "a malformed BLOCKULA_VERSION: refused" refuses "vX.Y.Z" BLOCKULA_VERSION=v9.9
run "latest is resolved to its tag first, then bound like any version" installs "Installed blockula 9.9.9" BLOCKULA_VERSION= BLOCKULA_RELEASE_URL=
gh_saw "--source-ref refs/tags/v9.9.9"
run "latest resolving to something other than a vX.Y.Z tag: refused" refuses "isn't a release tag" BLOCKULA_VERSION= BLOCKULA_RELEASE_URL= FAKE_LATEST="$REPO_URL/releases/tag/nightly"
run "a pinned version from GitHub" installs "Installed blockula 9.9.9" BLOCKULA_RELEASE_URL=

# With a release key id, a release must be signed (F49): provenance alone never installs.
run "key id, no .sig: refused even with provenance" refuses "no checksums.txt.sig" BLOCKULA_RELEASE_KEY_ID="$KEY"
run "key id, no .sig, REQUIRE_VERIFY: refused" refuses "no checksums.txt.sig" BLOCKULA_RELEASE_KEY_ID="$KEY" BLOCKULA_REQUIRE_VERIFY=1 "$WITH_VERIFIER"
run "key id, no .sig, no gh: refused" refuses "no checksums.txt.sig" BLOCKULA_RELEASE_KEY_ID="$KEY" FAKE_GH=logged-out "$WITH_VERIFIER"
sig "$T/release" good
run "key id, valid signature and provenance, REQUIRE_VERIFY: installs" installs "release signature (ML-DSA-65 key abababababababab…, from BLOCKULA_RELEASE_KEY_ID)" BLOCKULA_RELEASE_KEY_ID="$KEY" BLOCKULA_REQUIRE_VERIFY=1 "$WITH_VERIFIER"
run "key id, valid signature, no gh: installs, says provenance wasn't checked" installs "Not checked: build provenance" BLOCKULA_RELEASE_KEY_ID="$KEY" FAKE_GH=logged-out "$WITH_VERIFIER"
run "key id, valid signature, no gh, REQUIRE_VERIFY: refused (both checks)" refuses "build provenance wasn't checked" BLOCKULA_RELEASE_KEY_ID="$KEY" FAKE_GH=logged-out BLOCKULA_REQUIRE_VERIFY=1 "$WITH_VERIFIER"
run "key id given, signature but no verifier: refused (you asked for the signature)" refuses "can't check the release signature" BLOCKULA_RELEASE_KEY_ID="$KEY"
run "key id, signature but no verifier, REQUIRE_VERIFY: refused" refuses "can't check the release signature" BLOCKULA_RELEASE_KEY_ID="$KEY" BLOCKULA_REQUIRE_VERIFY=1
run "key id in upper case is the same key" installs "release signature" BLOCKULA_RELEASE_KEY_ID="$(printf %s "$KEY" | tr a-f A-F)" "$WITH_VERIFIER"
sig "$T/release" forged
run "key id, invalid signature: refused" refuses "signature check FAILED" BLOCKULA_RELEASE_KEY_ID="$KEY" "$WITH_VERIFIER"
# The reviewer's bypass of the first F49 fix: junk instead of a signature, on a machine with no
# verifier, installed on provenance alone. With the key id given, that's refused now.
sig "$T/release" junk
run "key id given, junk .sig, no verifier: refused" refuses "can't check the release signature" BLOCKULA_RELEASE_KEY_ID="$KEY"
run "a malformed key id: refused" refuses "64 hex characters" BLOCKULA_RELEASE_KEY_ID=abc

# A key id built into the script (after the key ceremony). It comes with the download, so it's no
# stronger than the script (docs/OPERATIONS.md "Installing and upgrading"); the operator's own key id
# wins any disagreement.
script=$T/install-with-key.sh
sed "s/^BUILTIN_KEY_ID=\"\"/BUILTIN_KEY_ID=\"$KEY\"/" "$INSTALL" > "$script"
grep -q "^BUILTIN_KEY_ID=\"$KEY\"" "$script" || { echo "✗ couldn't build the key into a copy of install.sh" >&2; exit 1; }
sig "$T/release" good
run "built-in key id, verifier, valid signature: installs" installs "release signature (ML-DSA-65 key abababababababab…, built into this script)" "$WITH_VERIFIER"
run "built-in key id, no verifier: installs with a loud warning" installs "THE RELEASE SIGNATURE WAS NOT CHECKED"
run "built-in key id, no verifier, REQUIRE_VERIFY: refused" refuses "can't check the release signature" BLOCKULA_REQUIRE_VERIFY=1
rm -rf "$T/unsigned" && cp -R "$T/release" "$T/unsigned" && sig "$T/unsigned" none
run "built-in key id, no .sig: refused" refuses "no checksums.txt.sig" BLOCKULA_RELEASE_URL="file://$T/unsigned" "$WITH_VERIFIER"
run "built-in key id other than BLOCKULA_RELEASE_KEY_ID: refused" refuses "isn't the key id you gave" BLOCKULA_RELEASE_KEY_ID="$OTHER_KEY" "$WITH_VERIFIER"
run "built-in key id equal to BLOCKULA_RELEASE_KEY_ID: installs" installs "from BLOCKULA_RELEASE_KEY_ID" BLOCKULA_RELEASE_KEY_ID="$KEY" "$WITH_VERIFIER"
script=$INSTALL

# A tampered archive.
sig "$T/release" none
{ echo "# blockula-release v9.9.9"; echo "00$(head -c 62 /dev/zero | tr '\0' 0)  blockula-$target.tar.gz"; } > "$T/release/checksums.txt"
run "checksum mismatch: refused" refuses "Checksum mismatch"

[ $failed = 0 ] || { echo "✗ install.sh: $failed failed, $pass passed" >&2; exit 1; }
echo "✓ install.sh: $pass cases"
