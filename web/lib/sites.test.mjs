// Name sites (sites/README.md): listings, records, targets and the DNS plan.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  checkSite,
  checkSites,
  parseSiteMemo,
  planDns,
  siteLabelProblem,
  siteMemo,
  siteNameProblem,
  siteRecordProblem,
  siteTargetProblem,
  SITE_RECORD_COMMENT,
  wellKnownProblem,
} from "./sites.ts";

const OWNER = "boo1pq9qa328a7teh8chphrf479p9gu6y766wtp4zg5cmyxdnjnu6r9sxlcsez";
const KEY = "080a0ec547ef979b9f170dc69af8a12a39a27b5a72c3512298d90cd9ca7cd0cb";
const RECORD = "a5db5a4cfab352f906e1ca3ffcc8011079135a4c5cf41d0d0497613c78677e01";
const listing = (patch = {}) => ({ name: "blood.boo", owner: OWNER, target: "blood-blockula.vercel.app", record: RECORD, contact: "darkoheth", listed_at: "2026-11-13", ...patch });
const errs = (patch, file = "blood.json") => checkSite(file, Object.fromEntries(Object.entries(listing(patch)).filter(([, v]) => v !== undefined)));

test("a good listing", () => {
  assert.deepEqual(errs({}), []);
  assert.deepEqual(errs({ listed_at: undefined, contact: "https://forum.example.org/u/darkoh" }), []);
});

