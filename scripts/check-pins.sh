#!/usr/bin/env bash
# Supply-chain lint, run by CI (OPS-7). Fails when something can change under us without a commit:
#   - a GitHub Action referenced by tag or branch instead of a full commit SHA (+ "# vX.Y.Z")
#   - a container image without an @sha256 digest (Dockerfile, compose, workflow containers)
#   - a rust image whose version differs from rust-toolchain.toml
#   - a cargo build/test/clippy/run/install without --locked
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
bad() { echo "✗ $*" >&2; fail=1; }

shopt -s nullglob
workflows=(.github/workflows/*.yml .github/workflows/*.yaml)

# Actions: owner/repo[/path]@<40 hex> # <version>. Local (./) actions are fine.
while IFS= read -r line; do
  [ -n "$line" ] || continue
  ref=$(printf '%s' "$line" | sed -E 's/^[^:]+:[0-9]+:[[:space:]]*-?[[:space:]]*uses:[[:space:]]*//')
  case $ref in
    ./*) ;;
    docker://*@sha256:*) ;;
    *@[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*" # "*)
      sha=${ref#*@}; sha=${sha%% *}
      [[ $sha =~ ^[0-9a-f]{40}$ ]] || bad "action not pinned to a full SHA: $line" ;;
    *) bad "action not pinned to a commit SHA with a version comment: $line" ;;
  esac
done < <(grep -nE '^[[:space:]]*-?[[:space:]]*uses:' "${workflows[@]}" /dev/null | sed 's/^\([^:]*:[0-9]*\):/\1:/' || true)

# Images: every FROM / image: / container: / RELEASE_IMAGE carries a digest.
while read -r where image; do
  [ -n "${image:-}" ] || continue
  [[ $image =~ @sha256:[0-9a-f]{64}$ ]] || bad "image not pinned by digest: $where $image"
done < <( {
  grep -nE '^FROM ' deploy/Dockerfile | sed -E 's/^([0-9]+):FROM ([^ ]+).*/deploy\/Dockerfile:\1 \2/'
  grep -nE '^RELEASE_IMAGE=' scripts/reproduce-release.sh | sed -E 's/^([0-9]+):RELEASE_IMAGE=/scripts\/reproduce-release.sh:\1 /'
  grep -noE '(image|container): *"?[^"}, ]+' deploy/monitoring/docker-compose.yml "${workflows[@]}" /dev/null \
    | grep -vF "\${{" | sed -E 's/^([^:]+:[0-9]+):(image|container): *"?/\1 /'
} || true)

# Toolchain parity: every rust image is the rust-toolchain.toml version.
channel=$(sed -nE 's/^channel = "([^"]+)"/\1/p' rust-toolchain.toml)
[[ $channel =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || bad "rust-toolchain.toml must pin an exact version, got '$channel'"
while IFS= read -r hit; do
  v=$(printf '%s' "$hit" | sed -E 's/.*rust:([0-9.]+)-.*/\1/')
  [ "$v" = "$channel" ] || bad "rust image $v != rust-toolchain.toml $channel: $hit"
done < <(grep -noE 'rust:[0-9][0-9.]*-[a-z]+' deploy/Dockerfile scripts/reproduce-release.sh "${workflows[@]}" /dev/null || true)
grep -qE 'rust:[0-9]' deploy/Dockerfile || bad "deploy/Dockerfile must build FROM a versioned rust image"

# --locked on every cargo build/test/clippy/run/install in CI, scripts and the image.
while IFS= read -r hit; do
  printf '%s' "$hit" | grep -q -- '--locked' || bad "cargo without --locked: $hit"
done < <(grep -nE '(^|[^-])cargo (build|test|clippy|run|install)\b' "${workflows[@]}" scripts/*.sh deploy/*.sh deploy/Dockerfile /dev/null \
  | grep -vE '^[^:]+:[0-9]+:[[:space:]]*#' | grep -vE 'echo |Build from source' || true)

# Every exactly pinned (`=`) crate is in Dependabot's solo consensus-and-crypto group, so its bumps
# never ride along in the weekly catch-all PR (audit F78).
group=$(grep -A1 -E '^[[:space:]]*consensus-and-crypto:' .github/dependabot.yml | sed -nE 's/.*patterns: *\[(.*)\].*/\1/p' | tr -d '" ' | tr ',' ' ')
[ -n "$group" ] || bad ".github/dependabot.yml has no consensus-and-crypto group patterns"
read -r -a pats <<< "$group" # (no pathname expansion: the patterns are globs)
while IFS= read -r crate; do
  [ -n "$crate" ] || continue
  hit=0
  for pat in "${pats[@]}"; do
    # shellcheck disable=SC2053 # glob match on purpose
    [[ $crate == $pat ]] && { hit=1; break; }
  done
  [ $hit = 1 ] || bad "$crate is pinned with = but isn't in dependabot.yml's consensus-and-crypto group"
done < <( {
  for m in node/Cargo.toml node/ghost/Cargo.toml; do
    sed -nE 's/^([a-z0-9_-]+) *= *(\{ *version *= *)?"=[^"]+".*/\1/p' "$m"
  done
  # Table-form pins: `[dependencies.NAME]` followed by `version = "=…"`.
  awk '/^\[dependencies\./{n=$0; sub(/^\[dependencies\./, "", n); sub(/\].*/, "", n)} /^version *= *"=/{ if (n) print n } /^\[/ && !/^\[dependencies\./{n=""}' node/vendor/commonware-stream/Cargo.toml
} | grep -vx 'blockula-ghost' | sort -u)

[ $fail = 0 ] && echo "✓ pins: actions by SHA, images by digest, rust $channel everywhere, cargo --locked, exact crate pins in their own Dependabot group"
exit $fail
