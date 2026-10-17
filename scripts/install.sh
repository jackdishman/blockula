#!/bin/sh
# 🧛 Install the `blockula` node + CLI from GitHub releases.
#   curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
# Env: BLOCKULA_VERSION (vX.Y.Z; default the latest release, resolved to its tag first),
#      BLOCKULA_INSTALL_DIR (default ~/.blockula/bin),
#      BLOCKULA_REQUIRE_VERIFY=1 (refuse what wasn't proven genuine: build provenance, and once a
#      release key id is known, the release signature too),
#      BLOCKULA_RELEASE_KEY_ID (the release key id to trust, from SECURITY.md, the website or your
#      operators' channel: never from the release itself; until the key ceremony there is none),
#      BLOCKULA_RELEASE_URL (a mirror, or a file:// directory, holding the release's files; needs
#      BLOCKULA_VERSION; every check below still applies).
#
# What gets checked (docs/OPERATIONS.md "Releases"):
#   0. The version. It's bound to everything below: checksums.txt must name it on its first line
#      ("# blockula-release vX.Y.Z", covered by the signature), provenance must come from its tag
#      (--source-ref), and the binary must report it. An older, genuinely signed release served as
#      the one you asked for is refused (audit supply-chain-B-2).
#   1. SHA-256 against the release's checksums.txt. Always. It only catches a corrupted download:
#      whoever can change the release can change checksums.txt too.
#   2. SLSA build provenance, with a logged-in GitHub CLI (`gh attestation verify`): the archive was
#      built by this repo's release workflow from the tag. A failed check is fatal; no gh skips it.
#   3. The offline ML-DSA-65 signature on checksums.txt (checksums.txt.sig) against the release key
#      id, with an already installed blockula that has `blockula release verify`. Once a key id is
#      known, every release must carry checksums.txt.sig, and an invalid one is fatal: leaving it
#      out is exactly what a compromised GitHub account or release workflow would do, and it can't
#      forge the offline signature (audit F49). With no installed verifier the signature can't be
#      checked at all, so a junk .sig would pass unseen: if you gave BLOCKULA_RELEASE_KEY_ID or
#      BLOCKULA_REQUIRE_VERIFY=1 the script refuses; otherwise it installs with a loud warning.
# The checks add up (each refuses a release it fails), and the script prints which ran.
#
# This script comes from the release too, and a compromised GitHub account or release workflow
# controls it (and any key id built into it): `gh attestation verify install.sh` only proves it
# came out of that workflow. Validators don't use it: they install with `blockula release install`,
# which checks the release against the key id they pinned at onboarding and never runs this script
# (docs/OPERATIONS.md "Installing and upgrading").
set -eu

REPO=jackdishman/blockula
DIR=${BLOCKULA_INSTALL_DIR:-$HOME/.blockula/bin}
VERSION=${BLOCKULA_VERSION:-latest}
# BLAKE3 of the release public key (docs/OPERATIONS.md "Releases"). Empty until the key ceremony.
# It comes with this script, so it's only as trustworthy as the script; BLOCKULA_RELEASE_KEY_ID,
# from an independent channel, is what an operator should rely on, and the two must agree.
BUILTIN_KEY_ID=""

hex_key() {
  k=$(printf %s "$1" | tr 'A-F' 'a-f')
  case "$k" in
    "") ;;
    *[!0-9a-f]*) echo "The release key id must be 64 hex characters." >&2; exit 1 ;;
    *) [ ${#k} -eq 64 ] || { echo "The release key id must be 64 hex characters." >&2; exit 1; } ;;
  esac
  printf %s "$k"
}
GIVEN_KEY_ID=$(hex_key "${BLOCKULA_RELEASE_KEY_ID:-}")
BUILTIN_KEY_ID=$(hex_key "$BUILTIN_KEY_ID")
if [ -n "$GIVEN_KEY_ID" ] && [ -n "$BUILTIN_KEY_ID" ] && [ "$GIVEN_KEY_ID" != "$BUILTIN_KEY_ID" ]; then
  echo "✗ This install.sh trusts release key $BUILTIN_KEY_ID, which isn't the key id you gave (BLOCKULA_RELEASE_KEY_ID=$GIVEN_KEY_ID); refusing to install." >&2
  echo "  If yours is the published one (SECURITY.md, the website, your operators' channel), this script isn't genuine." >&2
  exit 1
fi
if [ -n "$GIVEN_KEY_ID" ]; then RELEASE_KEY_ID=$GIVEN_KEY_ID; key_source="from BLOCKULA_RELEASE_KEY_ID"
else RELEASE_KEY_ID=$BUILTIN_KEY_ID; key_source="built into this script"; fi

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  *) echo "Sorry, no prebuilt blockula for $(uname -s) $(uname -m) yet. Build from source: cargo build --release --locked --manifest-path node/Cargo.toml" >&2; exit 1 ;;
esac

