#!/usr/bin/env bash
# Pins a launched (non-mainnet) chain in the clients, for the release after its launch
# (deploy/README.md "Relaunch", OPERATIONS.md "Checkpoints in releases"): the node's embedded trust
# file and its line in EMBEDDED, the MCP's KNOWN_GENESIS and KNOWN_CLOCKS (the genesis clock that
# bounds the height each tx is signed against), and, for the public testnet, the wallet's built-in
# genesis hash and clock. With --genesis it also commits the genesis where the release ships it
# from (genesis/<chain id>/genesis.json, attached as genesis-<chain id>.json), with its public
# inputs (spec.toml, operators/) so anyone can rebuild it byte for byte. Idempotent: rerun it for
# the next release's checkpoint (the committed genesis/<chain id>/genesis.json then supplies the
# clock). The tests read these tables, so none of them needs editing.
#
#   scripts/pin-network.sh <chain id> <genesis hash> [--genesis <genesis.json>] [--checkpoint HEIGHT:HASH:SET_HASH:TIMESTAMP_MS]…
#
# e.g. after `RESET=1 deploy/launch.sh`:
#   scripts/pin-network.sh blockula-testnet-2 <hash> --genesis ~/.config/blockula/blockula-testnet-2/genesis.json \
#     --checkpoint "$(blockula checkpoint --peer https://blockula-testnet.fly.dev --genesis-hash <hash>)"
# Embedded checkpoints carry their block's time (the fourth field, which `blockula checkpoint`
# prints), so joiners measure a checkpoint's age from its own block (review-r3-sync-1).
# Nothing is written unless every input is good. ROOT (the checkout, default this one) and BIN
# (a blockula, to check the genesis against the hash) are for tests (scripts/test-pin-network.sh).
set -euo pipefail
ROOT=${ROOT:-$(cd "$(dirname "$0")/.." && pwd)}
BIN=${BIN:-$ROOT/node/target/release/blockula}
die() { echo "❌ $*" >&2; exit 1; }
FORM="HEIGHT:HASH:SET_HASH:TIMESTAMP_MS"

[ $# -ge 2 ] || die "usage: scripts/pin-network.sh <chain id> <genesis hash> [--genesis <genesis.json>] [--checkpoint $FORM]…"
CHAIN=$1 HASH=$(printf '%s' "$2" | tr 'A-F' 'a-f'); shift 2
GENESIS="" CHECKPOINTS=()
while [ $# -gt 0 ]; do
  case $1 in
    --genesis) GENESIS=${2:?--genesis needs a file}; shift 2 ;;
    --checkpoint) CHECKPOINTS+=("${2:?--checkpoint needs $FORM}"); shift 2 ;;
    *) die "unknown argument $1" ;;
  esac
