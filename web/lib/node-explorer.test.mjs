// The node's built-in explorer (node/src/explorer.html) shows program transactions and programs: what a
// program or its deployer chose (a method's, an argument's or an event's name, an abort message, what a call
// returned) is a stranger's text and must reach the page as text, never as markup. The page's one inline
// script runs here under node:vm with a stubbed document and node, its views called with real receipts
// (program-vectors.json) and with the same receipts and records made hostile: the markup they produce must
// have exactly the structure the honest ones do, and none of the hostile text may appear unescaped.
// Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const read = (rel) => readFileSync(new URL(rel, import.meta.url), "utf8");
const html = read("../../node/src/explorer.html");
const script = html.split("<script>")[1].split("</script>")[0];
const { vectors } = JSON.parse(read("./program-vectors.json"));
const fixtures = JSON.parse(read("../../sdk-ts/test/program-fixtures.json"));

/** The explorer's script, loaded with `answers` as the node: a path to its JSON (or `[status, json]`). Returns its views. */
function explorer(answers) {
  const el = () => ({ innerHTML: "", textContent: "", value: "", addEventListener() {}, toggleAttribute() {}, append() {} });
  const context = vm.createContext({
    document: { head: { append() {} }, createElement: () => ({}), querySelector: el, querySelectorAll: () => [], visibilityState: "visible" },
    location: { hash: "#/nothing/here" },
    window: { addEventListener() {} },
    fetch: async (url) => {
      const path = String(url).replace(/^\/api/, "").split("?")[0];
      const [status, json] = Array.isArray(answers[path]) && typeof answers[path][0] === "number" ? answers[path] : path in answers ? [200, answers[path]] : [404, { error: `no route ${path}` }];
      return { ok: status < 400, status, json: async () => json };
    },
    setInterval: () => 0,
    console,
    TextEncoder,
  });
  vm.runInContext(script, context);
  return vm.runInContext("views", context);
}

