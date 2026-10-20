#!/usr/bin/env bash
# Tests scripts/pin-network.sh on a copy of the files it edits (CI `node` job, after the release
# build): a chain gets pinned in the node's trust table, the MCP and the wallet; reruns change
# nothing; checkpoints accumulate; a different genesis for a pinned chain id, a genesis file that
# doesn't hash to the pin, mainnet without --mainnet and malformed input are refused with nothing
# written. Then mainnet's guarded pin (--mainnet, CONFIRM_MAINNET): its refusals, and what it
# writes (the placeholder's hash, the MCP's tables and RPCs, the wallet's mainnet constants,
# genesis/blockula-1/). A blockula-1 genesis can't be made here (its operators' proofs of
# possession, the builder's policy), so those cases run a stand-in blockula that hashes files
# with SHA-256, unless MAINNET_GENESIS names a real one (the ceremony's, or a dry run's built for
# blockula-1) to run them with the real binary. The copy's trust file is checked against the
# node's schema here; in the release PR, `cargo test` (embedded_files_parse_and_mainnet_waits_for_
# its_ceremony) and the MCP's tests check the real ones.
#
#   bash scripts/test-pin-network.sh
#   GENESIS=~/.config/blockula/blockula-testnet-2-rehearsal/genesis.json bash scripts/test-pin-network.sh
#   MAINNET_GENESIS=<dir>/genesis.json bash scripts/test-pin-network.sh   (spec.toml, operators/ beside it)
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

# A checkout with just what the script edits, as it is before any chain is pinned: whatever this
# tree already pins (a launched testnet, from the release after its launch on) is taken out of the
# copies, so every case below is a first pin and a release's own pins can't fail the test. A rule
# that no longer matches its file fails loudly rather than testing the wrong thing.
fresh() {
  rm -rf "$T/repo"
  mkdir -p "$T/repo/node/src" "$T/repo/node/checkpoints" "$T/repo/mcp/src" "$T/repo/web/lib"
  # Mainnet's placeholder, as it is until the launch release pins it.
  cat > "$T/repo/node/checkpoints/blockula-1.json" <<'PLACEHOLDER'
{
  "chain_id": "blockula-1",
  "genesis_hash": "",
  "checkpoints": [],
  "note": "TODO(ceremony): placeholder. Set genesis_hash to the hash every genesis builder reproduced (docs/GENESIS_CEREMONY.md) in the release that launches mainnet; until then this binary refuses to run or join blockula-1. Every later release appends a checkpoint (docs/OPERATIONS.md, Weak subjectivity)."
}
PLACEHOLDER
  CHAIN="$CHAIN" OUT="$T/repo" node --input-type=module <<'JS' || exit 1
import { readFileSync, writeFileSync } from "node:fs";
const reset = (file, rules) => {
  let s = readFileSync(file, "utf8");
  for (const [re, by] of rules) {
    if (!re.test(s)) { console.error(`✗ test-pin-network.sh: ${file} has no ${re}`); process.exit(1); }
    s = s.replace(re, () => by);
  }
  writeFileSync(`${process.env.OUT}/${file}`, s);
};
reset("node/src/checkpoints.rs", [[/const EMBEDDED: &\[&str\] = &\[[^\]]*\];/, 'const EMBEDDED: &[&str] = &[include_str!("../checkpoints/blockula-1.json")];']]);
reset("node/src/cli_net.rs", []); // read only: the tests the mainnet pin names
reset("mcp/src/network.ts", [
  [/export const KNOWN_GENESIS: Record<string, string> = \{[^}]*\};/, "export const KNOWN_GENESIS: Record<string, string> = {};"],
  [/export const KNOWN_CLOCKS: Record<string, GenesisClock> = \{(?:[^{}]|\{[^{}]*\})*\};/, "export const KNOWN_CLOCKS: Record<string, GenesisClock> = {};"],
  [/export const MAINNET_RPCS: string\[\] = \[[^\]]*\];/, "export const MAINNET_RPCS: string[] = [];"],
]);
// The wallet's testnet is this chain, so its constants are pinned too.
reset("web/lib/config.ts", [
  [/export const TESTNET_CHAIN_ID = "[^"]*";/, `export const TESTNET_CHAIN_ID = "${process.env.CHAIN}";`],
  [/export const TESTNET_GENESIS_HASH: string \| undefined = (?:undefined|"[0-9a-f]{64}");/, "export const TESTNET_GENESIS_HASH: string | undefined = undefined;"],
  [/export const TESTNET_GENESIS_CLOCK: GenesisClock \| undefined = (?:undefined|\{ createdAtMs: \d+, blockTimeMs: \d+ \});/, "export const TESTNET_GENESIS_CLOCK: GenesisClock | undefined = undefined;"],
  [/export const MAINNET_GENESIS_HASH: string \| undefined = (?:undefined|"[0-9a-f]{64}");/, "export const MAINNET_GENESIS_HASH: string | undefined = undefined;"],
  [/export const MAINNET_GENESIS_CLOCK: GenesisClock \| undefined = (?:undefined|\{ createdAtMs: \d+, blockTimeMs: \d+ \});/, "export const MAINNET_GENESIS_CLOCK: GenesisClock | undefined = undefined;"],
]);
JS
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