done
[[ $CHAIN =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] || die "chain id $CHAIN: lowercase letters, digits and '-'"
[ "$CHAIN" != blockula-1 ] || die "blockula-1 is pinned by the genesis ceremony's release (docs/GENESIS_CEREMONY.md, section 6), not by this script"
[[ $HASH =~ ^[0-9a-f]{64}$ ]] || die "the genesis hash must be 64 hex digits"
for c in ${CHECKPOINTS[@]+"${CHECKPOINTS[@]}"}; do
  [[ $c =~ ^[0-9]+:[0-9a-f]{64}:[0-9a-f]{64}:[0-9]+$ ]] || die "checkpoint $c: embedded checkpoints are $FORM, with the validator set and the block's time (\`blockula checkpoint\` prints one)"
done

# The genesis the clock comes from: the one given, else the one an earlier pin committed.
SHIPPED=$ROOT/genesis/$CHAIN/genesis.json
CLOCK_FROM=${GENESIS:-}
GENESIS_CHECKPOINT=""
[ -n "$CLOCK_FROM" ] || [ ! -f "$SHIPPED" ] || CLOCK_FROM=$SHIPPED
if [ -n "$CLOCK_FROM" ]; then
  [ -f "$CLOCK_FROM" ] || die "$CLOCK_FROM: no such file"
  [ -x "$BIN" ] || die "checking $CLOCK_FROM needs a blockula to hash it ($BIN; build it, or set BIN)"
  got=$("$BIN" checkpoint --genesis "$CLOCK_FROM")
  [ "$got" = "$HASH" ] || die "$CLOCK_FROM hashes to $got, not $HASH"
  [ "$(G="$CLOCK_FROM" node -p 'JSON.parse(require("fs").readFileSync(process.env.G, "utf8")).chain_id')" = "$CHAIN" ] || die "$CLOCK_FROM is another chain's genesis"
  GENESIS_CHECKPOINT=$("$BIN" checkpoint --genesis "$CLOCK_FROM" --json)
fi

# Plan every file first (refusing on any conflict), then write them all.
CHAIN="$CHAIN" HASH="$HASH" ROOT="$ROOT" GENESIS="$GENESIS" CLOCK_FROM="$CLOCK_FROM" GENESIS_CHECKPOINT="$GENESIS_CHECKPOINT" CPS="${CHECKPOINTS[*]+"${CHECKPOINTS[*]}"}" node --input-type=module <<'JS'
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
const { CHAIN, HASH, ROOT, GENESIS, CLOCK_FROM, GENESIS_CHECKPOINT, CPS } = process.env;
const fail = (m) => { console.error(`❌ ${m}`); process.exit(1); };
const writes = new Map(); // path -> new content
const plan = (path, f) => { const before = writes.get(path) ?? readFileSync(path, "utf8"); const after = f(before); if (after !== before) writes.set(path, after); return after !== before; };
const log = [];

// The chain's genesis clock (created_at_ms, block_time_ms), from its genesis file.
let clock, genesisCheckpoint;
if (CLOCK_FROM) {
  const g = JSON.parse(readFileSync(CLOCK_FROM, "utf8"));
  const ok = (n, least) => Number.isSafeInteger(n) && n >= least;
  if (!ok(g.created_at_ms, 0) || !ok(g.block_time_ms, 1)) fail(`${CLOCK_FROM} has no created_at_ms / block_time_ms`);
  clock = { createdAtMs: g.created_at_ms, blockTimeMs: g.block_time_ms };
  genesisCheckpoint = JSON.parse(GENESIS_CHECKPOINT);
  if (genesisCheckpoint.height !== 0 || genesisCheckpoint.hash !== HASH || !/^[0-9a-f]{64}$/.test(genesisCheckpoint.set_hash) || genesisCheckpoint.timestamp_ms !== clock.createdAtMs) fail(`${CLOCK_FROM}: blockula returned an invalid genesis checkpoint (use a current binary)`);
}

// node/checkpoints/<chain>.json: the genesis hash never changes; checkpoints accumulate.
const file = `${ROOT}/node/checkpoints/${CHAIN}.json`;
const known = existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : { chain_id: CHAIN, genesis_hash: "", checkpoints: [] };
if (known.chain_id !== CHAIN) fail(`${file} is for ${known.chain_id}`);
if (known.genesis_hash && known.genesis_hash !== HASH) fail(`${file} already pins ${CHAIN} to genesis ${known.genesis_hash}: a chain id's genesis never changes (a relaunch takes a new chain id)`);
known.genesis_hash = HASH;
known.checkpoints ??= [];
for (const flag of (CPS ?? "").split(" ").filter(Boolean)) {
  const [height, hash, set_hash, time] = flag.split(":");
  const c = { height: Number(height), hash, set_hash, timestamp_ms: Number(time) };
  if (!Number.isSafeInteger(c.height) || !Number.isSafeInteger(c.timestamp_ms)) fail(`checkpoint ${flag}: height and time must be whole numbers`);
  const at = known.checkpoints.find((x) => x.height === c.height);
  if (at && (at.hash !== c.hash || at.set_hash !== c.set_hash || (at.timestamp_ms !== undefined && at.timestamp_ms !== c.timestamp_ms))) fail(`checkpoint #${c.height} conflicts with the one ${file} has`);
  if (at) at.timestamp_ms ??= c.timestamp_ms; // an older untimed entry learns its time
  else known.checkpoints.push(c);
}
known.checkpoints.sort((a, b) => a.height - b.height);
// Height zero is the genesis, not an arbitrary trusted block. Derive its set hash with the
// node's native implementation so these rules stay in step with checkpoints::Trust::new.
for (const c of known.checkpoints.filter((c) => c.height === 0)) {
  if (c.hash !== HASH) fail("checkpoint #0 must be the genesis hash");
  if (!genesisCheckpoint) fail("checkpoint #0 needs its genesis to check the validator set: pass --genesis <genesis.json>");
  if (c.set_hash !== genesisCheckpoint.set_hash) fail("checkpoint #0 must pin the genesis set");
  if (c.timestamp_ms !== undefined && c.timestamp_ms !== clock.createdAtMs) fail("checkpoint #0 must record created_at_ms");
}
// The node refuses times the chain can't have (checkpoints::Trust::check_times): blocks are at
// least a block time apart, from the genesis' created_at_ms on, and none is from the future.
if (clock) {
  let before = { height: 0, time: clock.createdAtMs };
  for (const c of known.checkpoints) {
    if (c.timestamp_ms === undefined) continue;
    const least = before.time + (c.height - before.height) * clock.blockTimeMs;
    if (c.timestamp_ms < least) fail(`checkpoint #${c.height}'s time ${c.timestamp_ms} is earlier than ${CHAIN} allows (${least}: blocks are at least ${clock.blockTimeMs} ms apart from #${before.height} at ${before.time}); the time is the block's timestamp_ms, in milliseconds`);
    if (c.timestamp_ms > Date.now() + 3_600_000) fail(`checkpoint #${c.height}'s time ${c.timestamp_ms} is in the future`);
    before = { height: c.height, time: c.timestamp_ms };
  }
}
known.note ||= `${CHAIN}: pinned after its launch (scripts/pin-network.sh). Each release appends a recent checkpoint (docs/OPERATIONS.md, Checkpoints in releases).`;
const json = JSON.stringify({ chain_id: known.chain_id, genesis_hash: known.genesis_hash, checkpoints: known.checkpoints, note: known.note, ...(known.weak_subjectivity_blocks ? { weak_subjectivity_blocks: known.weak_subjectivity_blocks } : {}) }, null, 2) + "\n";
const trustChanged = !existsSync(file) || readFileSync(file, "utf8") !== json;
if (trustChanged) writes.set(file, json);
log.push(`✓ node/checkpoints/${CHAIN}.json${trustChanged ? "" : " (unchanged)"}: genesis ${HASH}, ${known.checkpoints.length} checkpoint(s)`);

const line = `include_str!("../checkpoints/${CHAIN}.json")`;
const rs = `${ROOT}/node/src/checkpoints.rs`;
const embedded = plan(rs, (s) => {
  const m = s.match(/const EMBEDDED: &\[&str\] = &\[([^\]]*)\];/);
  if (!m) fail(`no EMBEDDED table in ${rs}`);
  if (m[1].includes(line)) return s;
  const items = m[1].split(",").map((x) => x.trim()).filter(Boolean).concat(line);
  return s.replace(m[0], `const EMBEDDED: &[&str] = &[${items.join(", ")}];`);
});
log.push(`✓ node/src/checkpoints.rs EMBEDDED${embedded ? "" : " (unchanged)"}`);

