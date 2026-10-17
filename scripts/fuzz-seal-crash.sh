#!/usr/bin/env bash
# Seals a fuzz crash for the maintainers (audit supply-chain-B-1). The repository is public, and so
# are its workflow logs and artifacts, while a crash is an undisclosed vulnerability until it's
# fixed. This packs a crash's material (the crash files and the fuzzer's log, gathered in one
# directory), encrypts it to the maintainers' age recipients, and deletes the plaintext whatever
# happens. It prints nothing about the crash.
#   RECIPIENTS="age1… age1…" scripts/fuzz-seal-crash.sh <target> <material dir> <out dir>
#     → <out dir>/fuzz-crash-<target>.tar.age, decrypted with `age -d -i <identity>` (node/fuzz/README.md)
# RECIPIENTS: age X25519 recipients (age1…), separated by spaces, commas or newlines; in CI the
# repository variable FUZZ_CRASH_AGE_RECIPIENTS. Without any it keeps nothing and exits 0: the run
# still fails, and a maintainer reproduces it privately. Writes sealed=1|0 to $GITHUB_OUTPUT if set.
set -euo pipefail
usage="usage: RECIPIENTS=\"age1…\" scripts/fuzz-seal-crash.sh <target> <material dir> <out dir>"
[ $# -eq 3 ] || { echo "$usage" >&2; exit 2; }
target=$1 material=$2 out=$3
[[ $target =~ ^[a-z0-9_]+$ ]] || { echo "Not a fuzz target name: $target" >&2; exit 2; }
[ -d "$material" ] || { echo "$material isn't a directory" >&2; exit 2; }
material=$(cd "$material" && pwd -P)
case $material in / | "$HOME" | "$(pwd -P)") echo "Refusing to seal (and delete) $material" >&2; exit 2 ;; esac

sealed=""
done_ok=""
# The plaintext goes whatever happens, and so does a half-written ciphertext.
cleanup() {
  rm -rf -- "$material"
  if [ -z "$done_ok" ] && [ -n "$sealed" ]; then rm -f -- "$sealed"; fi
  if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "sealed=${done_ok:-0}" >> "$GITHUB_OUTPUT"; fi
}
trap cleanup EXIT

read -r -a recipients <<< "$(printf '%s' "${RECIPIENTS:-}" | tr ',\n\t' '   ')"
if [ ${#recipients[@]} -eq 0 ]; then
  echo "No age recipients (repository variable FUZZ_CRASH_AGE_RECIPIENTS), so the crash material was deleted and nothing is uploaded."
  echo "Set it to keep the next one (deploy/github/README.md); meanwhile fuzz $target privately to find it again (node/fuzz/README.md, \"Findings\")."
  exit 0
fi
args=()
for r in "${recipients[@]}"; do
  # bech32: "age1", then 58 characters of its alphabet (no 1, b, i, o).
  if ! [[ $r =~ ^age1[02-9ac-hj-np-z]{58}$ ]]; then
    echo "FUZZ_CRASH_AGE_RECIPIENTS holds something that isn't an age X25519 recipient (age1…); nothing was kept." >&2
    exit 1
  fi
  args+=(-r "$r")
done
command -v age > /dev/null || { echo "age isn't installed; nothing was kept." >&2; exit 1; }

mkdir -p "$out"
sealed=$(cd "$out" && pwd -P)/fuzz-crash-$target.tar.age
tar -C "$material" -cf - . 2> /dev/null | age "${args[@]}" -o "$sealed"
done_ok=1
echo "✓ Sealed the $target crash for ${#recipients[@]} maintainer key(s): $(basename "$sealed")"
