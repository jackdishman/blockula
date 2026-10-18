#!/usr/bin/env bash
# Pins a launched (non-mainnet) chain in the clients, for the release after its launch
# (deploy/README.md "Relaunch", OPERATIONS.md "Checkpoints in releases"): the node's embedded trust
# file and its line in EMBEDDED, the MCP's KNOWN_GENESIS, and, for the public testnet, the
# wallet's built-in genesis hash. With --genesis it also commits the genesis where the release
# ships it from (genesis/<chain id>/genesis.json, attached as genesis-<chain id>.json), with its
# public inputs (spec.toml, operators/) so anyone can rebuild it byte for byte. Idempotent: rerun
# it for the next release's checkpoint. The tests read these tables, so none of them needs editing.
#
#   scripts/pin-network.sh <chain id> <genesis hash> [--genesis <genesis.json>] [--checkpoint HEIGHT:HASH:SET_HASH]…
#
# e.g. after `RESET=1 deploy/launch.sh`:
#   scripts/pin-network.sh blockula-testnet-2 <hash> --genesis ~/.config/blockula/blockula-testnet-2/genesis.json \
#     --checkpoint "$(blockula checkpoint --peer https://blockula-testnet.fly.dev --genesis-hash <hash>)"
# ROOT (the checkout, default this one) and BIN (a blockula, to check --genesis against the hash)
# are for tests (scripts/test-pin-network.sh).
set -euo pipefail
ROOT=${ROOT:-$(cd "$(dirname "$0")/.." && pwd)}
BIN=${BIN:-$ROOT/node/target/release/blockula}
die() { echo "❌ $*" >&2; exit 1; }

[ $# -ge 2 ] || die "usage: scripts/pin-network.sh <chain id> <genesis hash> [--genesis <genesis.json>] [--checkpoint HEIGHT:HASH:SET_HASH]…"
CHAIN=$1 HASH=$(printf '%s' "$2" | tr 'A-F' 'a-f'); shift 2
GENESIS="" CHECKPOINTS=()
while [ $# -gt 0 ]; do
  case $1 in
    --genesis) GENESIS=${2:?--genesis needs a file}; shift 2 ;;
    --checkpoint) CHECKPOINTS+=("${2:?--checkpoint needs HEIGHT:HASH:SET_HASH}"); shift 2 ;;
    *) die "unknown argument $1" ;;
  esac