# ---- mainnet: pinned only with --mainnet and CONFIRM_MAINNET, from the ceremony's inputs ----

if [ -n "${MAINNET_GENESIS:-}" ]; then
  MBIN=$BIN MG=$MAINNET_GENESIS
else
  # The stand-in blockula: a genesis file's "hash" is its SHA-256, its genesis checkpoint pins a
  # fixed set, and `genesis build` reproduces the genesis.json beside the spec (STUB_REBUILD=other:
  # something else, as a spec that doesn't rebuild it would).
  MBIN=$T/stub-blockula
  cat > "$MBIN" <<'STUB'
#!/usr/bin/env bash
sha() { if command -v sha256sum > /dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
case "$1 ${2:-}" in
  "--version ") echo "blockula 0.0.0 (stand-in)" ;;
  "checkpoint --genesis")
    [ -f "$3" ] || exit 1
    if [ "${4:-}" = --json ]; then
      G="$3" H="$(sha "$3")" node -e 'const g = JSON.parse(require("fs").readFileSync(process.env.G, "utf8")); console.log(JSON.stringify({ height: 0, hash: process.env.H, set_hash: "5e".repeat(32), timestamp_ms: g.created_at_ms }))'
    else sha "$3"; fi ;;
  "genesis build")
    if [ "${STUB_REBUILD:-}" = other ]; then echo '{}' > "$6"; else cp "$(dirname "$4")/genesis.json" "$6"; fi ;;
  *) exit 2 ;;
esac
STUB
  chmod +x "$MBIN"
  mkdir -p "$T/ceremony/operators"
  printf '{"chain_id":"blockula-1","address_prefix":"boo","created_at_ms":%s,"block_time_ms":1000,"validators":[]}\n' "$((CREATED - 86400000))" > "$T/ceremony/genesis.json"
  printf 'chain_id = "blockula-1"\noperators = ["operators/a.toml"]\n' > "$T/ceremony/spec.toml"
  echo '[operator]' > "$T/ceremony/operators/a.toml"
  MG=$T/ceremony/genesis.json
fi
MSRC=$(dirname "$MG")
MH=$("$MBIN" checkpoint --genesis "$MG")
MCLOCK=$(G="$MG" node -p 'const g = JSON.parse(require("fs").readFileSync(process.env.G, "utf8")); `${g.created_at_ms}:${g.block_time_ms}`')
mpin() { CONFIRM_MAINNET=${CONFIRM-$MH} ROOT="$T/repo" BIN="$MBIN" bash scripts/pin-network.sh "$@" > "$T/out" 2>&1; }
mrefused() { # what why args…
  local what=$1 why=$2; shift 2
  if mpin "$@"; then bad "mainnet, $what: accepted"; elif ! grep -qF -- "$why" "$T/out"; then bad "mainnet, $what: $(cat "$T/out")"; elif [ "$(snapshot)" != "$after" ]; then bad "mainnet, $what: changed files"; else ok "mainnet: refuses $what"; fi
}

