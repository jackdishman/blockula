#!/usr/bin/env bash
# Stages a chain archive's closed files where every nightly replay can read them: an OCI artifact
# on GitHub's container registry, one tag per chain (docs/REPLAY.md "The archive CI reads"). An
# archive is what scripts/archive-chain.mjs writes: meta.json, genesis.json, closed files of about
# 100,000 heights (gzipped, their SHA-256 in meta.json) and the open file it still appends to.
# Only closed files are staged, with meta.json saying none is open: the open one's heights are
# always under 100,000 blocks old, so the next run copies them from the node again (the public
# node keeps 14 days).
#
#   scripts/stage-archive.sh pull <archive dir>   # the staged copy into <archive dir>; any error
#                                                 # fails, "not found" too (a first run starts
#                                                 # fresh instead of pulling)
#   scripts/stage-archive.sh push <archive dir>   # its closed files, each checked against
#                                                 # meta.json first
#
# The chain's tag is its id (meta.json's chain_id, or CHAIN_ID for a pull). Log in first: the
# nightly workflow does with its token; by hand, `gh auth refresh -s write:packages`, then
#   gh auth token | oras login ghcr.io -u <you> --password-stdin
# Environment: STAGE (the repository, default ghcr.io/jackdishman/blockula-history), CHAIN_ID
# (pull), ORAS (the oras command; the tests use a stand-in), SOURCE (the repository the artifact
# links to).
set -euo pipefail

STAGE=${STAGE:-ghcr.io/jackdishman/blockula-history}
ORAS=${ORAS:-oras}
SOURCE=${SOURCE:-https://github.com/jackdishman/blockula}
usage() { echo "usage: $0 pull|push <archive dir>" >&2; exit 2; }
[ $# = 2 ] || usage
cmd=$1 dir=$2

sha256() {
  if command -v sha256sum > /dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'
}

case $cmd in
  pull)
    : "${CHAIN_ID:?CHAIN_ID is the tag to pull}"
    [[ $CHAIN_ID =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] || { echo "unexpected chain id $CHAIN_ID" >&2; exit 2; }
    mkdir -p "$dir"
    # Whatever stops it fails the pull, "not found" included: registries word a missing package,
    # no access and an outage alike (GHCR can answer "denied" for a package never pushed), and a
    # run that went on would start the archive over from height 1. Only the first run starts
    # fresh, on purpose (replay.yml's `fresh` input).
    if ! "$ORAS" pull "$STAGE:$CHAIN_ID" -o "$dir"; then
      echo "can't pull $STAGE:$CHAIN_ID. If nothing is staged yet, start fresh: docs/REPLAY.md \"The first run\"" >&2
      exit 1
    fi
    jq -e '.open == null' "$dir/meta.json" > /dev/null || { echo "the staged meta.json says a file is open" >&2; exit 1; }
    echo "pulled $STAGE:$CHAIN_ID: heights 1..$(jq -r '.segments[-1].to // 0' "$dir/meta.json")"
    ;;
  push)
    out=$(mktemp -d)
    trap 'rm -rf "$out"' EXIT
    # meta.json is read once: archive-chain.mjs may close a file while this runs (the maintainer's
    # copy is a live archive), and the staged meta.json must list exactly the files staged.
    meta=$out/meta.json.read
    cp "$dir/meta.json" "$meta"
    chain=$(jq -er '.chain_id | strings' "$meta") || { echo "$dir/meta.json has no chain id" >&2; exit 1; }
    [[ $chain =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] || { echo "$dir/meta.json: unexpected chain id $chain" >&2; exit 1; }
    rows=$(jq -r '.segments[] | "\(.file)\t\(.sha256)"' "$meta")
    [ -n "$rows" ] || { echo "$dir has no closed files to stage yet"; exit 0; }
    files=()
    while IFS=$'\t' read -r file sha; do
      [[ $file =~ ^sync-[0-9]{12}\.jsonl\.gz$ ]] || { echo "$dir/meta.json lists $file, not a closed archive file" >&2; exit 1; }
      ln "$dir/$file" "$out/$file" 2> /dev/null || cp "$dir/$file" "$out/$file"
      [ "$(sha256 "$out/$file")" = "$sha" ] || { echo "$dir/$file doesn't hash to $sha (meta.json)" >&2; exit 1; }
      files+=("$file:application/gzip")
    done <<< "$rows"
    jq '.open = null' "$meta" > "$out/meta.json"
    cp "$dir/genesis.json" "$out/genesis.json"
    (cd "$out" && "$ORAS" push "$STAGE:$chain" --artifact-type application/vnd.blockula.archive.v1 \
      --annotation "org.opencontainers.image.source=$SOURCE" \
      --annotation "org.opencontainers.image.description=$chain's finalized blocks and certificates (scripts/archive-chain.mjs), replayed nightly" \
      meta.json:application/json genesis.json:application/json "${files[@]}")
    echo "staged $STAGE:$chain: heights 1..$(jq -r '.segments[-1].to' "$meta") in ${#files[@]} closed files"
    ;;
  *) usage ;;
esac
