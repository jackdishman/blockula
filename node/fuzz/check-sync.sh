#!/usr/bin/env bash
# src/lib.rs compiles the node's modules from ../src, so this crate needs the node's module list
# and its exact [dependencies]. Fails, with the difference, when either drifts from node/.
set -euo pipefail
cd "$(dirname "$0")"
# Path dependencies are relative to each manifest: this crate's `../ghost` is the node's `ghost`.
deps() { awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f' "$1" | grep -v -e '^[[:space:]]*#' -e '^[[:space:]]*$' -e '^libfuzzer-sys' | sed 's#path = "\.\./#path = "#' | sort; }
# Non-test modules declared in main.rs (a `#[cfg(test)]` line skips the next declaration).
node_mods() { awk '/^#\[cfg\(test\)\]$/{skip=1; next} /^(pub )?mod [a-z_0-9]+;/{ if (!skip) { sub(/^(pub )?mod /, ""); sub(/;.*/, ""); print } } {skip=0}' ../src/main.rs | sort; }
fuzz_mods() { sed -n 's/^    pub mod \([a-z_0-9]*\);$/\1/p' src/lib.rs | sort; }
ok=1
diff -u <(deps ../Cargo.toml) <(deps Cargo.toml) || { echo "node/fuzz/Cargo.toml [dependencies] must match node/Cargo.toml's" >&2; ok=0; }
diff -u <(node_mods) <(fuzz_mods) || { echo "node/fuzz/src/lib.rs must declare the modules node/src/main.rs declares" >&2; ok=0; }
[ "$ok" = 1 ] && echo "node/fuzz is in sync with node/ (modules and dependencies)"