fresh
after=$(snapshot)
mrefused "without --mainnet" "genesis ceremony" blockula-1 "$MH" --genesis "$MG"
CONFIRM="" mrefused "without CONFIRM_MAINNET" "set CONFIRM_MAINNET" blockula-1 "$MH" --mainnet --genesis "$MG"
CONFIRM=$OTHER mrefused "with CONFIRM_MAINNET naming another hash" "set CONFIRM_MAINNET" blockula-1 "$MH" --mainnet --genesis "$MG"
mrefused "--mainnet for another chain" "--mainnet pins blockula-1" "$CHAIN" "$MH" --mainnet --genesis "$MG"
mrefused "a first pin without the genesis" "needs --genesis" blockula-1 "$MH" --mainnet
CONFIRM=$OTHER mrefused "a genesis that doesn't hash to the pin" "hashes to $MH, not $OTHER" blockula-1 "$OTHER" --mainnet --genesis "$MG"
mrefused "an http RPC" "https://host" blockula-1 "$MH" --mainnet --genesis "$MG" --rpc http://rpc.example
mrefused "an RPC with a path" "https://host" blockula-1 "$MH" --mainnet --genesis "$MG" --rpc https://rpc.example/api
refused "--rpc without --mainnet" "goes with --mainnet" "$CHAIN" "$HASH" --genesis "$G" --rpc https://rpc.example
mkdir -p "$T/bare" && cp "$MG" "$T/bare/genesis.json"
mrefused "a genesis without its spec and operators" "spec.toml and operators" blockula-1 "$MH" --mainnet --genesis "$T/bare/genesis.json"
if [ -z "${MAINNET_GENESIS:-}" ]; then
  STUB_REBUILD=other mrefused "a spec that doesn't rebuild the genesis" "doesn't rebuild" blockula-1 "$MH" --mainnet --genesis "$MG"
  mkdir -p "$T/tboo/operators" && cp "$T/ceremony/spec.toml" "$T/tboo/" && cp "$T/ceremony/operators/a.toml" "$T/tboo/operators/"
  sed 's/"address_prefix":"boo"/"address_prefix":"tboo"/' "$MG" > "$T/tboo/genesis.json"
  TH=$("$MBIN" checkpoint --genesis "$T/tboo/genesis.json")
  CONFIRM=$TH mrefused "a genesis without boo addresses" "isn't mainnet's genesis" blockula-1 "$TH" --mainnet --genesis "$T/tboo/genesis.json"
fi

if mpin blockula-1 "$MH" --mainnet --genesis "$MG" --rpc https://rpc-a.blockula.example --rpc https://rpc-b.blockula.example:8443; then ok "mainnet: pins blockula-1 ($MH)"; else bad "mainnet pin failed: $(cat "$T/out")"; fi
mf=$T/repo/node/checkpoints/blockula-1.json
node -e "
const k = JSON.parse(require('fs').readFileSync('$mf', 'utf8'));
if (k.chain_id !== 'blockula-1' || k.genesis_hash !== '$MH' || k.checkpoints.length !== 0) throw new Error(JSON.stringify(k));
if (/TODO\(ceremony\)/.test(k.note) || !/mainnet/.test(k.note)) throw new Error('note: ' + k.note);
" && ok "mainnet: node/checkpoints/blockula-1.json has the hash, and the placeholder's note is gone" || bad "mainnet trust file: $(cat "$mf")"
[ "$(grep -oF 'include_str!("../checkpoints/blockula-1.json")' "$T/repo/node/src/checkpoints.rs" | wc -l | tr -d ' ')" = 1 ] && ok "mainnet: EMBEDDED already lists blockula-1, once" || bad "EMBEDDED: $(grep EMBEDDED "$T/repo/node/src/checkpoints.rs")"
grep -qF "export const KNOWN_GENESIS: Record<string, string> = { \"blockula-1\": \"$MH\" };" "$T/repo/mcp/src/network.ts" && ok "mainnet: the MCP's KNOWN_GENESIS" || bad "KNOWN_GENESIS: $(grep KNOWN_GENESIS "$T/repo/mcp/src/network.ts")"
grep -qF "\"blockula-1\": { createdAtMs: ${MCLOCK%:*}, blockTimeMs: ${MCLOCK#*:} }" "$T/repo/mcp/src/network.ts" && ok "mainnet: the MCP's KNOWN_CLOCKS" || bad "KNOWN_CLOCKS: $(grep KNOWN_CLOCKS "$T/repo/mcp/src/network.ts")"
grep -qF 'export const MAINNET_RPCS: string[] = ["https://rpc-a.blockula.example", "https://rpc-b.blockula.example:8443"];' "$T/repo/mcp/src/network.ts" && ok "mainnet: the MCP's MAINNET_RPCS, in order" || bad "MAINNET_RPCS: $(grep 'MAINNET_RPCS:' "$T/repo/mcp/src/network.ts")"
grep -qF "export const MAINNET_GENESIS_HASH: string | undefined = \"$MH\";" "$T/repo/web/lib/config.ts" && ok "mainnet: the wallet's MAINNET_GENESIS_HASH" || bad "wallet: $(grep 'MAINNET_GENESIS_HASH:' "$T/repo/web/lib/config.ts")"
grep -qF "export const MAINNET_GENESIS_CLOCK: GenesisClock | undefined = { createdAtMs: ${MCLOCK%:*}, blockTimeMs: ${MCLOCK#*:} };" "$T/repo/web/lib/config.ts" && ok "mainnet: the wallet's MAINNET_GENESIS_CLOCK" || bad "wallet clock: $(grep 'MAINNET_GENESIS_CLOCK:' "$T/repo/web/lib/config.ts")"
grep -qF 'export const TESTNET_GENESIS_HASH: string | undefined = undefined;' "$T/repo/web/lib/config.ts" && ok "mainnet: the wallet's testnet is untouched" || bad "the wallet's testnet was pinned to mainnet"
cmp -s "$MG" "$T/repo/genesis/blockula-1/genesis.json" && cmp -s "$MSRC/spec.toml" "$T/repo/genesis/blockula-1/spec.toml" && [ -n "$(ls "$T/repo/genesis/blockula-1/operators/")" ] \
  && ok "mainnet: genesis/blockula-1/ has the genesis, its spec and operators" || bad "genesis/blockula-1: $(ls -R "$T/repo/genesis/blockula-1")"
