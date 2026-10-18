#!/usr/bin/env bash
# Tests scripts/pin-network.sh on a copy of the files it edits (CI `node` job, after the release
# build): a chain gets pinned in the node's trust table, the MCP and the wallet; reruns change
# nothing; checkpoints accumulate; a different genesis for a pinned chain id, a genesis file that
# doesn't hash to the pin, mainnet and malformed input are refused with nothing written. The
# copy's trust file is checked against the node's schema here; in the release PR, `cargo test`
# (embedded_files_parse_and_mainnet_waits_for_its_ceremony) and the MCP's tests check the real
# ones.
#
#   bash scripts/test-pin-network.sh
#   GENESIS=~/.config/blockula/blockula-testnet-2-rehearsal/genesis.json bash scripts/test-pin-network.sh
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${BIN:-$PWD/node/target/release/blockula}
[ -x "$BIN" ] || { echo "needs a built blockula ($BIN): cargo build --release --locked --manifest-path node/Cargo.toml" >&2; exit 1; }
T=$(mktemp -d "${TMPDIR:-/tmp}/blockula-pin-test.XXXXXX")
trap 'rm -rf "$T"' EXIT
fails=0
ok() { echo "✓ $1"; }
bad() { echo "✗ $1" >&2; fails=$((fails + 1)); }

# The genesis to pin: the one given (a rehearsal's), or a fresh one.
if [ -n "${GENESIS:-}" ]; then
  G=$GENESIS
else
  "$BIN" summon pin-test --home "$T/chain" > /dev/null
  G=$T/chain/genesis.json