/** The element names and attribute names of some markup, in order: its structure, with every text left out. */
const structure = (markup) => [...markup.matchAll(/<\/?([a-zA-Z][a-zA-Z0-9]*)((?:\s+[a-zA-Z-]+(?:="[^"]*")?)*)\s*\/?>/g)].map((m) => `${m[1]}${[...m[2].matchAll(/\s+([a-zA-Z-]+)/g)].map((a) => `[${a[1]}]`).join("")}`);
const HOSTILE = ['<script>window.__pwned=1</script>', '"><img src=x onerror=window.__pwned=2>', "'><svg onload=window.__pwned=3>", "</code><b>x</b>", "javascript:window.__pwned=4", "a\u202eb\u0007\u200bc"];
/** No element made of hostile text (an escaped `&lt;img …&gt;` is text and fine), and none of the characters that hide or reorder text. */
const bad = (markup) => assert.ok(!/<script|<img|<svg|<b>x/i.test(markup) && !/[\u202e\u0007\u200b]/.test(markup), `hostile text reached the page as markup: ${markup.match(/.{0,40}(<script|<img|<svg|<b>x).{0,40}/i)?.[0]}`);

test("the page's script has no inline handlers, and its program views are its own", () => {
  assert.equal([...html.matchAll(/<script/g)].length, 1, "one inline script, the one the CSP hashes");
  const views = explorer({});
  for (const v of ["tx", "program", "code"]) assert.equal(typeof views[v], "function", `views.${v}`);
});

test("a program transaction's receipt: facts, then claims, each program-chosen name in a box of its own", async () => {
  const honest = vectors.find((v) => v.name === "a call that writes").tx;
  const views = explorer({ [`/tx/${honest.id}`]: honest });
  const page = await views.tx(honest.id);
  assert.match(page, /What the program did/);
  assert.match(page, /It applied\./);
  assert.match(page, /What the program reports/);
  assert.match(page, /<code class="named">increment<\/code> returned/);
  assert.match(page, /checks none of this/);
  assert.match(page, /milli-drops each/);
  assert.ok(page.indexOf("What the program did") < page.indexOf("What the program reports"), "facts come before the claims");
  // A pending program transaction has no receipt yet; a failed one says how, in words that quote nothing the program wrote.
  const pending = await explorer({ [`/tx/${honest.id}`]: { ...honest, status: "pending", height: null, program: null } }).tx(honest.id);
  assert.match(pending, /It hasn't run yet/);
  const aborted = vectors.find((v) => v.name === "a call that aborts with a message").tx;
  const failed = await explorer({ [`/tx/${aborted.id}`]: aborted }).tx(aborted.id);
  assert.match(failed, /aborted with code 22\./);
  assert.match(failed, /Its fee was charged and nothing else happened\./);
  assert.match(failed, /aborted with the message <code class="named">no deal<\/code>/);
  // The raw action is cut: a program's code in hex isn't dumped on the page.
  const upload = vectors.find((v) => v.name === "upload").tx;
  const raw = await explorer({ [`/tx/${upload.id}`]: upload }).tx(upload.id);
  assert.match(raw, /\(\d[\d,]* characters\)/);
  assert.ok(raw.length < 20_000, `the page is ${raw.length} characters`);
  // What the programs' own sends burned is a fact of the receipt, shown only when there was a burn (the payout opened an account).
  const payout = vectors.find((v) => v.name === "a payout to a new account").tx;
  const paid = await explorer({ [`/tx/${payout.id}`]: payout }).tx(payout.id);
  assert.match(paid, /<dt>Burned by its programs<\/dt><dd>0\.1 FANG <span class="dim">= the creation fee of an account a program's own send opened/);
  assert.doesNotMatch(await explorer({ [`/tx/${honest.id}`]: honest }).tx(honest.id), /Burned by its programs/);
});

test("a v1 action's raw data is shown exactly as before: an added key's whole public key and proof, a long link", async () => {
  const hex = (n, seed) => Array.from({ length: n }, (_, i) => ((i * seed + 3) % 256).toString(16).padStart(2, "0")).join("");
  const addKey = { id: "e1".repeat(32), kind: "add_key", status: "finalized", height: 12, summary: "vlad added a key", from: { address: "boo1v", name: "vlad.boo", short: "boo1v" }, to: null, fee_display: "0.0001 FANG", nonce: 3, signature_scheme: "ML-DSA-65", size_bytes: 5500,
    action: { type: "add_key", key_id: "ab".repeat(32), label: "phone", role: { type: "owner" }, public_key: hex(1952, 7), pop: hex(3309, 11) } };
  const page = await explorer({ [`/tx/${addKey.id}`]: addKey }).tx(addKey.id);
  const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  assert.ok(page.includes(esc(JSON.stringify(addKey.action, null, 2))), "the raw data is JSON.stringify(action, null, 2), whole");
  assert.ok(page.includes(addKey.action.public_key) && page.includes(addKey.action.pop));
  assert.doesNotMatch(page, /\(\d[\d,]* characters\)/);
  const propose = { ...addKey, id: "e2".repeat(32), kind: "propose", action: { type: "propose", kind: { type: "param_change", key: "x", value: "1" }, title: "t", url: "https://example.org/" + "a".repeat(200), deposit: "10" } };
  assert.ok((await explorer({ [`/tx/${propose.id}`]: propose }).tx(propose.id)).includes("https://example.org/" + "a".repeat(200)), "a long link is whole");
  // Exactly as before: a hidden character in a v1 string is as JSON.stringify writes it, and a program action's long non-blob field is whole.
  const bidi = { ...addKey, id: "e3".repeat(32), kind: "propose", action: { type: "propose", kind: { type: "param_change", key: "x", value: "1" }, title: "a\u202eb", url: "", deposit: "10" } };
  assert.ok((await explorer({ [`/tx/${bidi.id}`]: bidi }).tx(bidi.id)).includes(esc(JSON.stringify(bidi.action, null, 2))), "no \\uXXXX escape in a v1 action");
  const create = { ...addKey, id: "e4".repeat(32), kind: "create_program", action: { type: "create_program", wasm_hash: "00".repeat(32), interface_hash: "11".repeat(32), authority: "x".repeat(300), upgrade_delay_blocks: 0, policy: "open", accepts_transfers: false, init: null, fuel_limit: 0, storage_limit: "0", bindings: [] }, program: null };
  assert.ok((await explorer({ [`/tx/${create.id}`]: create }).tx(create.id)).includes("x".repeat(300)), "only the blobs of a program action are cut");
});

test("hostile names and messages in a receipt show as text: the markup has exactly the honest structure", async () => {
  for (const base of ["a call that writes", "a call that aborts with a message", "create", "upload"]) {
    const honest = structuredClone(vectors.find((v) => v.name === base).tx);
    const claims = honest.program.claims;
    const hostile = structuredClone(honest);
    HOSTILE.forEach((h, i) => {
      for (const r of hostile.program.claims.return_data) r.untrusted_method = h;
      if (i === 0) hostile.program.claims.events = [{ program: honest.from.address, untrusted_name: h, data: "ab" }];
      if (hostile.program.outcome.untrusted_message !== undefined) hostile.program.outcome.untrusted_message = h;
    });
    claims.events = [{ program: honest.from.address, untrusted_name: "emitted", data: "ab" }];
    for (const h of HOSTILE) {
      const t = structuredClone(hostile);
      for (const r of t.program.claims.return_data) r.untrusted_method = h;
      t.program.claims.events = [{ program: honest.from.address, untrusted_name: h, data: "ab" }];
      if (t.program.outcome.untrusted_message !== undefined) t.program.outcome.untrusted_message = h;
      t.action = { ...t.action, calls: [{ program: honest.from.address, method: h, input: "", value: "0", version_digest: "00".repeat(32) }] };
      const page = await explorer({ [`/tx/${t.id}`]: t }).tx(t.id);
      bad(page);
      const same = structuredClone(honest);
      for (const r of same.program.claims.return_data) r.untrusted_method = "get";
      same.program.claims.events = [{ program: honest.from.address, untrusted_name: "emitted", data: "ab" }];
      if (same.program.outcome.untrusted_message !== undefined) same.program.outcome.untrusted_message = "no deal";
      same.action = { ...same.action, calls: [{ program: honest.from.address, method: "get", input: "", value: "0", version_digest: "00".repeat(32) }] };
      const reference = await explorer({ [`/tx/${same.id}`]: same }).tx(same.id);
      assert.deepEqual(structure(page), structure(reference), `${base}: hostile text changed the page's structure (${JSON.stringify(h)})`);
    }
  }
});

/** Every link to an account, program or code is to something shaped like one (or a .boo name): a node's junk is never a link. */
const linksAreWellFormed = (markup) => [...markup.matchAll(/href="#\/(account|program|code)\/([^"]*)"/g)].every(([, , to]) => /^(t?boo1[02-9ac-hj-np-z]{40,}|[0-9a-f]{64}|[a-z0-9-]{3,32}\.boo)$/.test(to));

test("a malformed receipt (amounts that aren't numbers, a node's junk) is text, never a crash or markup", async () => {
  const honest = structuredClone(vectors.find((v) => v.name === "a payout to a new account").tx);
  const junk = structuredClone(honest);
  junk.program.facts.balance_changes = [{ account: HOSTILE[0], change: HOSTILE[1] }, { account: "boo1x", change: null }];
  junk.program.facts.fee = { bytes: HOSTILE[1], fuel: "1e9", tip: -5 };
  junk.program.facts.outside_vm = { record_deposit: HOSTILE[0], upload_deposit: {}, account_creation_fee: [], total: 7 };
  junk.program.facts.created = HOSTILE[1];
  junk.program.facts.fuel = { reserved: HOSTILE[2], limit: NaN, load: null, used: "x", rate: {} };
  junk.program.claims = { return_data: [{ program: HOSTILE[0], untrusted_method: HOSTILE[1], value: HOSTILE[2], data: HOSTILE[3] }], events: "not a list", omitted: { events: HOSTILE[0], return_bytes: HOSTILE[1] } };
  junk.program.outcome = { ok: false, code: HOSTILE[0], trap: { name: HOSTILE[1], number: HOSTILE[2] }, rule: HOSTILE[3] };
  const page = await explorer({ [`/tx/${junk.id}`]: junk }).tx(junk.id);
  bad(page);
  assert.ok(linksAreWellFormed(page), "a node's junk where an address goes isn't linked");
  assert.ok(linksAreWellFormed(await explorer({ [`/tx/${honest.id}`]: honest }).tx(honest.id)) && /href="#\/account\/boo1/.test(await explorer({ [`/tx/${honest.id}`]: honest }).tx(honest.id)), "and a real address is");
});

test("a deployed program's page: its record, its methods named by the deployer, hostile names as text", async () => {
  const p = fixtures.counter.program;
  const views = explorer({ [`/program/${p.address}`]: p, [`/account/${p.address}`]: { account: { address: p.address, name: null, short: "x" }, recent_txs: [] } });
  const page = await views.program(p.address);
  assert.match(page, /Immutable: no account can ever change this program's code\./);
  assert.match(page, /<code class="named">increment<\/code>\(\)/);
  assert.match(page, /view: a read, nothing signed/);
  assert.match(page, /call: a transaction runs it/);
  assert.ok(page.includes(p.version_digest));
  assert.match(page, /This page signs nothing/);
  const hostile = structuredClone(p);
  hostile.methods = HOSTILE.map((h, i) => ({ untrusted_name: h, selector: "00".repeat(8), kind: i % 2 ? "view" : "call", payable: i % 3 === 0, args: [{ untrusted_name: HOSTILE[(i + 1) % HOSTILE.length], type: { vec: [{ option: h }, h] } }], returns: h }));
  hostile.creator = { address: p.creator.address, name: HOSTILE[0], short: HOSTILE[1] };
  hostile.authority = { address: HOSTILE[2], name: HOSTILE[3], short: HOSTILE[0] };
  hostile.code = { wasm_hash: HOSTILE[1], size: HOSTILE[0], interface_hash: HOSTILE[2] };
  hostile.version_digest = HOSTILE[3];
  hostile.upgradability = HOSTILE[0];
  const out = await explorer({ [`/program/${p.address}`]: hostile }).program(p.address);
  bad(out);
  // A name is no address.
  await assert.rejects(views.program("alice.boo"), /never by a \.boo name/);
  await assert.rejects(views.program(HOSTILE[0]), /never by a \.boo name/);
});

test("a stored code's page: its exports as the deployer's names", async () => {
  const c = fixtures.counter.code;
  const page = await explorer({ [`/code/${c.wasm_hash}`]: c }).code(c.wasm_hash);
  assert.match(page, /<code class="named">get<\/code> <code class="named">increment<\/code>/);
  const hostile = { ...c, untrusted_exports: HOSTILE, uploader: { address: c.uploader.address, name: HOSTILE[0], short: HOSTILE[1] }, deposit: HOSTILE[1], refs: HOSTILE[2], size: HOSTILE[3] };
  bad(await explorer({ [`/code/${c.wasm_hash}`]: hostile }).code(c.wasm_hash));
  await assert.rejects(explorer({}).code("not a hash"), /64-character hash/);
});
