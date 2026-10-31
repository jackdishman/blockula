// Compares a fresh run of scripts/explorer-vectors.mjs with the committed web/lib/v1-vectors.json, after
// normalising what is random in every run (wallets' addresses, names' random tags, heights and amounts that
// come from the faucet, ids): what must be identical is what a v1 client depends on. Per vector, the
// refusal text (if the node refused it), the sealed result's status, kind, fee, tip, max fee, error and
// summary (and which keys it has), the balance change of each account the transaction touched, the key
// paths of every read the wallet's preflight takes, the action's type and the size of the signed
// transaction (its fee). The file itself is regenerated only when a v1 answer changes (WBS S7b).
//
//   blockula dev --programs --port 7340 &                       # a chain with programs on: v1 must read the same
//   node scripts/explorer-vectors.mjs http://localhost:7340 > /tmp/v1-fresh.json
//   node scripts/vdiff.mjs web/lib/v1-vectors.json /tmp/v1-fresh.json
//
// Prints the number of vectors and of differences (each one listed); exits 1 when there is any.
import { readFileSync } from "node:fs";

const [oldPath, newPath] = process.argv.slice(2);
if (!oldPath || !newPath) {
  console.error("usage: node scripts/vdiff.mjs <committed v1-vectors.json> <fresh run's output>");
  process.exit(2);
}
const oldV = JSON.parse(readFileSync(oldPath, "utf8"));
const newV = JSON.parse(readFileSync(newPath, "utf8"));

/** A string with every random identifier and number replaced, so two runs of the same flow read the same. */
function norm(s) {
  return String(s)
    .replace(/\bt?boo1[a-z0-9]{50,}\b/g, "ADDR")
    .replace(/\bboo1[a-z0-9]{6}…[a-z0-9]{6}\b/g, "SHORT")
    .replace(/\b[0-9a-f]{64}\b/g, "HEX")
    .replace(/\b(vlad|mina|lucy|tomb|crypt|voter|coven)[a-z0-9]{5}\b/g, "$1TAG")
    .replace(/validators [a-z0-9]{5}/g, "validators TAG")
    .replace(/\d+(\.\d+)?/g, "N");
}

/** The key paths of a JSON value, lists as `[]`: its shape without its values. */
function paths(v, prefix = "", out = new Set()) {
  if (v !== null && typeof v === "object" && !Array.isArray(v)) for (const [k, x] of Object.entries(v)) paths(x, `${prefix}.${k}`, out);
  else if (Array.isArray(v)) {
    out.add(`${prefix}[]`);
    for (const x of v) paths(x, `${prefix}[]`, out);
  } else out.add(prefix);
  return out;
}

/** What each account's balance did, in the order the vector lists them. */
const delta = (v) => (v.before && v.after ? Object.values(v.after).map((a, i) => (BigInt(a) - BigInt(Object.values(v.before)[i])).toString()) : null);
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const only = (a, b) => [...a].filter((x) => !b.has(x)).sort().slice(0, 6);

const diffs = [];
const namesOld = oldV.vectors.map((v) => v.name);
const namesNew = newV.vectors.map((v) => v.name);
if (!same(namesOld, namesNew)) diffs.push(["names", namesOld, namesNew]);
console.log(`vectors: ${oldV.vectors.length} ${newV.vectors.length}`);
oldV.vectors.forEach((a, i) => {
  const b = newV.vectors[i];
  if (!b) return;
  const n = a.name;
  if (norm(a.refused ?? "") !== norm(b.refused ?? "")) diffs.push([n, "refused", a.refused, b.refused]);
  const [ra, rb] = [a.result, b.result];
  if (!ra !== !rb) diffs.push([n, "result present", !!ra, !!rb]);
  if (ra && rb) {
    for (const k of ["status", "kind", "fee", "tip", "max_fee", "error"]) if (norm(ra[k] ?? null) !== norm(rb[k] ?? null)) diffs.push([n, `result.${k}`, ra[k], rb[k]]);
    if (norm(ra.summary ?? "") !== norm(rb.summary ?? "")) diffs.push([n, "summary", ra.summary, rb.summary]);
    const [ka, kb] = [Object.keys(ra), Object.keys(rb)];
    const keyDiff = [...ka.filter((k) => !kb.includes(k)), ...kb.filter((k) => !ka.includes(k))].sort();
    if (keyDiff.length) diffs.push([n, "result keys", keyDiff]);
  }
  if (!same(delta(a), delta(b))) diffs.push([n, "balance deltas", delta(a), delta(b)]);
  const [pa, pb] = [paths(a.reads ?? {}), paths(b.reads ?? {})];
  if (!same([...pa].sort(), [...pb].sort())) diffs.push([n, "reads key paths: only in old", only(pa, pb), "only in new", only(pb, pa)]);
  if (a.tx?.action?.type !== b.tx?.action?.type) diffs.push([n, "action type"]);
  // The same fee for the same action means the same encoding length.
  if (a.tx?.fee !== b.tx?.fee) diffs.push([n, "signed fee", a.tx?.fee, b.tx?.fee]);
});
console.log(`${diffs.length} differences`);
for (const d of diffs) console.log(JSON.stringify(d));
process.exit(diffs.length ? 1 : 0);
