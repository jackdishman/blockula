// The chain directory's rules (lib/chains.ts, chains/README.md) and its checker
// (scripts/check-chains.mjs): what a listing must look like, what's refused, and the links the
// site and `blockula launch` build from one. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { checkChain, checkChains, contactUrl, listingJson, listingLink, publicNameProblem, soundsOfficial, units } from "./chains.ts";

const GOOD = {
  name: "Jack in the Block",
  chain_id: "jack-in-the-block",
  token: "JACK",
  description: "Pizza debts for a few friends.",
  rpc: "https://chain.example.org",
  genesis_hash: "ab".repeat(32),
  validators_wanted: true,
  p2p: "chain.example.org:30300",
  contact: "jackdishman",
  listed_at: "2026-11-07",
};
const FILE = "jack-in-the-block.json";
const errs = (patch, file = FILE) => checkChain(file, { ...GOOD, ...patch });

test("a good listing, and the optional fields", () => {
  assert.deepEqual(checkChain(FILE, GOOD), []);
  const { p2p, listed_at, ...bare } = GOOD;
  assert.deepEqual(checkChain(FILE, { ...bare, validators_wanted: false }), []);
  assert.deepEqual(errs({ rpc: "https://box.tail1234.ts.net", validators_wanted: false, p2p: undefined }).filter(Boolean), [], "a Funnel host is public");
  assert.deepEqual(errs({ rpc: "https://chain.example.org:8443" }), []);
  assert.deepEqual(errs({ p2p: "203.0.113.10:30300" }), []);
  assert.deepEqual(errs({ contact: "https://forum.example.org/u/jack" }), []);
});