# 0. The version: an explicit vX.Y.Z tag from here on, which every check below is bound to.
is_tag() { printf %s "$1" | grep -Eqx 'v[0-9]+\.[0-9]+\.[0-9]+'; }
if [ "$VERSION" = latest ]; then
  if [ -n "${BLOCKULA_RELEASE_URL:-}" ]; then
    echo "A mirror (BLOCKULA_RELEASE_URL) can't say which release is the latest: set BLOCKULA_VERSION to the vX.Y.Z you want." >&2
    exit 1
  fi
  # GitHub redirects /releases/latest to /releases/tag/<tag>.
  latest=$(curl -fsSIL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
  VERSION=${latest##*/}
  is_tag "$VERSION" || { echo "The latest release ($latest) isn't a release tag (vX.Y.Z); refusing to install. Set BLOCKULA_VERSION." >&2; exit 1; }
  echo "🧛 The latest release is $VERSION."
fi
is_tag "$VERSION" || { echo "BLOCKULA_VERSION must be a release tag, vX.Y.Z (got '$VERSION')." >&2; exit 1; }

if [ -n "${BLOCKULA_RELEASE_URL:-}" ]; then base=$BLOCKULA_RELEASE_URL
else base=https://github.com/$REPO/releases/download/$VERSION; fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
asset=blockula-$target.tar.gz
echo "🧛 Downloading ${asset} ($VERSION)…"
curl -fsSL "$base/$asset" -o "$tmp/$asset"
curl -fsSL "$base/checksums.txt" -o "$tmp/checksums.txt"

# The release checksums.txt names (release.yml, scripts/release-checksums.sh). The signature in
# step 3 covers it, provenance in step 2 is bound to the same tag.
tagline=$(head -n 1 "$tmp/checksums.txt")
case "$tagline" in
  "# blockula-release $VERSION") ;;
  "# blockula-release "*)
    echo "✗ This checksums.txt is for ${tagline#\# blockula-release }, not $VERSION: an older or other release served as the one you asked for. Refusing to install." >&2
    exit 1 ;;
  *)
    echo "✗ This checksums.txt names no release (releases before v0.5.0 don't): it can't be bound to $VERSION. Refusing to install." >&2
    echo "  To install an older release, use that release's own install.sh." >&2
    exit 1 ;;
esac

# 1. Checksum.
expected=$(grep " $asset\$" "$tmp/checksums.txt" | awk '{print $1}')
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$tmp/$asset" | awk '{print $1}')
else actual=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}'); fi
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  echo "Checksum mismatch for $asset; refusing to install." >&2
  exit 1
fi

verified=""
skipped=""
provenance=""
signature=""
# 2. Build provenance, from this version's tag. A failed check is fatal; a missing or logged-out gh
#    just skips it.
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  echo "🔏 Checking build provenance…"
  if gh attestation verify "$tmp/$asset" --repo "$REPO" --signer-workflow "$REPO/.github/workflows/release.yml" --source-ref "refs/tags/$VERSION" >/dev/null; then
    provenance=1
    verified="build provenance (GitHub attestation, $REPO release workflow, tag $VERSION)"
  else
    echo "✗ Build provenance check FAILED for $asset (not built by the release workflow from tag $VERSION); refusing to install." >&2
    echo "  (Releases up to v0.4.x predate provenance: they have no attestation to check. A patch distributed privately" >&2
    echo "  has none either: install it with blockula release install $VERSION --from <dir>, which checks its signature.)" >&2
    exit 1
  fi
else
  skipped="build provenance (needs a logged-in GitHub CLI: gh auth login)"
fi

