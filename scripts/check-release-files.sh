#!/usr/bin/env bash
# The signer's check of what a release's checksums.txt lists, against the source of the commit the
# signer reviewed, before signing it (docs/OPERATIONS.md "How a release is made", audit F49
# round 3). The offline signature vouches for every line of checksums.txt, and operators trust
# the files because of it, so each line must be something the signer actually checked:
#   blockula-<target>.tar.gz   one per release target. The Linux ones are rebuilt bit for bit by
#                              scripts/reproduce-release.sh (two builders); the macOS ones only
#                              with the same Xcode, otherwise they're signed on provenance.
#   install.sh                 the tag's scripts/install.sh, byte for byte
#   genesis-<id>.json          the tag's genesis/<id>/genesis.json
#   checkpoints-<id>.json      the tag's node/checkpoints/<id>.json
# and every one of those files the tag has must be listed. Anything else (an unknown file, a
# missing one, a duplicate, a malformed line, another release's tag line) is refused, and so is a
# tag that doesn't point at COMMIT: a tag moved to another commit would rebuild and match from its
# own source, so only the commit you reviewed says the source is the one you meant to sign.
#   COMMIT=<full sha you reviewed> scripts/check-release-files.sh v0.5.0 /tmp/rel/checksums.txt
# Env: COMMIT (required), SRC=<checkout that has the tag> instead of a fresh clone,
#      BLOCKULA_REPO (default jackdishman/blockula). Needs git.
set -euo pipefail
usage="usage: COMMIT=<reviewed commit> scripts/check-release-files.sh <vX.Y.Z> <checksums.txt>"
tag=${1:?$usage}
sums=${2:?$usage}
die() { echo "✗ $*" >&2; exit 1; }
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "a release tag is vX.Y.Z, got '$tag'"
[ -f "$sums" ] || die "$sums isn't a file"
commit=${COMMIT:-}
[[ $commit =~ ^[0-9a-f]{40}$ ]] || die "set COMMIT to the full commit SHA (40 hex) you reviewed for $tag: the tag must point at it"
targets=(aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if [ -n "${SRC:-}" ]; then
  src=$SRC
else
  src=$work/src
  git clone --quiet --bare --depth 1 --branch "$tag" "https://github.com/${BLOCKULA_REPO:-jackdishman/blockula}.git" "$src"
fi
tagged=$(git -C "$src" rev-parse --verify --quiet "refs/tags/$tag^{commit}") || die "$src has no tag $tag"
[ "$tagged" = "$commit" ] || die "tag $tag points at $tagged, not the commit you reviewed ($commit). Don't sign: find out who moved it"

sha() { if command -v sha256sum > /dev/null; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi; }
# The hash of PATH at the commit, or nothing if it isn't there.
at_commit() { git -C "$src" cat-file -e "$commit:$1" 2> /dev/null && git -C "$src" show "$commit:$1" | sha; }

errors=0
bad() { echo "✗ $*" >&2; errors=$((errors + 1)); }
first=$(head -n 1 "$sums")
[ "$first" = "# blockula-release $tag" ] || bad "the first line is '$first', not '# blockula-release $tag'"

# name<TAB>hash of each line read so far (bash 3 has no associative arrays).
listed=$work/listed
: > "$listed"
hash_of() { awk -F '\t' -v n="$1" '$1 == n {print $2}' "$listed"; }
n=1
while IFS= read -r line || [ -n "$line" ]; do
  n=$((n + 1))
  if ! [[ $line =~ ^([0-9a-f]{64})\ \ ([A-Za-z0-9._+-]+)$ ]] || [[ ${BASH_REMATCH[2]} == .* ]]; then
    bad "line $n isn't '<sha256>  <file>': '$line'"
    continue
  fi
  hash=${BASH_REMATCH[1]}
  name=${BASH_REMATCH[2]}
  [ -z "$(hash_of "$name")" ] || { bad "$name is listed twice"; continue; }
  printf '%s\t%s\n' "$name" "$hash" >> "$listed"
  case $name in
    blockula-*.tar.gz)
      t=${name#blockula-}; t=${t%.tar.gz}
      known=0
      for k in "${targets[@]}"; do [ "$k" = "$t" ] && known=1; done
      [ $known = 1 ] || bad "$name isn't a release target's archive"
      ;;
    install.sh) path=scripts/install.sh ;;
    genesis-*.json) path=genesis/${name#genesis-}; path=${path%.json}/genesis.json ;;
    checkpoints-*.json) path=node/checkpoints/${name#checkpoints-} ;;
    *) bad "$name isn't a file a release carries (archives, install.sh, genesis-<id>.json, checkpoints-<id>.json)" ;;
  esac
  case $name in
    install.sh | genesis-*.json | checkpoints-*.json)
      want=$(at_commit "$path" || true)
      if [ -z "$want" ]; then bad "$name: $tag has no $path to check it against"
      elif [ "$want" != "$hash" ]; then bad "$name doesn't match $tag's $path (listed $hash, the source's is $want)"
      else echo "✓ $name is $tag's $path"; fi
      ;;
  esac
done < <(tail -n +2 "$sums")

# Everything the tag would publish is there.
for t in "${targets[@]}"; do [ -n "$(hash_of "blockula-$t.tar.gz")" ] || bad "blockula-$t.tar.gz isn't listed"; done
[ -n "$(hash_of install.sh)" ] || bad "install.sh isn't listed"
while IFS= read -r path; do
  case $path in
    genesis/*/genesis.json) name=genesis-$(basename "$(dirname "$path")").json ;;
    node/checkpoints/*.json) name=checkpoints-$(basename "$path") ;;
    *) continue ;;
  esac
  [ -n "$(hash_of "$name")" ] || bad "$tag has $path, but $name isn't listed"
done < <(git -C "$src" ls-tree -r --name-only "$commit" -- genesis node/checkpoints)

if [ $errors -gt 0 ]; then
  echo "✗ $sums: $errors problem(s). Don't sign it." >&2
  exit 1
fi
echo "✓ $sums: $tag is $commit; install.sh, the genesis and checkpoint files are the tag's own; ${#targets[@]} archives listed (rebuild the Linux ones with scripts/reproduce-release.sh)"