test("subdomains: the .boo name rules, less the project's own", () => {
  for (const ok of ["blood", "abc", "a-b-c", "x2y", "pizza-club"]) assert.equal(siteLabelProblem(ok), null, ok);
  for (const [bad, says] of [
    ["ab", /3–32/],
    ["a".repeat(33), /3–32/],
    ["Blood", /a–z/],
    ["blo_od", /a–z/],
    ["-blood", /start or end/],
    ["xn--blood", /xn--/],
    ["www", /keeps/],
    ["wallet", /keeps/],
    ["demo", /keeps/],
    ["blockula", /Blockula's own/],
    ["myblockula-wallet", /Blockula's own/],
    ["b1ockula", /Blockula's own/],
    ["official", /official/],
  ]) assert.match(siteLabelProblem(bad) ?? "", says, bad);
});

test("targets: someone's public host, never the project's domain", () => {
  for (const ok of ["blood-blockula.vercel.app", "darkoheth.github.io", "blood.example.org", "app.fly.dev"]) assert.equal(siteTargetProblem(ok), null, ok);
  for (const [bad, says] of [
    ["Blood.vercel.app", /lowercase/],
    ["blood.vercel.app.", /trailing dot/],
    ["203.0.113.7", /IP address/],
    ["localhost", /local|DNS name/],
    ["box.local", /local/],
    ["demo.blockula.app", /blockula\.app itself/],
    ["blockula.app", /blockula\.app itself/],
    ["https://blood.vercel.app", /DNS name/],
    [`${"a".repeat(195)}.example`, /200/],
  ]) assert.match(siteTargetProblem(bad) ?? "", says, bad);
});

test("memos: exactly site:v1:<name>:<target>", () => {
  assert.equal(siteMemo("blood", "blood-blockula.vercel.app"), "site:v1:blood:blood-blockula.vercel.app");
  assert.deepEqual(parseSiteMemo("site:v1:blood:blood-blockula.vercel.app"), { label: "blood", target: "blood-blockula.vercel.app" });
  for (const other of ["", "site:v1:blood", "site:v2:blood:x.example", " site:v1:blood:x.example", "site:v1:blood:x.example ", "site:v1:Blood:x.example", null, 7]) assert.equal(parseSiteMemo(other), null, String(other));
  assert.throws(() => siteMemo("www", "x.example"), /keeps/);
  assert.throws(() => siteMemo("blood", "demo.blockula.app"), /itself/);
  assert.ok(new TextEncoder().encode(siteMemo("a".repeat(32), `${"b".repeat(63)}.${"c".repeat(63)}.${"d".repeat(63)}.org`)).length <= 256, "the longest memo fits the chain's 256 bytes");
});

test("listings refuse bad shapes, saying why", () => {
  assert.deepEqual(checkSite("blood.json", []), ["isn't a JSON object"]);
  for (const [patch, says, file] of [
    [{ name: "other.boo" }, /name must be blood\.boo/],
    [{ owner: "darkoh.boo" }, /owner must be/],
    [{ owner: "boo1BAD" }, /owner must be/],
    [{ target: "demo.blockula.app" }, /target is on blockula\.app/],
    [{ record: "abc" }, /record must be/],
    [{ contact: "javascript:alert(1)" }, /contact/],
    [{ contact: "http://example.org" }, /contact/],
    [{ listed_at: "2026-13-01" }, /listed_at/],
    [{ extra: true }, /doesn't take: extra/],
    [{ record: undefined }, /missing record/],
    [{}, /keeps/, "www.json"],
    [{}, /\.json file/, "blood.txt"],
  ]) {
    const e = errs(patch, file);
    assert.ok(e.some((m) => says.test(m)), `${JSON.stringify(patch)} ${file ?? ""}: ${e.join("; ")}`);
  }
});

test("the directory", () => {
  const { sites, errors } = checkSites([
    { name: "README.md", text: "# sites" },
    { name: "blood.json", text: JSON.stringify(listing()) },
    { name: "zz.json", text: "{nope" },
  ]);
  assert.deepEqual(sites.map((s) => s.label), ["blood"]);
  assert.equal(errors.length, 1);
  assert.match(errors[0], /sites\/zz\.json: isn't JSON/);
});

test("the name: active and the listing's owner's", () => {
  const site = listing();
  assert.equal(siteNameProblem(site, { status: "active", owner: { address: OWNER } }), null);
  assert.match(siteNameProblem(site, { status: "grace", owner: { address: OWNER } }), /grace.*renew/);
  assert.match(siteNameProblem(site, { status: "available" }), /available/);
  assert.match(siteNameProblem(site, { status: "active", owner: { address: "boo1someoneelse0000000000000000000" } }), /owned by boo1someoneelse/);
});

test("the record: the owner to itself, an owner key, the exact memo", () => {
  const site = { label: "blood", ...listing() };
  const tx = { id: RECORD, kind: "transfer", status: "finalized", error: null, from: { address: OWNER }, to: { address: OWNER }, signer_key: KEY, memo: "site:v1:blood:blood-blockula.vercel.app" };
  assert.equal(siteRecordProblem(site, tx, [KEY]), null);
  for (const [patch, says] of [
    [{ id: "00".repeat(32) }, /another transaction/],
    [{ kind: "register_name" }, /isn't a transfer/],
    [{ status: "pending" }, /didn't run/],
    [{ error: "out of funds" }, /didn't run/],
    [{ to: { address: "boo1someoneelse0000000000000000000" } }, /to itself/],
    [{ from: { address: "boo1someoneelse0000000000000000000" } }, /to itself/],
    [{ signer_key: "ff".repeat(32) }, /owner keys/],
    [{ memo: "site:v1:blood:elsewhere.example.org" }, /memo isn't/],
    [{ memo: "site:v1:other:blood-blockula.vercel.app" }, /memo isn't/],
  ]) assert.match(siteRecordProblem(site, { ...tx, ...patch }, [KEY]) ?? "", says, JSON.stringify(patch));
  assert.match(siteRecordProblem(site, tx, []) ?? "", /owner keys/, "a key that's no longer an owner key stops counting");
});

test("the target's /.well-known/boo names the owner on a line", () => {
  assert.equal(wellKnownProblem(OWNER, `${OWNER}\n`), null);
  assert.equal(wellKnownProblem(OWNER, `# blood.blockula.app\r\n  ${OWNER}  \r\n`), null);
  assert.ok(wellKnownProblem(OWNER, `not ${OWNER}`));
  assert.ok(wellKnownProblem(OWNER, ""));
});

test("the DNS plan touches only the sync's own records", () => {
  const mine = (id, name, value, type = "CNAME") => ({ id, name, type, value, comment: SITE_RECORD_COMMENT });
  const records = [
    { id: "r1", name: "demo", type: "A", value: "37.16.2.248", comment: "Blockula demo chain on Fly" },
    { id: "r2", name: "", type: "CAA", value: '0 issue "letsencrypt.org"' },
    mine("r3", "blood", "old.vercel.app."),
    mine("r4", "gone", "gone.example.org"),
    mine("r5", "keep", "keep.example.org"),
    { id: "r6", name: "taken", type: "TXT", value: "hand-made" },
  ];
  const want = new Map([["blood", "blood-blockula.vercel.app"], ["keep", "keep.example.org"], ["new", "new.example.org"], ["taken", "x.example.org"], ["demo", "evil.example.org"]]);
  const { changes, conflicts } = planDns(want, records);
  assert.deepEqual(changes, [
    { op: "update", id: "r3", name: "blood", from: "old.vercel.app", value: "blood-blockula.vercel.app" },
    { op: "create", name: "new", value: "new.example.org" },
    { op: "delete", id: "r4", name: "gone", value: "gone.example.org" },
  ]);
  assert.equal(conflicts.length, 2);
  assert.match(conflicts.join("\n"), /demo:.*A 37\.16\.2\.248.*left alone/);
  assert.match(conflicts.join("\n"), /taken:.*TXT/);
  assert.deepEqual(planDns(new Map(), [records[0], records[1]]).changes, [], "nothing of anyone else's is deleted");
  assert.deepEqual(planDns(new Map([["dup", "d.example.org"]]), [mine("a", "dup", "d.example.org"), mine("b", "dup", "1.2.3.4", "A")]).changes, [{ op: "delete", id: "b", name: "dup", value: "1.2.3.4" }]);
});

const script = fileURLToPath(new URL("../../scripts/check-sites.mjs", import.meta.url));

test("scripts/check-sites.mjs passes the repo's directory and fails a bad one", () => {
  execFileSync(process.execPath, [script], { stdio: "pipe" });
  const dir = mkdtempSync(join(tmpdir(), "sites-"));
  writeFileSync(join(dir, "www.json"), JSON.stringify(listing({ name: "www.boo" })));
  assert.throws(() => execFileSync(process.execPath, [script, dir], { stdio: "pipe" }), (e) => /keeps/.test(String(e.stderr)));
});