const ts = `${ROOT}/mcp/src/network.ts`;
const mcp = plan(ts, (s) => {
  const m = s.match(/export const KNOWN_GENESIS: Record<string, string> = \{([^}]*)\};/);
  if (!m) fail(`no KNOWN_GENESIS table in ${ts}`);
  const entries = [...m[1].matchAll(/"([^"]+)":\s*"([0-9a-f]{64})"/g)].map(([, k, v]) => [k, v]);
  const have = entries.find(([k]) => k === CHAIN);
  if (have && have[1] !== HASH) fail(`${ts} already pins ${CHAIN} to ${have[1]}`);
  if (!have) entries.push([CHAIN, HASH]);
  const body = entries.length ? ` ${entries.map(([k, v]) => `"${k}": "${v}"`).join(", ")} ` : "";
  return s.replace(m[0], `export const KNOWN_GENESIS: Record<string, string> = {${body}};`);
});
log.push(`✓ mcp/src/network.ts KNOWN_GENESIS${mcp ? "" : " (unchanged)"}`);
// The genesis clock goes with the hash (mcp/src/index.ts uses it only for that genesis).
const clocks = plan(ts, (s) => {
  const m = s.match(/export const KNOWN_CLOCKS: Record<string, GenesisClock> = \{((?:[^{}]|\{[^{}]*\})*)\};/);
  if (!m) fail(`no KNOWN_CLOCKS table in ${ts}`);
  const entries = [...m[1].matchAll(/"([^"]+)":\s*\{\s*createdAtMs:\s*(\d+),\s*blockTimeMs:\s*(\d+)\s*\}/g)].map(([, k, c, b]) => [k, { createdAtMs: Number(c), blockTimeMs: Number(b) }]);
  const have = entries.find(([k]) => k === CHAIN);
  if (have && clock && (have[1].createdAtMs !== clock.createdAtMs || have[1].blockTimeMs !== clock.blockTimeMs)) fail(`${ts} already has another genesis clock for ${CHAIN} (${JSON.stringify(have[1])})`);
  if (!have && !clock) fail(`${CHAIN} needs its genesis clock pinned with its hash (mcp/src/network.ts KNOWN_CLOCKS): pass --genesis <genesis.json>`);
  if (!have) entries.push([CHAIN, clock]);
  const body = entries.length ? ` ${entries.map(([k, v]) => `"${k}": { createdAtMs: ${v.createdAtMs}, blockTimeMs: ${v.blockTimeMs} }`).join(", ")} ` : "";
  return s.replace(m[0], `export const KNOWN_CLOCKS: Record<string, GenesisClock> = {${body}};`);
});
log.push(`✓ mcp/src/network.ts KNOWN_CLOCKS${clocks ? "" : " (unchanged)"}`);

