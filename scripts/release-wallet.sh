#!/usr/bin/env bash
# Builds the wallet every node serves at /wallet/ (the static export of web/, `npm run
# build:wallet`) into one archive, reproducibly: the same commit, lockfile and Node.js image give
# the same bytes. The release workflow runs this once and builds every target's binary with it
# (scripts/release-build.sh, BLOCKULA_WALLET); scripts/reproduce-release.sh runs it again.
#   scripts/release-wallet.sh [out-dir]   →  out-dir/blockula-wallet.tar.gz (+ .sha256)
# Release builds run it in the pinned Node.js image (WALLET_IMAGE in reproduce-release.sh).
set -euo pipefail
out=${1:-dist}
cd "$(dirname "$0")/.."
root=$PWD
mkdir -p "$out"
out=$(cd "$out" && pwd)

tar=tar
command -v gtar >/dev/null 2>&1 && tar=gtar
"$tar" --version 2>/dev/null | grep -q 'GNU tar' || { echo "GNU tar is required (macOS: brew install gnu-tar)" >&2; exit 1; }

git() { command git -c safe.directory="$root" "$@"; }
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
# The export's build id (web/next.config.ts): the commit's, as the binary's version shows it.
export BLOCKULA_GIT_SHA=${BLOCKULA_GIT_SHA:-$(git rev-parse HEAD | cut -c1-12)}
export NEXT_TELEMETRY_DISABLED=1
# Nothing from the environment that the build reads: the served wallet takes its node, chain and
# passkey domain from the page's location.
unset NEXT_PUBLIC_NETWORK NEXT_PUBLIC_RP_ID NEXT_PUBLIC_TESTNET_RPC NEXT_PUBLIC_LOCAL_RPC NEXT_PUBLIC_MAINNET_RPC

echo "release-wallet: $(git rev-parse HEAD), node $(node --version), npm $(npm --version)" >&2
rm -rf web/out
npm ci --no-audit --no-fund
npm run build:wallet

name=blockula-wallet
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -R web/out "$stage/$name"
chmod -R u=rwX,go=rX "$stage/$name"
"$tar" --sort=name --format=gnu --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner \
  -C "$stage" -cf - "$name" | gzip -9 -n > "$out/$name.tar.gz"

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
(cd "$out" && sha "$name.tar.gz" > "$name.tar.gz.sha256" && cat "$name.tar.gz.sha256")
