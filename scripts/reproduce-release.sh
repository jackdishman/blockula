#!/usr/bin/env bash
# Rebuilds a release's Linux archive from source in the pinned build image and compares it with
# the published checksums.txt. Independent builders run this before a release is signed
# (docs/OPERATIONS.md "Releases"). Needs git, curl and docker.
#   COMMIT=<sha> scripts/reproduce-release.sh v0.5.0          target = this machine's arch
#   COMMIT=<sha> scripts/reproduce-release.sh v0.5.0 aarch64-unknown-linux-gnu
# Env: COMMIT=<the full commit SHA you reviewed> (builders and the signer always set it): the
#      tag must be that commit, and then scripts/check-release-files.sh also checks checksums.txt's
#      other lines (install.sh, genesis and checkpoint files) against it. A moved tag rebuilds and
#      matches from its own source, so without COMMIT this only proves the archive is the tag's.
#      CHECKSUMS=<file> (e.g. from a draft release: gh release download v0.5.0 -p checksums.txt),
#      SRC=<checkout> (build its committed HEAD instead of cloning the tag), BLOCKULA_REPO (default
#      jackdishman/blockula).
set -euo pipefail
# The image release.yml builds Linux targets in. Its rust version must match rust-toolchain.toml
# (scripts/check-pins.sh enforces both).
RELEASE_IMAGE=rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e

tag=${1:?usage: scripts/reproduce-release.sh <tag> [target]}
case "$(uname -m)" in arm64 | aarch64) arch=aarch64 ;; *) arch=x86_64 ;; esac
target=${2:-$arch-unknown-linux-gnu}
case $target in
  x86_64-unknown-linux-gnu) platform=linux/amd64 ;;
  aarch64-unknown-linux-gnu) platform=linux/arm64 ;;
  *) echo "Only Linux targets are reproduced in docker; build macOS targets on macOS with scripts/release-build.sh" >&2; exit 1 ;;
esac
repo=${BLOCKULA_REPO:-jackdishman/blockula}
asset=blockula-$target.tar.gz

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
src=$work/src
commit=${COMMIT:-}
[ -z "$commit" ] || [[ $commit =~ ^[0-9a-f]{40}$ ]] || { echo "COMMIT must be a full commit SHA (40 hex), got '$commit'" >&2; exit 1; }
if [ -n "${SRC:-}" ]; then git clone --quiet "$SRC" "$src"
else git clone --quiet --depth 1 --branch "$tag" "https://github.com/$repo.git" "$src"; fi
built=$(git -C "$src" rev-parse HEAD)
if [ -n "$commit" ] && [ "$built" != "$commit" ]; then
  echo "✗ $tag is $built here, not the commit you reviewed ($commit). Don't sign or install it: find out who moved the tag." >&2
  exit 1
fi
echo "🔨 Building $asset from $built in $RELEASE_IMAGE ($platform)…"
# A fresh clone inside a throwaway container, at a path unlike CI's: the build remaps paths, so
# the output must not change. Only committed files are built.
mkdir -p "$work/dist"
docker run --rm --platform "$platform" -v "$src:/src:ro" -v "$work/dist:/out" "$RELEASE_IMAGE" bash -euc "
  git config --global --add safe.directory '*'
  git clone -q /src /work/blockula
  cd /work/blockula
  rustup toolchain install >/dev/null
  scripts/release-build.sh $target /out
  chown -R $(id -u):$(id -g) /out"

if [ -n "${CHECKSUMS:-}" ]; then cp "$CHECKSUMS" "$work/checksums.txt"
else curl -fsSL "https://github.com/$repo/releases/download/$tag/checksums.txt" -o "$work/checksums.txt"; fi
expected=$(awk -v a="$asset" '$2 == a {print $1}' "$work/checksums.txt")
actual=$(awk '{print $1}' "$work/dist/$asset.sha256")
echo "published: ${expected:-<missing>}"
echo "rebuilt:   $actual"
if [ "$expected" = "$actual" ]; then
  echo "✓ $asset reproduces bit for bit"
  echo "  For the signer's REPRODUCED file (send it on the operators' channel, not only the release issue):"
  echo "  $actual  $asset"
else
  echo "✗ $asset does NOT match the release. Don't sign or install it; report the diff (diffoscope helps)." >&2
  exit 1
fi
if [ -n "$commit" ]; then
  # The other lines of checksums.txt: the tag's own install.sh, genesis and checkpoint files.
  SRC=$src COMMIT=$commit "$(dirname "$0")/check-release-files.sh" "$tag" "$work/checksums.txt"
else
  echo "⚠️  Built from $built: compare it with the commit you reviewed. COMMIT=<sha> makes this script check it, and checksums.txt's other files (scripts/check-release-files.sh)." >&2
fi