const cfg = `${ROOT}/web/lib/config.ts`;
const testnet = readFileSync(cfg, "utf8").match(/export const TESTNET_CHAIN_ID = "([^"]+)";/)?.[1];
if (testnet === CHAIN) {
  const web = plan(cfg, (s) => {
    const m = s.match(/export const TESTNET_GENESIS_HASH: string \| undefined = (undefined|"([0-9a-f]{64})");/);
    if (!m) fail(`no TESTNET_GENESIS_HASH in ${cfg}`);
    if (m[2] && m[2] !== HASH) fail(`${cfg} already pins ${CHAIN} to ${m[2]}`);
    return s.replace(m[0], `export const TESTNET_GENESIS_HASH: string | undefined = "${HASH}";`);
  });
  log.push(`✓ web/lib/config.ts TESTNET_GENESIS_HASH${web ? "" : " (unchanged)"}`);
  const webClock = plan(cfg, (s) => {
    const m = s.match(/export const TESTNET_GENESIS_CLOCK: GenesisClock \| undefined = (undefined|\{ createdAtMs: (\d+), blockTimeMs: (\d+) \});/);
    if (!m) fail(`no TESTNET_GENESIS_CLOCK in ${cfg}`);
    const have = m[2] ? { createdAtMs: Number(m[2]), blockTimeMs: Number(m[3]) } : undefined;
    if (have && clock && (have.createdAtMs !== clock.createdAtMs || have.blockTimeMs !== clock.blockTimeMs)) fail(`${cfg} already has another genesis clock for ${CHAIN}`);
    const c = have ?? clock;
    if (!c) fail(`${CHAIN} needs its genesis clock pinned with its hash (web/lib/config.ts TESTNET_GENESIS_CLOCK): pass --genesis <genesis.json>`);
    return s.replace(m[0], `export const TESTNET_GENESIS_CLOCK: GenesisClock | undefined = { createdAtMs: ${c.createdAtMs}, blockTimeMs: ${c.blockTimeMs} };`);
  });
  log.push(`✓ web/lib/config.ts TESTNET_GENESIS_CLOCK${webClock ? "" : " (unchanged)"}`);
} else {
  log.push(`  (web/lib/config.ts: ${CHAIN} isn't the wallet's testnet ${testnet}; nothing to pin there)`);
}

// Everything checked: write. The genesis and its public inputs only (never the keys next to them).
if (GENESIS) {
  const dest = `${ROOT}/genesis/${CHAIN}`;
  const src = dirname(GENESIS);
  mkdirSync(dest, { recursive: true });
  if (resolve(GENESIS) !== resolve(`${dest}/genesis.json`)) copyFileSync(GENESIS, `${dest}/genesis.json`);
  let inputs = "";
  if (existsSync(`${src}/spec.toml`) && resolve(src) !== resolve(dest)) {
    copyFileSync(`${src}/spec.toml`, `${dest}/spec.toml`);
    if (existsSync(`${src}/operators`)) {
      mkdirSync(`${dest}/operators`, { recursive: true });
      for (const f of readdirSync(`${src}/operators`).filter((f) => f.endsWith(".toml"))) copyFileSync(`${src}/operators/${f}`, `${dest}/operators/${f}`);
    }
    inputs = ", spec.toml, operators/";
  }
  console.log(`✓ genesis/${CHAIN}/: genesis.json${inputs}`);
}
for (const [path, content] of writes) writeFileSync(path, content);
for (const l of log) console.log(l);
JS
echo "Next: cargo test --locked --manifest-path node/Cargo.toml checkpoints cli_net; npm run build -w @blockula/sdk && npm test -w @blockula/mcp; npm test -w web; commit, and release (deploy/README.md \"Relaunch\")."
