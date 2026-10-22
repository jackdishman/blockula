#!/usr/bin/env bash
# Builds the guest programs from scratch and copies them to ../tests/programs/, where the
# tests load them: committed, so node/vm's tests never need the wasm target. Prints each module's
# size and sha256 and records them in SHA256SUMS.
#
#   ./build.sh           build, copy, print
#   ./build.sh --check   build a copy of these sources in another directory, from scratch, and
#                        fail unless every module matches the committed one byte for byte
#
# Reproducible given the same rustc (rust-toolchain.toml pins it): no wasm-opt, a fresh target
# directory every time, `--locked`, and nothing from the environment that could change the flags.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="$here/../tests/programs"
programs=(counter token escrow router)

# .cargo/config.toml is the only place the flags live: these would replace or extend them.
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS CARGO_TARGET_WASM32V1_NONE_RUSTFLAGS \
  CARGO_TARGET_WASM32V1_NONE_LINKER CARGO_BUILD_TARGET CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR \
  RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC_WRAPPER CARGO_INCREMENTAL
for v in $(compgen -e | grep -E '^CARGO_PROFILE_' || true); do unset "$v"; done

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

# Builds the workspace at $1 into a fresh target directory and leaves the modules in $2.
build() {
  local src="$1" dest="$2"
  (cd "$src" && cargo build --release --locked --target-dir "$scratch/target-$(basename "$dest")" >&2)
  mkdir -p "$dest"
  for p in "${programs[@]}"; do
    cp "$scratch/target-$(basename "$dest")/wasm32v1-none/release/$p.wasm" "$dest/$p.wasm"
  done
}

(cd "$here" && rustc --version >&2)

if [[ "${1:-}" == "--check" ]]; then
  # Another path, so a module that embedded one would differ.
  mkdir -p "$scratch/elsewhere"
  (cd "$here" && tar cf - --exclude ./target .) | (cd "$scratch/elsewhere" && tar xf -)
  build "$scratch/elsewhere" "$scratch/check"
  status=0
  for p in "${programs[@]}"; do
    if cmp -s "$scratch/check/$p.wasm" "$out/$p.wasm"; then
      echo "same     $p.wasm"
    else
      echo "DIFFERS  $p.wasm" >&2
      status=1
    fi
  done
  (cd "$scratch/check" && sha256 "${programs[@]/%/.wasm}")
  exit $status
fi

build "$here" "$scratch/fresh"
mkdir -p "$out"
for p in "${programs[@]}"; do cp "$scratch/fresh/$p.wasm" "$out/$p.wasm" && chmod 644 "$out/$p.wasm"; done
(cd "$out" && sha256 "${programs[@]/%/.wasm}") > "$here/SHA256SUMS"
for p in "${programs[@]}"; do
  printf '%-12s %6d bytes  %s\n' "$p.wasm" "$(wc -c < "$out/$p.wasm")" "$(grep " $p.wasm\$" "$here/SHA256SUMS" | cut -d' ' -f1)"
done
