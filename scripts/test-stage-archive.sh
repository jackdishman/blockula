#!/usr/bin/env bash
# Tests scripts/stage-archive.sh against a stand-in oras (a registry in a directory), offline: a
# push stages exactly the closed files, each checked against meta.json, with meta.json saying none
# is open; a pull brings them back for scripts/archive-chain.mjs to continue from; a pull that
# can't read the staged copy fails, whatever the registry says (nothing staged yet too: the first
# run starts fresh); files that don't match meta.json, or aren't archive files, are refused.
#   bash scripts/test-stage-archive.sh
set -euo pipefail
cd "$(dirname "$0")/.."
STAGE_SH=$PWD/scripts/stage-archive.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
pass=0
fail() { echo "✗ $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "✓ $1"; }
sha256() { if command -v sha256sum > /dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'; }

# The stand-in: `pull REF -o DIR`, `push REF [flags] FILE:TYPE…` on a registry under
# $T/registry (one directory per reference), every call logged. A missing reference fails as oras
# does ("REF: not found"); ORAS_ERROR makes every call fail with that message.
cat > "$T/oras" << 'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >> "$REGISTRY.log"
[ -z "${ORAS_ERROR:-}" ] || { echo "Error: $ORAS_ERROR" >&2; exit 1; }
ref_dir() { printf '%s/%s' "$REGISTRY" "$(printf '%s' "$1" | tr '/:' '__')"; }
case "$1 $2" in
  "pull "*) src=$(ref_dir "$2"); [ -d "$src" ] || { echo "Error: $2: not found" >&2; exit 1; }; [ "$3" = -o ] && cp "$src"/* "$4"/ ;;
  "push "*)
    dst=$(ref_dir "$2"); shift 2; rm -rf "$dst"; mkdir -p "$dst"
    while [ $# -gt 0 ]; do
      case $1 in
        --artifact-type|--annotation) shift 2 ;;
        *) cp "${1%%:*}" "$dst/"; shift ;;
      esac
    done ;;
  *) echo "stand-in oras: unexpected $*" >&2; exit 3 ;;
esac
EOF
chmod +x "$T/oras"
export ORAS=$T/oras REGISTRY=$T/registry STAGE=ghcr.example/blockula-history
mkdir -p "$REGISTRY"

# An archive as archive-chain.mjs leaves it: two closed files and the open one.
A=$T/archive/replay-test
mkdir -p "$A"
echo '{"chain_id":"replay-test"}' > "$A/genesis.json"
for f in 000000000001 000000000003; do printf '{"from":%d}\n' "$((10#$f))" | gzip -9n > "$A/sync-$f.jsonl.gz"; done
printf '{"from":5}\n' > "$A/sync-000000000005.jsonl"
jq -n --arg a "$(sha256 "$A/sync-000000000001.jsonl.gz")" --arg b "$(sha256 "$A/sync-000000000003.jsonl.gz")" \
  '{chain_id: "replay-test", genesis_hash: "00", source: "test", segments: [{file: "sync-000000000001.jsonl.gz", from: 1, to: 2, sha256: $a}, {file: "sync-000000000003.jsonl.gz", from: 3, to: 4, sha256: $b}], open: {file: "sync-000000000005.jsonl", from: 5}}' > "$A/meta.json"

code=0; out=$(CHAIN_ID=replay-test "$STAGE_SH" pull "$T/fresh" 2>&1) || code=$?
[ "$code" = 1 ] && [[ $out == *"not found"*"can't pull"*"start fresh"* && -z $(ls -A "$T/fresh") ]] || fail "pull with nothing staged: exit $code: $out"
ok "nothing staged yet fails the pull, pointing at a fresh first run (it doesn't start over by itself)"

code=0; out=$(ORAS_ERROR="response status code 403: denied: requested access to the resource is denied" CHAIN_ID=replay-test "$STAGE_SH" pull "$T/denied" 2>&1) || code=$?
[ "$code" = 1 ] && [[ $out == *"denied"*"can't pull"* && -z $(ls -A "$T/denied") ]] || fail "a registry that refuses: exit $code: $out"
ok "a registry that refuses fails the pull too"

out=$("$STAGE_SH" push "$A" 2>&1) || fail "push: $out"
staged=$REGISTRY/ghcr.example_blockula-history_replay-test
got=$(cd "$staged" && printf '%s ' *)
[ "$got" = "genesis.json meta.json sync-000000000001.jsonl.gz sync-000000000003.jsonl.gz " ] || fail "push staged: $got"
jq -e '.open == null and (.segments | length) == 2' "$staged/meta.json" > /dev/null || fail "the staged meta.json: $(cat "$staged/meta.json")"
grep -q "push ghcr.example/blockula-history:replay-test --artifact-type application/vnd.blockula.archive.v1 --annotation org.opencontainers.image.source=" "$REGISTRY.log" || fail "push call: $(cat "$REGISTRY.log")"
ok "push stages the closed files, genesis and meta.json (no open file), linked to the repository"

out=$(CHAIN_ID=replay-test "$STAGE_SH" pull "$T/back" 2>&1) || fail "pull: $out"
for f in genesis.json sync-000000000001.jsonl.gz sync-000000000003.jsonl.gz; do cmp -s "$A/$f" "$T/back/$f" || fail "pull: $f differs"; done
jq -e '.open == null and .segments[-1].to == 4' "$T/back/meta.json" > /dev/null || fail "pulled meta.json: $(cat "$T/back/meta.json")"
[[ $out == *"heights 1..4"* ]] || fail "pull says: $out"
ok "pull brings them back, for archive-chain.mjs to continue from height 5"

cp "$A/sync-000000000001.jsonl.gz" "$T/keep.gz"
printf 'x' >> "$A/sync-000000000001.jsonl.gz"
code=0; out=$("$STAGE_SH" push "$A" 2>&1) || code=$?
[ "$code" = 1 ] && [[ $out == *"doesn't hash to"* ]] || fail "a damaged file: exit $code: $out"
cp "$T/keep.gz" "$A/sync-000000000001.jsonl.gz"
ok "a closed file that doesn't hash to meta.json's SHA-256 is refused"

jq '.segments[0].file = "../../etc/passwd"' "$A/meta.json" > "$T/bad.json" && cp "$A/meta.json" "$T/meta.keep" && mv "$T/bad.json" "$A/meta.json"
code=0; out=$("$STAGE_SH" push "$A" 2>&1) || code=$?
[ "$code" = 1 ] && [[ $out == *"not a closed archive file"* ]] || fail "a strange file name: exit $code: $out"
mv "$T/meta.keep" "$A/meta.json"
ok "meta.json naming anything but a closed archive file is refused"

E=$T/archive/empty && mkdir -p "$E" && cp "$A/genesis.json" "$E/" && jq '.segments = []' "$A/meta.json" > "$E/meta.json"
: > "$REGISTRY.log"
out=$("$STAGE_SH" push "$E" 2>&1) || fail "push with no closed files: $out"
[[ $out == *"no closed files"* && ! -s $REGISTRY.log ]] || fail "push with no closed files: $out / $(cat "$REGISTRY.log")"
ok "an archive with no closed file yet stages nothing"

jq '.segments = null' "$A/meta.json" > "$E/meta.json"
code=0; out=$("$STAGE_SH" push "$E" 2>&1) || code=$?
[ "$code" != 0 ] && [ ! -s "$REGISTRY.log" ] || fail "a meta.json without its segments: exit $code: $out"
ok "a meta.json that doesn't list its segments fails the push (it isn't read as none)"

jq 'del(.chain_id)' "$A/meta.json" > "$E/meta.json"
code=0; out=$("$STAGE_SH" push "$E" 2>&1) || code=$?
[ "$code" = 1 ] && [[ $out == *"has no chain id"* ]] && [ ! -s "$REGISTRY.log" ] || fail "a meta.json without its chain id: exit $code: $out"
ok "a meta.json without its chain id fails the push (it isn't staged as \"null\")"

code=0; CHAIN_ID='../x' "$STAGE_SH" pull "$T/x" > /dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "a chain id that isn't one: exit $code"
ok "a pull refuses a chain id that isn't one"

echo "stage-archive.sh: $pass checks passed"
