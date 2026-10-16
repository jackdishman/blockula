#!/bin/sh
# 🧛 Install the `blockula` node + CLI from GitHub releases.
#   curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
# Env: BLOCKULA_VERSION (e.g. v0.5.0; default latest), BLOCKULA_INSTALL_DIR (default ~/.blockula/bin),
#      BLOCKULA_REQUIRE_VERIFY=1 (refuse what wasn't proven genuine: build provenance, and once a
#      release key id is known, the release signature too),
#      BLOCKULA_RELEASE_KEY_ID (the release key id to trust, until one is built in below),
#      BLOCKULA_RELEASE_URL (a mirror holding the release's files; every check below still applies).
#
# What gets checked (docs/OPERATIONS.md "Releases"):
#   1. SHA-256 against the release's checksums.txt. Always. It only catches a corrupted download:
#      whoever can change the release can change checksums.txt too.
#   2. SLSA build provenance, with a logged-in GitHub CLI (`gh attestation verify`): the archive was
#      built by this repo's release workflow. A failed check is fatal; no gh skips it.
#   3. The offline ML-DSA-65 signature on checksums.txt (checksums.txt.sig) against the release key
#      id, with an already installed blockula that has `blockula release verify`. Once a key id is
#      known, every release must carry a valid checksums.txt.sig: a missing or invalid one is fatal.
#      Leaving it out is exactly what a compromised GitHub account or release workflow would do,
#      and it can't forge the offline signature (audit F49). With no installed verifier the
#      signature can't be checked: the script says so, and BLOCKULA_REQUIRE_VERIFY=1 refuses.
# The checks add up (each refuses a release it fails), and the script prints which ran.
# This script comes from the release too. For more than trust-on-download, verify it first:
#   gh attestation verify install.sh --repo jackdishman/blockula
set -eu

REPO=jackdishman/blockula
DIR=${BLOCKULA_INSTALL_DIR:-$HOME/.blockula/bin}
VERSION=${BLOCKULA_VERSION:-latest}
# BLAKE3 of the release public key (docs/OPERATIONS.md "Releases"). Empty until the key ceremony;
# BLOCKULA_RELEASE_KEY_ID sets it meanwhile (as for the MCP's install_node).
RELEASE_KEY_ID=""
RELEASE_KEY_ID=$(printf %s "${BLOCKULA_RELEASE_KEY_ID:-$RELEASE_KEY_ID}" | tr 'A-F' 'a-f')
case "$RELEASE_KEY_ID" in
  "") ;;
  *[!0-9a-f]*) echo "The release key id must be 64 hex characters." >&2; exit 1 ;;
  *) [ ${#RELEASE_KEY_ID} -eq 64 ] || { echo "The release key id must be 64 hex characters." >&2; exit 1; } ;;
esac

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  *) echo "Sorry, no prebuilt blockula for $(uname -s) $(uname -m) yet. Build from source: cargo build --release --locked --manifest-path node/Cargo.toml" >&2; exit 1 ;;
esac

if [ -n "${BLOCKULA_RELEASE_URL:-}" ]; then base=$BLOCKULA_RELEASE_URL
elif [ "$VERSION" = latest ]; then base=https://github.com/$REPO/releases/latest/download
else base=https://github.com/$REPO/releases/download/$VERSION; fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
asset=blockula-$target.tar.gz
echo "🧛 Downloading ${asset}…"
curl -fsSL "$base/$asset" -o "$tmp/$asset"
curl -fsSL "$base/checksums.txt" -o "$tmp/checksums.txt"

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
# 2. Build provenance. A failed check is fatal; a missing or logged-out gh just skips it.
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  echo "🔏 Checking build provenance…"
  if gh attestation verify "$tmp/$asset" --repo "$REPO" --signer-workflow "$REPO/.github/workflows/release.yml" >/dev/null; then
    provenance=1
    verified="build provenance (GitHub attestation, $REPO release workflow)"
  else
    echo "✗ Build provenance check FAILED for $asset; refusing to install." >&2
    echo "  (Releases up to v0.4.x predate provenance: they have no attestation to check.)" >&2
    exit 1
  fi
else
  skipped="build provenance (needs a logged-in GitHub CLI: gh auth login)"
fi

# 3. Release signature, against the pinned key id (never one taken from the download), verified
#    by a blockula you already have (never by the download itself). Required once a key id is known.
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
      verified="${verified:+$verified; }release signature (ML-DSA-65 key ${key_short}…)"
    else
      echo "✗ checksums.txt signature check FAILED; refusing to install." >&2
      exit 1
    fi
  else
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
    echo "  Install a blockula that has \`blockula release verify\` first (the previous release, or one built" >&2
    echo "  from source) and re-run, or verify checksums.txt.sig by hand (docs/OPERATIONS.md \"Releases\")." >&2
    exit 1
  fi
fi

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$DIR"
install -m 755 "$tmp/blockula-$target/blockula" "$DIR/blockula"
echo "✓ Installed $("$DIR/blockula" --version) to $DIR/blockula"

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) echo "  Add it to your PATH:  echo 'export PATH=\"$DIR:\$PATH\"' >> ~/.zshrc  (or ~/.bashrc)" ;;
esac
echo "  Try:  blockula dev        (local chain + explorer at http://localhost:7070)"
echo "        blockula wallet new && blockula wallet faucet --rpc https://blockula-testnet.fly.dev"