fi
CHAIN=$(node -p "JSON.parse(require('fs').readFileSync('$G', 'utf8')).chain_id")
HASH=$("$BIN" checkpoint --genesis "$G")
CLOCK=$(G="$G" node -p 'const g = JSON.parse(require("fs").readFileSync(process.env.G, "utf8")); `${g.created_at_ms}:${g.block_time_ms}`')
CREATED=${CLOCK%:*}
BLOCK_TIME=${CLOCK#*:}
TIME1=$((CREATED + BLOCK_TIME))
TIME2=$((CREATED + 2 * BLOCK_TIME))
ZERO=$("$BIN" checkpoint --genesis "$G" --json)
ZERO_SET=$(ZERO="$ZERO" node -p 'JSON.parse(process.env.ZERO).set_hash')

# A checkout with just what the script edits.
fresh() {
  rm -rf "$T/repo"
  mkdir -p "$T/repo/node/src" "$T/repo/node/checkpoints" "$T/repo/mcp/src" "$T/repo/web/lib"
  cp node/src/checkpoints.rs "$T/repo/node/src/"
  cp node/checkpoints/*.json "$T/repo/node/checkpoints/"
  cp mcp/src/network.ts "$T/repo/mcp/src/"
  # The wallet's testnet is this chain, so its constant is pinned too.
  sed "s/export const TESTNET_CHAIN_ID = \"[^\"]*\";/export const TESTNET_CHAIN_ID = \"$CHAIN\";/" web/lib/config.ts > "$T/repo/web/lib/config.ts"
}
pin() { ROOT="$T/repo" BIN="$BIN" bash scripts/pin-network.sh "$@" > "$T/out" 2>&1; }
snapshot() { (cd "$T/repo" && find . -type f | sort | xargs shasum) ; }

fresh
SET=$(printf 'cd%.0s' $(seq 32))
CP1="1:$(printf 'ab%.0s' $(seq 32)):$SET:$TIME1"
if pin "$CHAIN" "$HASH" --genesis "$G" --checkpoint "$CP1"; then ok "pins $CHAIN ($HASH)"; else bad "pin failed: $(cat "$T/out")"; fi
f=$T/repo/node/checkpoints/$CHAIN.json
node -e "
const k = JSON.parse(require('fs').readFileSync('$f', 'utf8'));
const want = { chain_id: '$CHAIN', genesis_hash: '$HASH', checkpoints: [{ height: 1, hash: 'ab'.repeat(32), set_hash: 'cd'.repeat(32), timestamp_ms: $TIME1 }] };
for (const key of Object.keys(k)) if (!['chain_id', 'genesis_hash', 'checkpoints', 'note', 'weak_subjectivity_blocks'].includes(key)) throw new Error('unknown field ' + key);
if (JSON.stringify({ chain_id: k.chain_id, genesis_hash: k.genesis_hash, checkpoints: k.checkpoints }) !== JSON.stringify(want)) throw new Error(JSON.stringify(k));
if (!k.note) throw new Error('no note');
" && ok "node/checkpoints/$CHAIN.json: the hash and the checkpoint, in the node's schema" || bad "trust file: $(cat "$f")"
grep -qF "include_str!(\"../checkpoints/$CHAIN.json\")" "$T/repo/node/src/checkpoints.rs" && grep -qF 'include_str!("../checkpoints/blockula-1.json")' "$T/repo/node/src/checkpoints.rs" && ok "EMBEDDED gains the file, keeps mainnet's" || bad "EMBEDDED: $(grep EMBEDDED "$T/repo/node/src/checkpoints.rs")"
grep -qF "export const KNOWN_GENESIS: Record<string, string> = { \"$CHAIN\": \"$HASH\" };" "$T/repo/mcp/src/network.ts" && ok "the MCP's KNOWN_GENESIS" || bad "KNOWN_GENESIS: $(grep KNOWN_GENESIS "$T/repo/mcp/src/network.ts")"
grep -qF "\"$CHAIN\": { createdAtMs: $CREATED, blockTimeMs: $BLOCK_TIME }" "$T/repo/mcp/src/network.ts" && ok "the MCP's pinned genesis clock" || bad "MCP clock missing"
grep -qF "export const TESTNET_GENESIS_HASH: string | undefined = \"$HASH\";" "$T/repo/web/lib/config.ts" && ok "the wallet's TESTNET_GENESIS_HASH" || bad "wallet: $(grep TESTNET_GENESIS_HASH "$T/repo/web/lib/config.ts")"
grep -qF "export const TESTNET_GENESIS_CLOCK: GenesisClock | undefined = { createdAtMs: $CREATED, blockTimeMs: $BLOCK_TIME };" "$T/repo/web/lib/config.ts" && ok "the wallet's pinned genesis clock" || bad "wallet clock missing"
cmp -s "$G" "$T/repo/genesis/$CHAIN/genesis.json" && ok "genesis/$CHAIN/genesis.json is the genesis (release.yml ships it as genesis-$CHAIN.json)" || bad "genesis not copied"
src=$(dirname "$G")
if [ -f "$src/spec.toml" ]; then
  cmp -s "$src/spec.toml" "$T/repo/genesis/$CHAIN/spec.toml" && [ -d "$T/repo/genesis/$CHAIN/operators" ] && ok "with its public inputs (spec.toml, operators/)" || bad "spec not copied"
  (cd "$T/repo/genesis/$CHAIN" && "$BIN" genesis build --spec spec.toml --out "$T/rebuilt.json" > /dev/null 2>&1) && cmp -s "$G" "$T/rebuilt.json" && ok "which rebuild it byte for byte" || bad "the committed spec doesn't rebuild the genesis"
fi
if ls "$T/repo/genesis/$CHAIN" | grep -qvE '^(genesis\.json|spec\.toml|operators)$'; then bad "something besides the public inputs: $(ls "$T/repo/genesis/$CHAIN")"; else ok "and nothing else (no keys)"; fi
grep -rqE '"seed"|validator\.key' "$T/repo/genesis" && bad "a secret in genesis/" || ok "no secret in genesis/"

after=$(snapshot)
pin "$CHAIN" "$HASH" --genesis "$G" --checkpoint "$CP1" || bad "rerun failed: $(cat "$T/out")"
[ "$(snapshot)" = "$after" ] && grep -q "unchanged" "$T/out" && ok "a rerun changes nothing" || bad "a rerun changed files"
CP2="2:$(printf 'ef%.0s' $(seq 32)):$SET:$TIME2"
pin "$CHAIN" "$HASH" --checkpoint "$CP2" || bad "a second checkpoint failed: $(cat "$T/out")"
[ "$(node -p "JSON.parse(require('fs').readFileSync('$f', 'utf8')).checkpoints.map(c => c.height).join(',')")" = "1,2" ] && ok "checkpoints accumulate, oldest first" || bad "checkpoints: $(cat "$f")"
[ "$(grep -o "checkpoints/$CHAIN.json" "$T/repo/node/src/checkpoints.rs" | wc -l | tr -d ' ')" = 1 ] && ok "EMBEDDED lists the file once" || bad "EMBEDDED repeated"

# Refusals leave everything as it was.
after=$(snapshot)
refused() { # what why args…
  local what=$1 why=$2; shift 2
  if pin "$@"; then bad "$what: accepted"; elif ! grep -qF -- "$why" "$T/out"; then bad "$what: $(cat "$T/out")"; elif [ "$(snapshot)" != "$after" ]; then bad "$what: changed files"; else ok "refuses $what"; fi
}
OTHER=$(printf '12%.0s' $(seq 32))
refused "another genesis for a pinned chain id" "hashes to $HASH, not $OTHER" "$CHAIN" "$OTHER"
refused "a genesis file that doesn't hash to the pin" "hashes to $HASH, not $OTHER" "$CHAIN" "$OTHER" --genesis "$G"
refused "mainnet (the ceremony pins it)" "genesis ceremony" blockula-1 "$HASH"
refused "a malformed hash" "64 hex digits" "$CHAIN" abc
refused "a checkpoint without its set hash" "HEIGHT:HASH:SET_HASH" "$CHAIN" "$HASH" --checkpoint "5:$HASH"
refused "an untimed embedded checkpoint" "HEIGHT:HASH:SET_HASH:TIMESTAMP_MS" "$CHAIN" "$HASH" --checkpoint "1:$HASH:$SET"
refused "a conflicting checkpoint" "conflicts" "$CHAIN" "$HASH" --checkpoint "1:$OTHER:$SET:$TIME1"
refused "a checkpoint whose time changed" "conflicts" "$CHAIN" "$HASH" --checkpoint "1:$(printf 'ab%.0s' $(seq 32)):$SET:$TIME2"
refused "a checkpoint before its earliest possible time" "earlier than" "$CHAIN" "$HASH" --checkpoint "3:$HASH:$SET:$TIME2"
FUTURE=$(node -p 'Date.now() + 7200000')
refused "a checkpoint from the future" "in the future" "$CHAIN" "$HASH" --checkpoint "3:$HASH:$SET:$FUTURE"
refused "a height-zero checkpoint with another hash" "checkpoint #0 must be the genesis hash" "$CHAIN" "$HASH" --checkpoint "0:$OTHER:$ZERO_SET:$CREATED"
refused "a height-zero checkpoint with another set" "checkpoint #0 must pin the genesis set" "$CHAIN" "$HASH" --checkpoint "0:$HASH:$SET:$CREATED"
refused "a height-zero checkpoint with another time" "checkpoint #0 must record created_at_ms" "$CHAIN" "$HASH" --checkpoint "0:$HASH:$ZERO_SET:$((CREATED + 1))"
pin "$CHAIN" "$HASH" --checkpoint "0:$HASH:$ZERO_SET:$CREATED" && ok "accepts the genuine genesis checkpoint" || bad "the genuine genesis checkpoint: $(cat "$T/out")"
# A remembered clock alone cannot authenticate the genesis validator set.
mv "$T/repo/genesis/$CHAIN/genesis.json" "$T/saved-genesis.json"
after=$(snapshot)
refused "a height-zero checkpoint without its genesis" "needs its genesis to check the validator set" "$CHAIN" "$HASH" --checkpoint "0:$HASH:$ZERO_SET:$CREATED"
mv "$T/saved-genesis.json" "$T/repo/genesis/$CHAIN/genesis.json"

# A chain that isn't the wallet's testnet leaves the wallet alone.
fresh
sed -i.bak "s/export const TESTNET_CHAIN_ID = \"[^\"]*\";/export const TESTNET_CHAIN_ID = \"someone-else\";/" "$T/repo/web/lib/config.ts" && rm "$T/repo/web/lib/config.ts.bak"
after=$(snapshot)
refused "a new chain without its genesis clock" "needs its genesis clock" "$CHAIN" "$HASH"
pin "$CHAIN" "$HASH" --genesis "$G" || bad "pin failed: $(cat "$T/out")"
grep -qF 'export const TESTNET_GENESIS_HASH: string | undefined = undefined;' "$T/repo/web/lib/config.ts" && ok "another chain doesn't touch the wallet's testnet" || bad "the wallet was pinned to another chain"
grep -qF 'export const TESTNET_GENESIS_CLOCK: GenesisClock | undefined = undefined;' "$T/repo/web/lib/config.ts" && ok "another chain doesn't touch the wallet's clock" || bad "the wallet's clock was pinned to another chain"

if [ $fails -gt 0 ]; then echo "✗ $fails failed" >&2; exit 1; fi
echo "✓ pin-network.sh behaves"
