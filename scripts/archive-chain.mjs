// Keep a chain's history after its nodes prune it: copy every finalized block, with the
// finalization certificates that prove them, from a node's `/api/sync` to local files. The
// public testnet node keeps 14 days of blocks and validators about 2, so without a copy a
// chain's early history is gone for good, and with it any replay of that chain from genesis
// (the programs replay gate needs blockula-testnet-2's v1 history; docs/PROGRAMS-PLAN.md, P0).
//
// Each run resumes where the last one stopped and returns once it has caught up, so it's safe
// to run on a schedule. Batches are kept exactly as the node served them: the blocks (hex, their
// consensus encoding) and the certificate of the last one, one JSON line per batch, in files of
// about 100,000 heights that are gzipped once full:
//
//   <out>/<chain id>/meta.json                   chain id, genesis hash, closed files + sha256
//   <out>/<chain id>/genesis.json                the node's /api/genesis
//   <out>/<chain id>/sync-<first height>.jsonl   the open file; closed ones end in .jsonl.gz
//
// It checks shape and continuity (each block's height), not signatures: a node checks those
// when it replays the blocks. It refuses a node that serves another chain than the one in <out>.
//
//   node scripts/archive-chain.mjs [rpc=https://blockula-testnet.fly.dev] [out=~/.blockula/archive]
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";

const rpc = (process.argv[2] || "https://blockula-testnet.fly.dev").replace(/\/+$/, "");
const root = process.argv[3] || path.join(os.homedir(), ".blockula", "archive");
const SEGMENT_HEIGHTS = 100_000;
const LIMIT = 500; // the most blocks `/api/sync` serves at once
const HEX = /^(?:[0-9a-f]{2})+$/;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...a) => console.log(new Date().toISOString(), ...a);

async function get(p, tries = 6) {
  for (let i = 1; ; i++) {
    let r;
    try {
      r = await fetch(rpc + p, { signal: AbortSignal.timeout(120_000), headers: { "user-agent": "blockula-archive-chain" } });
    } catch (e) {
      if (i >= tries) throw new Error(`${p}: ${e.message}`);
      await sleep(2 ** i * 1000);
      continue;
    }
    if (r.ok) return r.json();
    const body = (await r.text()).slice(0, 300);
    if (r.status === 410) throw new Error(`${rpc} no longer holds ${p} (pruned): ${body}. Archive from a node that does.`);
    if ((r.status === 429 || r.status >= 500) && i < tries) {
      await sleep((Number(r.headers.get("retry-after")) || 2 ** i) * 1000);
      continue;
    }
    throw new Error(`${p}: HTTP ${r.status}: ${body}`);
  }
}

function writeAtomic(file, data) {
  const tmp = `${file}.tmp`;
  const fd = fs.openSync(tmp, "w", 0o600);
  fs.writeSync(fd, data);
  fs.fsyncSync(fd);
  fs.closeSync(fd);
  fs.renameSync(tmp, file);
}

// The last complete batch in the open file. A line torn by a crash is cut off.
function lastBatch(file) {
  if (!fs.existsSync(file)) return null;
  const buf = fs.readFileSync(file);
  let end = buf.length;
  if (end && buf[end - 1] !== 0x0a) {
    end = buf.lastIndexOf(0x0a) + 1;
    fs.truncateSync(file, end);
    log(`cut a torn last line from ${path.basename(file)}`);
  }
  if (!end) return null;
  return JSON.parse(buf.subarray(buf.lastIndexOf(0x0a, end - 2) + 1, end - 1).toString());
}

// CBlock v2 starts version u32 ‖ parent [32] ‖ height u64, big-endian (node/src/consensus.rs).
const heightOf = (hex) => BigInt(`0x${hex.slice(72, 88)}`);