test("refuses bad shapes", () => {
  assert.deepEqual(checkChain(FILE, []), ["isn't a JSON object"]);
  assert.deepEqual(checkChain(FILE, null), ["isn't a JSON object"]);
  for (const [patch, says] of [
    [{ extra: 1 }, /doesn't take: extra/],
    [{ name: undefined }, /missing name|name must be a string/],
    [{ name: "x".repeat(49) }, /more than 48/],
    [{ name: " Jack" }, /space around/],
    [{ name: "Jack‮Block" }, /bidi/],
    [{ name: "Jack​Block" }, /zero-width/],
    [{ name: "<b>Jack</b>" }, /< or >/],
    [{ description: "x".repeat(161) }, /more than 160/],
    [{ description: "" }, /description is empty/],
    [{ token: "jack" }, /token must be/],
    [{ token: "J" }, /token must be/],
    [{ token: "JACKJACKJ" }, /token must be/],
    [{ chain_id: "Jack" }, /chain_id must be/],
    [{ chain_id: "jack--block" }, /chain_id must be/],
    [{ chain_id: "other-chain" }, /named other-chain\.json/],
    [{ genesis_hash: "AB".repeat(32) }, /genesis_hash/],
    [{ genesis_hash: "ab".repeat(31) }, /genesis_hash/],
    [{ validators_wanted: "yes" }, /true or false/],
    [{ listed_at: "2026-13-01" }, /listed_at/],
    [{ listed_at: "yesterday" }, /listed_at/],
    [{ contact: "not a handle!" }, /contact/],
    [{ contact: "http://example.org" }, /contact/],
    [{ contact: "-jack" }, /contact/],
  ]) assert.ok(errs(patch).some((e) => says.test(e)), `${JSON.stringify(patch)}: ${errs(patch).join("; ")}`);
});

test("rpc: public https origins only", () => {
  for (const [rpc, says] of [
    ["http://chain.example.org", /https/],
    ["https://chain.example.org/", /origin alone/],
    ["https://chain.example.org/wallet", /origin alone/],
    ["https://chain.example.org?x=1", /origin alone/],
    ["https://user:pw@chain.example.org", /credentials/],
    ["https://203.0.113.10", /IP address/],
    ["https://[2001:db8::1]", /IPv6/],
    ["https://localhost", /DNS name/],
    ["https://box.local", /local or reserved/],
    ["https://nas.lan", /local or reserved/],
    ["https://node.internal", /local or reserved/],
    ["https://router.home.arpa", /local or reserved/],
    ["https://chain.test", /local or reserved/],
    ["https://blockula.app", /Blockula's own/],
    ["https://demo.blockula.app", /Blockula's own/],
    ["https://bl0ckula-wallet.example.org", /Blockula's own/],
    ["not a url", /isn't a URL/],
    [`https://${"a".repeat(100)}.org`, /at most 100/],
  ]) assert.ok(errs({ rpc }).some((e) => says.test(e)), `${rpc}: ${errs({ rpc }).join("; ")}`);
});

test("p2p: needed to take validators, and reachable from the internet", () => {
  assert.ok(errs({ p2p: undefined }).some((e) => /validators_wanted needs p2p/.test(e)));
  for (const p2p of ["chain.example.org", "chain.example.org:0", "chain.example.org:70000", "10.0.0.5:30300", "192.168.1.2:30300", "100.101.2.3:30300", "127.0.0.1:30300", "box.tail1234.ts.net:30300", "box.local:30300", "[::1]:30300"])
    assert.ok(errs({ p2p }).some((e) => /p2p/.test(e)), p2p);
});

test("names that read as Blockula itself, or as official", () => {
  for (const s of ["Blockula", "blockula testnet", "BL0CKULA", "Block-Ula", "Blockulá", "The Official Chain"]) assert.ok(soundsOfficial(s), s);
  for (const s of ["Jack in the Block", "Pizza Ledger", "Unofficially ours", "Block party", "Dracula's crypt"]) assert.ok(!soundsOfficial(s), s);
  assert.ok(errs({ name: "Blockula Mainnet" }).some((e) => /Blockula itself/.test(e)));
  assert.ok(errs({ chain_id: "blockula-2" }, "blockula-2.json").some((e) => /Blockula's own/.test(e)));
  assert.ok(errs({ description: "The official chain." }).some((e) => /official/.test(e)));
});

test("public names", () => {
  assert.equal(publicNameProblem("chain.example.org"), null);
  assert.equal(publicNameProblem("box.tail1234.ts.net"), null);
  for (const h of ["localhost", "single", "box.local", "10.0.0.1", "[::1]", "-bad.example.org"]) assert.ok(publicNameProblem(h), h);
});

test("the directory: duplicates, stray files, sorted by name", () => {
  const f = (name, patch = {}) => ({ name, text: JSON.stringify({ ...GOOD, ...patch }) });
  const other = { name: "Other Chain", chain_id: "other-chain", rpc: "https://other.example.org", genesis_hash: "cd".repeat(32), p2p: "other.example.org:30300" };
  const ok = checkChains([f("other-chain.json", other), f(FILE), { name: "README.md", text: "# hi" }]);
  assert.deepEqual(ok.errors, []);
  assert.deepEqual(ok.chains.map((c) => c.chain_id), ["jack-in-the-block", "other-chain"]);
  const dup = checkChains([f(FILE), f("other-chain.json", { ...other, genesis_hash: GOOD.genesis_hash, rpc: GOOD.rpc })]);
  assert.ok(dup.errors.some((e) => /same genesis_hash/.test(e)) && dup.errors.some((e) => /same rpc/.test(e)), dup.errors.join("; "));
  const name = checkChains([f(FILE), f("other-chain.json", { ...other, name: "JACK IN THE BLOCK" })]);
  assert.ok(name.errors.some((e) => /same name/.test(e)), name.errors.join("; "));
  const junk = checkChains([{ name: "notes.txt", text: "" }, { name: "broken.json", text: "{" }]);
  assert.ok(junk.errors.some((e) => /notes\.txt: only/.test(e)) && junk.errors.some((e) => /broken\.json: isn't valid JSON/.test(e)), junk.errors.join("; "));
});

test("links: a listing's JSON, GitHub's new-file page, a contact", () => {
  const json = listingJson({ ...GOOD, listed_at: undefined, contact: "jackdishman" });
  assert.deepEqual(Object.keys(JSON.parse(json)), ["name", "chain_id", "token", "description", "rpc", "genesis_hash", "validators_wanted", "p2p", "contact"]);
  const link = new URL(listingLink("jack-in-the-block", json));
  assert.equal(link.origin + link.pathname, "https://github.com/jackdishman/blockula/new/main");
  assert.equal(link.searchParams.get("filename"), "chains/jack-in-the-block.json");
  assert.equal(link.searchParams.get("value"), json);
  assert.equal(contactUrl("jackdishman"), "https://github.com/jackdishman");
  assert.equal(contactUrl("https://forum.example.org/u/jack"), "https://forum.example.org/u/jack");
  assert.equal(units("50000000000"), "50");
  assert.equal(units("10000000000000"), "10,000");
  assert.equal(units("500000000"), "0.5");
  assert.equal(units("1"), "0.000000001");
});

const root = new URL("../../", import.meta.url);
const check = (dir) => spawnSync(process.execPath, [fileURLToPath(new URL("scripts/check-chains.mjs", root)), ...(dir ? [fileURLToPath(new URL(dir, root))] : [])], { encoding: "utf8" });

test("scripts/check-chains.mjs passes the repo's directory and the e2e fixtures", () => {
  for (const dir of [undefined, "web/e2e/fixtures/chains/", "web/e2e/fixtures/chains-empty/"]) {
    const r = check(dir);
    assert.equal(r.status, 0, `${dir}: ${r.stderr}`);
    assert.match(r.stdout, /✓ chain directory/);
    assert.equal(r.stderr, "", "no warnings");
  }
  // The repo lists only *.json files and its README.
  for (const name of readdirSync(new URL("chains/", root))) assert.ok(name === "README.md" || name.endsWith(".json"), name);
  assert.match(readFileSync(new URL("chains/README.md", root), "utf8"), /## List your chain[\s\S]*## What gets accepted[\s\S]*## Delist/);
});

test("scripts/check-chains.mjs fails a bad directory, saying why", () => {
  const r = check("web/e2e/fixtures/chains-bad/");
  assert.equal(r.status, 1);
  for (const says of [/http-node\.json: rpc must be https/, /blockula-official\.json: name reads as Blockula/, /wrong-name\.json: the file must be named tailnet-only\.json/, /wrong-name\.json: p2p's host .* on a tailnet/, /notes\.txt: only/]) assert.match(r.stderr, says);
  const missing = check("web/e2e/fixtures/no-such-dir/");
  assert.equal(missing.status, 1);
  assert.match(missing.stderr, /can't read/);
});
