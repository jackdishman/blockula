#!/usr/bin/env bash
# Checks this vendored commonware-stream against the crates.io release it came from: verifies the
# release tarball's checksum, fails if any upstream file other than the three Blockula patches
# (Cargo.toml, src/lib.rs, src/encrypted.rs) differs, and prints the full diff for review.
#
#   bash node/vendor/commonware-stream/verify-vendor.sh
set -euo pipefail
cd "$(dirname "$0")"

VERSION=2026.9.0
SHA256=e4448b7e1d6a0d7a3fc9521be6ddebba0e3bde321da63e414233b6c53bd39e77
PATCHED=("Cargo.toml" "src/encrypted.rs" "src/lib.rs")
REMOVED=("Cargo.lock")

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
crate="$tmp/commonware-stream-$VERSION.crate"
cached=$(ls "${CARGO_HOME:-$HOME/.cargo}"/registry/cache/*/commonware-stream-$VERSION.crate 2>/dev/null | head -1 || true)
if [[ -n "$cached" ]]; then
  cp "$cached" "$crate"
else
  curl -sSfL -o "$crate" "https://static.crates.io/crates/commonware-stream/commonware-stream-$VERSION.crate"
fi
echo "$SHA256  $crate" | shasum -a 256 -c - >/dev/null
echo "✓ crates.io commonware-stream $VERSION matches sha256 $SHA256"
tar -xzf "$crate" -C "$tmp"
upstream="$tmp/commonware-stream-$VERSION"

status=0
while IFS= read -r -d '' f; do
  rel=${f#"$upstream"/}
  if [[ " ${REMOVED[*]} " == *" $rel "* ]]; then
    [[ ! -e "$rel" ]] || { echo "❌ $rel should have been removed"; status=1; }
  elif [[ ! -e "$rel" ]]; then
    echo "❌ upstream file $rel is missing"; status=1
  elif ! cmp -s "$f" "$rel" && [[ " ${PATCHED[*]} " != *" $rel "* ]]; then
    echo "❌ upstream file $rel was changed but isn't a declared patch"; status=1
  fi
done < <(find "$upstream" -type f -print0)
[[ $status -eq 0 ]] && echo "✓ only ${PATCHED[*]} differ from upstream"

echo
echo "— Blockula changes to upstream files"
for rel in "${PATCHED[@]}"; do
  diff -u --label "upstream/$rel" --label "vendored/$rel" "$upstream/$rel" "$rel" || true
done
echo
echo "— Files added by Blockula"
(cd "$upstream" && find . -type f | sort) >"$tmp/upstream.txt"
find . -type f -not -path './target/*' | sort | comm -13 "$tmp/upstream.txt" -
exit $status
