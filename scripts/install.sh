#!/bin/sh
# 🧛 Install the `blockula` node + CLI from GitHub releases.
#   curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
# Env: BLOCKULA_VERSION (e.g. v0.5.0; default latest), BLOCKULA_INSTALL_DIR (default ~/.blockula/bin),
#      BLOCKULA_REQUIRE_VERIFY=1 (refuse to install unless provenance or the release signature checks out),
#      BLOCKULA_RELEASE_URL (a mirror holding the release's files; every check below still applies).
#
# What gets checked (docs/OPERATIONS.md "Releases"):
#   1. SHA-256 against the release's checksums.txt. Always. It only catches a corrupted download:
#      whoever can change the release can change checksums.txt too.
#   2. SLSA build provenance, with a logged-in GitHub CLI (`gh attestation verify`): the archive was
#      built by this repo's release workflow.
#   3. The offline ML-DSA-65 signature on checksums.txt, with an already installed blockula that
#      has `blockula release verify` and the release key id pinned below.
# This script comes from the release too. For more than trust-on-download, verify it first:
#   gh attestation verify install.sh --repo jackdishman/blockula
set -eu

REPO=jackdishman/blockula
DIR=${BLOCKULA_INSTALL_DIR:-$HOME/.blockula/bin}
VERSION=${BLOCKULA_VERSION:-latest}
# BLAKE3 of the release public key (docs/OPERATIONS.md "Releases"). Empty until the key ceremony.
RELEASE_KEY_ID=""

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
# 2. Build provenance. A failed check is fatal; a missing or logged-out gh just skips it.
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  echo "🔏 Checking build provenance…"
  if gh attestation verify "$tmp/$asset" --repo "$REPO" --signer-workflow "$REPO/.github/workflows/release.yml" >/dev/null; then
    verified="build provenance (GitHub attestation, $REPO release workflow)"
  else
    echo "✗ Build provenance check FAILED for $asset; refusing to install." >&2
    echo "  (Releases up to v0.4.x predate provenance: they have no attestation to check.)" >&2
    exit 1
  fi
fi

# 3. Release signature, verified by a blockula you already have (never by the download itself).
if [ -n "$RELEASE_KEY_ID" ] && curl -fsSL "$base/checksums.txt.sig" -o "$tmp/checksums.txt.sig" 2>/dev/null; then
  verifier=""
  for b in "$DIR/blockula" "$(command -v blockula 2>/dev/null || true)"; do
    if [ -n "$b" ] && [ -x "$b" ] && "$b" release verify --help >/dev/null 2>&1; then verifier=$b; break; fi
  done
  if [ -n "$verifier" ]; then
    echo "🔏 Checking the release signature…"
    if "$verifier" release verify --key-id "$RELEASE_KEY_ID" --sig "$tmp/checksums.txt.sig" "$tmp/checksums.txt" >/dev/null; then
      verified="${verified:+$verified; }release signature (ML-DSA-65 key $(printf %.16s "$RELEASE_KEY_ID")…)"
    else
      echo "✗ checksums.txt signature check FAILED; refusing to install." >&2
      exit 1
    fi
  fi
fi

if [ -n "$verified" ]; then
  echo "✓ Verified: $verified"
else
  cat >&2 <<EOF

  ⚠️  ONLY THE CHECKSUM WAS VERIFIED.
  That proves the download wasn't corrupted, not that this release is genuine: anyone who can
  change the release can change checksums.txt too. To verify it, either
    - install the GitHub CLI and log in (gh auth login), then re-run this script, or
    - check the files by hand: docs/OPERATIONS.md "Releases" (provenance, signature, rebuild).
  Set BLOCKULA_REQUIRE_VERIFY=1 to make this an error. Don't run a validator on an unverified binary.

EOF
  if [ "${BLOCKULA_REQUIRE_VERIFY:-}" = 1 ]; then
    echo "BLOCKULA_REQUIRE_VERIFY=1: refusing to install." >&2
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