done
[[ $CHAIN =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] || die "chain id $CHAIN: lowercase letters, digits and '-'"
[ "$CHAIN" != blockula-1 ] || die "blockula-1 is pinned by the genesis ceremony's release (docs/GENESIS_CEREMONY.md, section 6), not by this script"
[[ $HASH =~ ^[0-9a-f]{64}$ ]] || die "the genesis hash must be 64 hex digits"
for c in ${CHECKPOINTS[@]+"${CHECKPOINTS[@]}"}; do
  [[ $c =~ ^[0-9]+:[0-9a-f]{64}:[0-9a-f]{64}$ ]] || die "checkpoint $c: embedded checkpoints are HEIGHT:HASH:SET_HASH (\`blockula checkpoint\` prints one)"
done

if [ -n "$GENESIS" ]; then
  [ -x "$BIN" ] || die "--genesis needs a blockula to hash it ($BIN; build it, or set BIN)"
  got=$("$BIN" checkpoint --genesis "$GENESIS")
  [ "$got" = "$HASH" ] || die "$GENESIS hashes to $got, not $HASH"
  [ "$(G="$GENESIS" node -p 'JSON.parse(require("fs").readFileSync(process.env.G, "utf8")).chain_id')" = "$CHAIN" ] || die "$GENESIS is another chain's genesis"
  dest=$ROOT/genesis/$CHAIN
  mkdir -p "$dest"
  cp "$GENESIS" "$dest/genesis.json"
  # Its public inputs only: the spec and the operator files (never the keys next to them).
  src=$(dirname "$GENESIS")
  if [ -f "$src/spec.toml" ]; then
    cp "$src/spec.toml" "$dest/spec.toml"
    if [ -d "$src/operators" ]; then mkdir -p "$dest/operators" && cp "$src"/operators/*.toml "$dest/operators/"; fi
  fi
  echo "✓ genesis/$CHAIN/: genesis.json$( [ -f "$dest/spec.toml" ] && echo ", spec.toml, operators/")"
fi

# The node's trust file and its EMBEDDED line, the MCP's table and the wallet's constant.
CHAIN="$CHAIN" HASH="$HASH" ROOT="$ROOT" CPS="${CHECKPOINTS[*]+"${CHECKPOINTS[*]}"}" node --input-type=module <<'JS'
import { existsSync, readFileSync, writeFileSync } from "node:fs";
const { CHAIN, HASH, ROOT, CPS } = process.env;
const fail = (m) => { console.error(`❌ ${m}`); process.exit(1); };
const edit = (path, f) => { const before = readFileSync(path, "utf8"); const after = f(before); if (after !== before) writeFileSync(path, after); return after !== before; };

// node/checkpoints/<chain>.json: the genesis hash never changes; checkpoints accumulate.
const file = `${ROOT}/node/checkpoints/${CHAIN}.json`;
const known = existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : { chain_id: CHAIN, genesis_hash: "", checkpoints: [] };
if (known.chain_id !== CHAIN) fail(`${file} is for ${known.chain_id}`);
if (known.genesis_hash && known.genesis_hash !== HASH) fail(`${file} already pins ${CHAIN} to genesis ${known.genesis_hash}: a chain id's genesis never changes (a relaunch takes a new chain id)`);
known.genesis_hash = HASH;
known.checkpoints ??= [];
for (const flag of (CPS ?? "").split(" ").filter(Boolean)) {
  const [height, hash, set_hash] = flag.split(":");
  const c = { height: Number(height), hash, set_hash };
  const at = known.checkpoints.find((x) => x.height === c.height);
  if (at && (at.hash !== c.hash || at.set_hash !== c.set_hash)) fail(`checkpoint #${c.height} conflicts with the one ${file} has`);
  if (!at) known.checkpoints.push(c);
}
known.checkpoints.sort((a, b) => a.height - b.height);
known.note ||= `${CHAIN}: pinned after its launch (scripts/pin-network.sh). Each release appends a recent checkpoint (docs/OPERATIONS.md, Checkpoints in releases).`;
const json = JSON.stringify({ chain_id: known.chain_id, genesis_hash: known.genesis_hash, checkpoints: known.checkpoints, note: known.note, ...(known.weak_subjectivity_blocks ? { weak_subjectivity_blocks: known.weak_subjectivity_blocks } : {}) }, null, 2) + "\n";
const wrote = !existsSync(file) || readFileSync(file, "utf8") !== json;
if (wrote) writeFileSync(file, json);
console.log(`✓ node/checkpoints/${CHAIN}.json${wrote ? "" : " (unchanged)"}: genesis ${HASH}, ${known.checkpoints.length} checkpoint(s)`);

const line = `include_str!("../checkpoints/${CHAIN}.json")`;
const rs = `${ROOT}/node/src/checkpoints.rs`;
const embedded = edit(rs, (s) => {
  const m = s.match(/const EMBEDDED: &\[&str\] = &\[([^\]]*)\];/);
  if (!m) fail(`no EMBEDDED table in ${rs}`);
  if (m[1].includes(line)) return s;
  const items = m[1].split(",").map((x) => x.trim()).filter(Boolean).concat(line);
  return s.replace(m[0], `const EMBEDDED: &[&str] = &[${items.join(", ")}];`);
});
console.log(`✓ node/src/checkpoints.rs EMBEDDED${embedded ? "" : " (unchanged)"}`);

const ts = `${ROOT}/mcp/src/network.ts`;
const mcp = edit(ts, (s) => {
  const m = s.match(/export const KNOWN_GENESIS: Record<string, string> = \{([^}]*)\};/);
  if (!m) fail(`no KNOWN_GENESIS table in ${ts}`);
  const entries = [...m[1].matchAll(/"([^"]+)":\s*"([0-9a-f]{64})"/g)].map(([, k, v]) => [k, v]);
  const have = entries.find(([k]) => k === CHAIN);
  if (have && have[1] !== HASH) fail(`${ts} already pins ${CHAIN} to ${have[1]}`);
  if (!have) entries.push([CHAIN, HASH]);
  const body = entries.length ? ` ${entries.map(([k, v]) => `"${k}": "${v}"`).join(", ")} ` : "";
  return s.replace(m[0], `export const KNOWN_GENESIS: Record<string, string> = {${body}};`);
});
console.log(`✓ mcp/src/network.ts KNOWN_GENESIS${mcp ? "" : " (unchanged)"}`);

const cfg = `${ROOT}/web/lib/config.ts`;
const testnet = readFileSync(cfg, "utf8").match(/export const TESTNET_CHAIN_ID = "([^"]+)";/)?.[1];
if (testnet === CHAIN) {
  const web = edit(cfg, (s) => {
    const m = s.match(/export const TESTNET_GENESIS_HASH: string \| undefined = (undefined|"([0-9a-f]{64})");/);
    if (!m) fail(`no TESTNET_GENESIS_HASH in ${cfg}`);
    if (m[2] && m[2] !== HASH) fail(`${cfg} already pins ${CHAIN} to ${m[2]}`);
    return s.replace(m[0], `export const TESTNET_GENESIS_HASH: string | undefined = "${HASH}";`);
  });
  console.log(`✓ web/lib/config.ts TESTNET_GENESIS_HASH${web ? "" : " (unchanged)"}`);
} else {
  console.log(`  (web/lib/config.ts: ${CHAIN} isn't the wallet's testnet ${testnet}; nothing to pin there)`);
}
JS
echo "Next: cargo test --locked --manifest-path node/Cargo.toml checkpoints cli_net; npm run build -w @blockula/sdk && npm test -w @blockula/mcp; commit, and release (deploy/README.md \"Relaunch\")."
