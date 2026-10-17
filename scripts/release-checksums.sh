#!/usr/bin/env bash
# Writes a release's checksums.txt: a first line naming the release, "# blockula-release vX.Y.Z",
# then the SHA-256 of every file in DIR, sorted by name. release.yml runs it; so does a maintainer
# for a patch release built outside it (docs/OPERATIONS.md "Patch distribution"). The offline
# signature covers the whole file, so a signed checksums.txt can't be passed off as another
# version's: install.sh, the MCP's install_node and `blockula release verify --tag` check the line
# (audit supply-chain-B-2).
#   scripts/release-checksums.sh v0.5.0 release      → release/checksums.txt
set -euo pipefail
usage="usage: scripts/release-checksums.sh <vX.Y.Z> <dir>"
tag=${1:?$usage}
dir=${2:?$usage}
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "A release tag is vX.Y.Z, got '$tag'." >&2; exit 1; }
[ -d "$dir" ] || { echo "$dir isn't a directory." >&2; exit 1; }
[ ! -e "$dir/checksums.txt" ] || { echo "$dir/checksums.txt exists already." >&2; exit 1; }
out=$(mktemp)
trap 'rm -f "$out"' EXIT
(
  cd "$dir"
  shopt -s nullglob
  files=(*)
  [ ${#files[@]} -gt 0 ] || { echo "$dir is empty." >&2; exit 1; }
  echo "# blockula-release $tag"
  if command -v sha256sum >/dev/null; then sha256sum -- "${files[@]}"; else shasum -a 256 -- "${files[@]}"; fi | LC_ALL=C sort -k2
) > "$out"
install -m 644 "$out" "$dir/checksums.txt"
cat "$dir/checksums.txt"
