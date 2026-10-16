#!/usr/bin/env bash
# Tests scripts/install.sh's verification policy offline (audit F49): a fake release served from a
# file:// mirror (BLOCKULA_RELEASE_URL), and stand-ins for `uname`, `gh` and an installed
# `blockula` that can verify release signatures, on a PATH with nothing else of yours on it.
#   bash scripts/test-install.sh
set -euo pipefail
cd "$(dirname "$0")/.."
INSTALL=$PWD/scripts/install.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
KEY=$(printf 'ab%.0s' $(seq 32))
target=x86_64-unknown-linux-gnu

# The stand-ins. The fake gh's behaviour comes from FAKE_GH (ok, fail, logged-out); the verifier
# accepts a signature file containing "good" for $KEY.
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
  "attestation ok") exit 0 ;;
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
chmod +x "$T/bin/"* "$T/verifier/blockula"

# The release: an archive whose blockula prints a version, and checksums.txt.
mkdir -p "$T/src/blockula-$target" "$T/release"
printf '#!/bin/sh\necho "blockula 9.9.9-test"\n' > "$T/src/blockula-$target/blockula"
chmod +x "$T/src/blockula-$target/blockula"
tar -czf "$T/release/blockula-$target.tar.gz" -C "$T/src" "blockula-$target"
(cd "$T/release" && { if command -v sha256sum >/dev/null; then sha256sum blockula-*.tar.gz; else shasum -a 256 blockula-*.tar.gz; fi; } > checksums.txt)

pass=0
# run NAME EXPECT(installs|refuses) PATTERN [VAR=value…]: runs install.sh in a clean environment.
run() {
  local name=$1 expect=$2 pattern=$3; shift 3
  local dir=$T/install-$pass out code=0
  rm -rf "$dir"
  out=$(env -i HOME="$T/home" PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" BLOCKULA_INSTALL_DIR="$dir" BLOCKULA_RELEASE_URL="file://$T/release" "$@" /bin/sh "$INSTALL" 2>&1) || code=$?
  local installed=no
  [ -x "$dir/blockula" ] && installed=yes
  if { [ "$expect" = installs ] && { [ $code -ne 0 ] || [ $installed = no ]; }; } || { [ "$expect" = refuses ] && { [ $code -eq 0 ] || [ $installed = yes ]; }; }; then
    printf '✗ %s: expected it %s (exit %s, installed: %s)\n%s\n' "$name" "$expect" "$code" "$installed" "$out" >&2
    exit 1
  fi
  if ! printf '%s' "$out" | grep -qF -- "$pattern"; then
    printf '✗ %s: output lacks "%s"\n%s\n' "$name" "$pattern" "$out" >&2
    exit 1
  fi
  pass=$((pass + 1))
  echo "✓ $name"
}
sig() { rm -f "$T/release/checksums.txt.sig"; [ "$1" = none ] || echo "$1" > "$T/release/checksums.txt.sig"; }
WITH_VERIFIER="PATH=$T/bin:$T/verifier:/usr/bin:/bin:/usr/sbin:/sbin"

# Before the key ceremony (no key id): provenance is the check.
sig none
run "no key id: provenance verifies" installs "Verified: build provenance"
run "no key id, no gh: checksum only, with a warning" installs "ONLY THE CHECKSUM WAS VERIFIED" FAKE_GH=logged-out
run "no key id, no gh, REQUIRE_VERIFY: refused" refuses "build provenance wasn't checked" FAKE_GH=logged-out BLOCKULA_REQUIRE_VERIFY=1
run "failed provenance: refused" refuses "provenance check FAILED" FAKE_GH=fail

# With a release key id, a release must be signed (F49): provenance alone never installs.
run "key id, no .sig: refused even with provenance" refuses "no checksums.txt.sig" BLOCKULA_RELEASE_KEY_ID="$KEY"
run "key id, no .sig, REQUIRE_VERIFY: refused" refuses "no checksums.txt.sig" BLOCKULA_RELEASE_KEY_ID="$KEY" BLOCKULA_REQUIRE_VERIFY=1 "$WITH_VERIFIER"
run "key id, no .sig, no gh: refused" refuses "no checksums.txt.sig" BLOCKULA_RELEASE_KEY_ID="$KEY" FAKE_GH=logged-out "$WITH_VERIFIER"
sig good
run "key id, valid signature and provenance, REQUIRE_VERIFY: installs" installs "release signature (ML-DSA-65 key abababababababab…)" BLOCKULA_RELEASE_KEY_ID="$KEY" BLOCKULA_REQUIRE_VERIFY=1 "$WITH_VERIFIER"
run "key id, valid signature, no gh: installs, says provenance wasn't checked" installs "Not checked: build provenance" BLOCKULA_RELEASE_KEY_ID="$KEY" FAKE_GH=logged-out "$WITH_VERIFIER"
run "key id, valid signature, no gh, REQUIRE_VERIFY: refused (both checks)" refuses "build provenance wasn't checked" BLOCKULA_RELEASE_KEY_ID="$KEY" FAKE_GH=logged-out BLOCKULA_REQUIRE_VERIFY=1 "$WITH_VERIFIER"
run "key id, signature but no verifier: installs, says the signature wasn't checked" installs "Not checked: release signature" BLOCKULA_RELEASE_KEY_ID="$KEY"
run "key id, signature but no verifier, REQUIRE_VERIFY: refused" refuses "the release signature wasn't checked" BLOCKULA_RELEASE_KEY_ID="$KEY" BLOCKULA_REQUIRE_VERIFY=1
run "key id in upper case is the same key" installs "release signature" BLOCKULA_RELEASE_KEY_ID="$(printf %s "$KEY" | tr a-f A-F)" "$WITH_VERIFIER"
sig forged
run "key id, invalid signature: refused" refuses "signature check FAILED" BLOCKULA_RELEASE_KEY_ID="$KEY" "$WITH_VERIFIER"
run "a malformed key id: refused" refuses "64 hex characters" BLOCKULA_RELEASE_KEY_ID=abc

# A tampered archive.
sig none
echo "00$(head -c 62 /dev/zero | tr '\0' 0)  blockula-$target.tar.gz" > "$T/release/checksums.txt"
run "checksum mismatch: refused" refuses "Checksum mismatch"

echo "✓ install.sh: $pass cases"