function check(b, from) {
  if (!Array.isArray(b.blocks) || !b.blocks.length) throw new Error(`empty batch at ${from}`);
  if (b.to !== from + b.blocks.length - 1) throw new Error(`batch at ${from}: ${b.blocks.length} blocks but to = ${b.to}`);
  if (typeof b.finalization !== "string" || !HEX.test(b.finalization)) throw new Error(`batch ${from}..${b.to} has no certificate`);
  b.blocks.forEach((hex, i) => {
    if (typeof hex !== "string" || hex.length < 88 || !HEX.test(hex)) throw new Error(`block ${from + i} isn't a hex block`);
    if (heightOf(hex) !== BigInt(from + i)) throw new Error(`block ${from + i} says height ${heightOf(hex)}`);
  });
}

const status = await get("/api/status");
const { chain_id, genesis_hash } = status;
if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(chain_id ?? "")) throw new Error(`unexpected chain id ${JSON.stringify(chain_id)}`);
const dir = path.join(root, chain_id);
fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
const metaFile = path.join(dir, "meta.json");
const meta = fs.existsSync(metaFile)
  ? JSON.parse(fs.readFileSync(metaFile, "utf8"))
  : { chain_id, genesis_hash, source: rpc, segments: [], open: null };
if (meta.chain_id !== chain_id || meta.genesis_hash !== genesis_hash) {
  throw new Error(`${rpc} serves ${chain_id} (genesis ${genesis_hash}), but ${dir} holds ${meta.chain_id} (genesis ${meta.genesis_hash})`);
}
const saveMeta = () => writeAtomic(metaFile, `${JSON.stringify(meta, null, 1)}\n`);
saveMeta();
const genesisFile = path.join(dir, "genesis.json");
if (!fs.existsSync(genesisFile)) writeAtomic(genesisFile, `${JSON.stringify(await get("/api/genesis"), null, 1)}\n`);

let next = (meta.segments.at(-1)?.to ?? 0) + 1;
if (meta.open) next = (lastBatch(path.join(dir, meta.open.file))?.to ?? next - 1) + 1;
if (next < (status.history_from ?? 1)) {
  throw new Error(`${rpc} holds heights from ${status.history_from}, but this archive needs ${next}: heights ${next}..${status.history_from - 1} are gone from it`);
}
log(`${chain_id}: archiving from height ${next} (tip ${status.height}) into ${dir}`);

function closeOpen(to) {
  const src = path.join(dir, meta.open.file);
  const gz = gzipSync(fs.readFileSync(src), { level: 9 });
  writeAtomic(`${src}.gz`, gz);
  meta.segments.push({ file: `${meta.open.file}.gz`, from: meta.open.from, to, sha256: createHash("sha256").update(gz).digest("hex") });
  meta.open = null;
  saveMeta();
  fs.unlinkSync(src);
}

let fd = null;
let batches = 0;
const start = next;
for (;;) {
  const b = await get(`/api/sync?from=${next}&limit=${LIMIT}`);
  if (!b.blocks?.length) break;
  check(b, next);
  if (meta.open && next >= meta.open.from + SEGMENT_HEIGHTS) {
    if (fd !== null) {
      fs.closeSync(fd);
      fd = null;
    }
    closeOpen(next - 1);
  }
  if (!meta.open) {
    meta.open = { file: `sync-${String(next).padStart(12, "0")}.jsonl`, from: next };
    saveMeta();
  }
  if (fd === null) fd = fs.openSync(path.join(dir, meta.open.file), "a", 0o600);
  fs.writeSync(fd, `${JSON.stringify({ from: next, to: b.to, blocks: b.blocks, finalization: b.finalization })}\n`);
  fs.fsyncSync(fd);
  next = b.to + 1;
  if (++batches % 50 === 0) log(`… through ${b.to} of ${b.tip}`);
  if (next > b.tip) break;
}
if (fd !== null) fs.closeSync(fd);
log(`${chain_id}: ${next - start} blocks in ${batches} batches; archived through ${next - 1}`);