if ls "$T/repo/genesis/blockula-1" | grep -qvE '^(genesis\.json|spec\.toml|operators)$'; then bad "mainnet: something besides the public inputs: $(ls "$T/repo/genesis/blockula-1")"; else ok "mainnet: and nothing else"; fi
expect_flip=$(grep -c 'embedded_files_parse_and_mainnet_waits_for_its_ceremony' "$T/out" || true)
[ "$expect_flip" -ge 1 ] && ok "mainnet: says which placeholder test the PR deletes" || bad "mainnet: no word of the placeholder test: $(cat "$T/out")"
grep -qF "tests mainnet_is_never_trusted_on_first_use validator_commands_sign_only_addresses_they_were_given stand in for mainnet" "$T/out" && ok "mainnet: and which tests' stand-in mainnet nodes need the pinned genesis" || bad "mainnet: no word of cli_net's stand-in mainnet nodes: $(cat "$T/out")"
grep -qF "NEXT_PUBLIC_MAINNET_GENESIS_HASH must be $MH" "$T/out" && ok "mainnet: and what Vercel's NEXT_PUBLIC_MAINNET_GENESIS_HASH must be" || bad "mainnet: no word of Vercel's setting: $(cat "$T/out")"

after=$(snapshot)
mpin blockula-1 "$MH" --mainnet --genesis "$MG" --rpc https://rpc-a.blockula.example || bad "mainnet rerun failed: $(cat "$T/out")"
[ "$(snapshot)" = "$after" ] && ok "mainnet: a rerun changes nothing" || bad "mainnet: a rerun changed files"
! grep -qF "Also in this PR" "$T/out" && ok "mainnet: a rerun doesn't ask for the first pin's test changes again" || bad "mainnet rerun: $(cat "$T/out")"
MCP1="10:$(printf 'ab%.0s' $(seq 32)):$SET:$((${MCLOCK%:*} + 20000))"
mpin blockula-1 "$MH" --mainnet --checkpoint "$MCP1" --rpc https://rpc-c.blockula.example || bad "mainnet's next release (a checkpoint, a new RPC, the committed genesis): $(cat "$T/out")"
[ "$(node -p "JSON.parse(require('fs').readFileSync('$mf', 'utf8')).checkpoints.map(c => c.height).join(',')")" = 10 ] && ok "mainnet: a later release appends its checkpoint (the committed genesis supplies the clock)" || bad "mainnet checkpoints: $(cat "$mf")"
grep -qF '"https://rpc-b.blockula.example:8443", "https://rpc-c.blockula.example"];' "$T/repo/mcp/src/network.ts" && ok "mainnet: and adds RPCs after the built-in ones" || bad "MAINNET_RPCS: $(grep 'MAINNET_RPCS:' "$T/repo/mcp/src/network.ts")"
after=$(snapshot)
CONFIRM=$OTHER mrefused "another genesis once pinned" "not $OTHER" blockula-1 "$OTHER" --mainnet
CONFIRM=$OTHER mrefused "another genesis once pinned, given with it" "not $OTHER" blockula-1 "$OTHER" --mainnet --genesis "$MG"
refused "a testnet pin of a pinned blockula-1 without --mainnet" "genesis ceremony" blockula-1 "$MH"

if [ $fails -gt 0 ]; then echo "✗ $fails failed" >&2; exit 1; fi
echo "✓ pin-network.sh behaves"
