#!/usr/bin/env bash
# Builds one release archive, reproducibly: the same commit, toolchain (rust-toolchain.toml) and
# build image give byte-identical output on any machine and in any directory. The release workflow
# runs exactly this; so does scripts/reproduce-release.sh for anyone checking a release.
#   BLOCKULA_WALLET=<wallet archive> scripts/release-build.sh <target> [out-dir]
#     →  out-dir/blockula-<target>.tar.gz (+ .sha256); paths are absolute or from the checkout
# Linux targets are built in the pinned rust image (RELEASE_IMAGE in reproduce-release.sh); macOS
# targets need the same Xcode as the release run (printed in its log).
set -euo pipefail
target=${1:?usage: scripts/release-build.sh <target> [out-dir]}
out=${2:-dist}
cd "$(dirname "$0")/.."
root=$PWD
mkdir -p "$out"
out=$(cd "$out" && pwd)
cargo_home=${CARGO_HOME:-$HOME/.cargo}
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

tar=tar
command -v gtar >/dev/null 2>&1 && tar=gtar
"$tar" --version 2>/dev/null | grep -q 'GNU tar' || { echo "GNU tar is required (macOS: brew install gnu-tar)" >&2; exit 1; }

# Everything that could leak the build machine into the output is pinned or remapped.
git() { command git -c safe.directory="$root" "$@"; }   # containers check out as another uid
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
# build.rs embeds `git rev-parse --short=12 HEAD` when git works there; this is the same string.
export BLOCKULA_GIT_SHA=${BLOCKULA_GIT_SHA:-$(git rev-parse HEAD | cut -c1-12)}
export CARGO_INCREMENTAL=0
export RUSTFLAGS="--remap-path-prefix=$root=/blockula --remap-path-prefix=$cargo_home=/cargo"
# C dependencies (aws-lc, zstd, blake3) embed __FILE__ paths.
export CFLAGS="-ffile-prefix-map=$root=/blockula -ffile-prefix-map=$cargo_home=/cargo"
export CXXFLAGS=$CFLAGS
case $target in *-apple-darwin) export MACOSX_DEPLOYMENT_TARGET=11.0 ;; esac
unset RUSTC_WRAPPER CARGO_BUILD_RUSTFLAGS

{
  echo "release-build: $target at $(git rev-parse HEAD), SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH"
  rustc -vV
  cc --version 2>/dev/null | head -1 || true
  if [ "$(uname -s)" = Darwin ]; then xcodebuild -version 2>/dev/null | tr '\n' ' '; echo; fi
} >&2

# The wallet nodes serve at /wallet/ (scripts/release-wallet.sh's archive), built into the binary.
# A release always has it; BLOCKULA_WALLET=none builds one without, for a test.
wallet=${BLOCKULA_WALLET:?BLOCKULA_WALLET=<blockula-wallet.tar.gz from scripts/release-wallet.sh> (or none)}
if [ "$wallet" = none ]; then
  [ ! -e web/out ] || { echo "web/out exists: remove it, or pass its archive as BLOCKULA_WALLET" >&2; exit 1; }
  unset BLOCKULA_WALLET_DIR
else
  wallet_dir=$root/node/target/release-wallet
  rm -rf "$wallet_dir" && mkdir -p "$wallet_dir"
  "$tar" -xzf "$wallet" -C "$wallet_dir" --no-same-owner
  export BLOCKULA_WALLET_DIR=$wallet_dir/blockula-wallet
  echo "wallet: $(sha "$wallet")" >&2
fi

cargo build --release --locked --manifest-path node/Cargo.toml --target "$target"

# A deterministic archive: sorted entries, fixed mtime/owner/modes, no gzip name or timestamp.
name=blockula-$target
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -m 755 "$stage/$name"
install -m 755 "node/target/$target/release/blockula" "$stage/$name/blockula"
install -m 644 LICENSE README.md "$stage/$name/"
"$tar" --sort=name --format=gnu --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner \
  -C "$stage" -cf - "$name" | gzip -9 -n > "$out/$name.tar.gz"

(cd "$out" && sha "$name.tar.gz" > "$name.tar.gz.sha256" && cat "$name.tar.gz.sha256")
echo "binary: $(cd "$stage/$name" && sha blockula)" >&2
