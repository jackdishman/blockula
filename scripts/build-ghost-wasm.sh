#!/usr/bin/env bash
# Builds the Ghost Mode prover for wallets (node/ghost-wasm: blockula-ghost behind wasm-bindgen)
# and writes it where @blockula/sdk/ghost loads it from: sdk-ts/src/ghost-wasm/ (the .wasm, its
# JavaScript glue and types; `npm run build -w @blockula/sdk` copies them into dist/).
#
#   scripts/build-ghost-wasm.sh           build, write the files, print their sizes
#   scripts/build-ghost-wasm.sh --check   build, and fail if the committed .wasm isn't what came out
#
# Needs: the repo's Rust toolchain with the wasm32 target (`rustup target add
# wasm32-unknown-unknown`) and wasm-bindgen-cli at the exact version node/ghost-wasm/Cargo.toml
# pins (`cargo install wasm-bindgen-cli --version <that> --locked`). No wasm-opt: what rustc
# emits is what ships, so two builds with the same toolchain give the same bytes.
set -euo pipefail
cd "$(dirname "$0")/.."

crate=node/ghost-wasm
out=sdk-ts/src/ghost-wasm
name=blockula_ghost_wasm

want=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' "$crate/Cargo.toml")
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [ "$have" != "$want" ]; then
  echo "wasm-bindgen-cli $want is needed (found: ${have:-none}): cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi
if ! rustup target list --installed | grep -qx wasm32-unknown-unknown; then
  echo "the wasm32 target is needed: rustup target add wasm32-unknown-unknown" >&2
  exit 1
fi

# No path of this machine in the binary (panic messages name source files).
cargo_home=${CARGO_HOME:-$HOME/.cargo}
export RUSTFLAGS="--remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$PWD=/blockula ${GHOST_WASM_RUSTFLAGS:-}"
cargo build --manifest-path "$crate/Cargo.toml" --release --locked --target wasm32-unknown-unknown

built=$(mktemp -d)
trap 'rm -rf "$built"' EXIT
wasm-bindgen --target web --out-dir "$built" --out-name "$name" "$crate/target/wasm32-unknown-unknown/release/$name.wasm"

if [ "${1:-}" = "--check" ]; then
  for f in "${name}_bg.wasm" "$name.js" "$name.d.ts"; do
    cmp -s "$built/$f" "$out/$f" || { echo "$out/$f isn't what this build makes: run scripts/build-ghost-wasm.sh and commit the result" >&2; exit 1; }
  done
  echo "$out is current"
  exit 0
fi

mkdir -p "$out"
cp "$built/${name}_bg.wasm" "$built/$name.js" "$built/$name.d.ts" "$built/${name}_bg.wasm.d.ts" "$out/"

size() { wc -c < "$1" | tr -d ' '; }
gz() { gzip -9 -c "$1" | wc -c | tr -d ' '; }
echo "wasm:    $(size "$out/${name}_bg.wasm") bytes raw, $(gz "$out/${name}_bg.wasm") gzipped"
echo "JS glue: $(size "$out/$name.js") bytes raw, $(gz "$out/$name.js") gzipped"
echo "sha256:  $(shasum -a 256 "$out/${name}_bg.wasm" | awk '{print $1}')"