# 3. Release signature, against the key id (never one taken from the download's files), verified
#    by a blockula you already have (never by the download itself). Required once a key id is known.
unchecked_signature=""
if [ -n "$RELEASE_KEY_ID" ]; then
  key_short=$(printf %.16s "$RELEASE_KEY_ID")
  if ! curl -fsSL "$base/checksums.txt.sig" -o "$tmp/checksums.txt.sig" 2>/dev/null; then
    echo "✗ This release has no checksums.txt.sig, but releases are signed offline with the release key ${key_short}…; refusing to install." >&2
    echo "  An unsigned release is what a compromised GitHub account or release workflow would publish." >&2
    exit 1
  fi
  verifier=""
  for b in "$DIR/blockula" "$(command -v blockula 2>/dev/null || true)"; do
    if [ -n "$b" ] && [ -x "$b" ] && "$b" release verify --help >/dev/null 2>&1; then verifier=$b; break; fi
  done
  if [ -n "$verifier" ]; then
    echo "🔏 Checking the release signature…"
    if "$verifier" release verify --key-id "$RELEASE_KEY_ID" --sig "$tmp/checksums.txt.sig" "$tmp/checksums.txt" >/dev/null; then
      signature=1
      verified="${verified:+$verified; }release signature (ML-DSA-65 key ${key_short}…, $key_source)"
    else
      echo "✗ checksums.txt signature check FAILED; refusing to install." >&2
      exit 1
    fi
  elif [ -n "$GIVEN_KEY_ID" ] || [ "${BLOCKULA_REQUIRE_VERIFY:-}" = 1 ]; then
    # You asked for the signature to be checked, and nothing here can check it.
    echo "✗ There's no installed blockula with \`blockula release verify\`, so this script can't check the release signature; refusing to install." >&2
    echo "  Install with a blockula you trust instead (blockula release install $VERSION --key-id <id>), or follow" >&2
    echo "  docs/OPERATIONS.md \"A first install\", which never runs this script." >&2
    exit 1
  else
    unchecked_signature=1
    skipped="${skipped:+$skipped; }release signature (needs an installed blockula with \`blockula release verify\`)"
  fi
fi

if [ -n "$verified" ]; then echo "✓ Verified: $verified"; fi
if [ -n "$skipped" ]; then echo "⚠️  Not checked: $skipped" >&2; fi
if [ -z "$verified" ]; then
  cat >&2 <<EOF

  ⚠️  ONLY THE CHECKSUM WAS VERIFIED.
  That proves the download wasn't corrupted, not that this release is genuine: anyone who can
  change the release can change checksums.txt too. To verify it, either
    - install the GitHub CLI and log in (gh auth login), then re-run this script, or
    - check the files by hand: docs/OPERATIONS.md "Releases" (provenance, signature, rebuild).
  Set BLOCKULA_REQUIRE_VERIFY=1 to make this an error. Don't run a validator on an unverified binary.

EOF
elif [ -n "$unchecked_signature" ]; then
  cat >&2 <<EOF

  ⚠️  THE RELEASE SIGNATURE WAS NOT CHECKED (no installed blockula can verify it yet).
  Provenance alone is what a compromised GitHub account or release workflow could produce too.
  Now that blockula is installed, \`blockula release install\` checks the signature of the next one. A
  validator doesn't install this way: docs/OPERATIONS.md "A first install".

EOF
fi
if [ "${BLOCKULA_REQUIRE_VERIFY:-}" = 1 ]; then
  # Provenance always; once a release key id is known, the signature too: provenance alone is all a
  # compromised GitHub account or release workflow can produce.
  if [ -z "$provenance" ]; then
    echo "BLOCKULA_REQUIRE_VERIFY=1: build provenance wasn't checked (log in to the GitHub CLI: gh auth login); refusing to install." >&2
    exit 1
  fi
  if [ -n "$RELEASE_KEY_ID" ] && [ -z "$signature" ]; then
    echo "BLOCKULA_REQUIRE_VERIFY=1: the release signature wasn't checked; refusing to install." >&2
    exit 1
  fi
fi

tar -xzf "$tmp/$asset" -C "$tmp"
# The binary must be the version you asked for, before it replaces anything.
got=$("$tmp/blockula-$target/blockula" --version 2>/dev/null || true)
if [ "$got" != "blockula ${VERSION#v}" ]; then
  echo "✗ The downloaded binary is ${got:-unrunnable here}, not ${VERSION#v}; refusing to install." >&2
  exit 1
fi
mkdir -p "$DIR"
install -m 755 "$tmp/blockula-$target/blockula" "$DIR/blockula"
echo "✓ Installed $("$DIR/blockula" --version) to $DIR/blockula"

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) echo "  Add it to your PATH:  echo 'export PATH=\"$DIR:\$PATH\"' >> ~/.zshrc  (or ~/.bashrc)" ;;
esac
echo "  Try:  blockula dev        (local chain + explorer at http://localhost:7070)"
echo "        blockula wallet new && blockula wallet faucet --rpc https://blockula-testnet.fly.dev"
