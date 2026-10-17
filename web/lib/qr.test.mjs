// Known-answer tests for lib/qr.ts. Expected hashes come from Project Nayuki's reference
// generator (python/qrcodegen.py: QrSegment.make_bytes, Ecc.MEDIUM, boostecl=False): SHA-256 of
// the module rows joined by "\n", "1" = dark. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { encodeQr, qrPath, qrSvg } from "./qr.ts";
import { checkLinkChain, linkChain, MAX_DROPS, payLink, requestAmountError } from "./paylink.ts";

const digest = (m) => createHash("sha256").update(m.map((r) => r.map((c) => (c ? "1" : "0")).join("")).join("\n")).digest("hex");

const BASE = "https://blockula-wallet.vercel.app";

test("matches the reference encoder (version and auto mask)", () => {
  const cases = [
    ["boo", 21, "d857ff165ebe858886396585872aaa7590eb0af5f075524341c13dfdb8f6c3b6"],
    [`${BASE}/send?to=vladimirskalp.boo`, 33, "9df6278a7ac3d6fa4b3c76ecfe858eb480ecfbd527c14fa985fa8da6d17ed827"],
    [
      `${BASE}/send?to=boo1yuuvyc7ngus0l2vytdt6yr0zqhj2qyz9j4zyfxk97vjtpsw9rk4qzmxxe9&amount=1.5&memo=trick+or+treat+%F0%9F%A7%9B`,
      49,
      "0659efc15bc0ed286e5d8da666373c632f7aeb0578fd4912b80f43a5373b072a",
    ],
    ["x".repeat(1000), 121, "e9dd5f05743a886f858ae21aaee03839ffa1dc9fc53143b44583d2ebef5874c8"],
  ];
  for (const [text, size, hash] of cases) {
    const m = encodeQr(text);
    assert.equal(m.length, size, text);
    assert.equal(digest(m), hash, text);
  }
});

test("matches the reference encoder for every mask", () => {
  const hashes = [
    "34b8b56303efb4698d2fa812fca48716f37d3a1580c7ca33327da861cf5c7f96",
    "5b0e8c6a7c6690b5478ad4f69034ec368e80f89d8dc909b0bf335181f1d3e20e",
    "d857ff165ebe858886396585872aaa7590eb0af5f075524341c13dfdb8f6c3b6",
    "e2eac1817ca6370e1410c6f4bfe237637e5e3d1d1a270b4593f0ee74c7cd1144",
    "d2b5a7442da7696e19465d281d9e421271ef0e067124e4ec2bc44be19c5f8995",
    "3ec307c989668c8b3f1cd4182bc2df74ade54e7540bd6410ac4edbcccfe560df",
    "3efd08830de0aa4de6323f520ae6b6cb438299997715e6c36af256ce0606d277",
    "4e2dd1caa70b92ae0f38c1ee9f3b5a66a1a0daedd3b6cb30a909a09c02831940",
  ];
  hashes.forEach((hash, mask) => assert.equal(digest(encodeQr("boo", { mask })), hash, `mask ${mask}`));
});

test("rejects data beyond version 40", () => {
  assert.equal(encodeQr("x".repeat(2331)).length, 177);
  assert.throws(() => encodeQr("x".repeat(2332)), RangeError);
  // What QrCode draws: null instead of the RangeError, so /receive shows a note (audit main-11-2).
  assert.equal(qrSvg("x".repeat(2332)), null);
  assert.deepEqual(qrSvg("boo"), { size: 29, d: qrPath(encodeQr("boo"), 4) });
});

test("path merges runs and offsets by the quiet zone", () => {
  const m = [
    [true, true, false],
    [false, false, true],
    [false, false, false],
  ];
  assert.equal(qrPath(m, 4), "M4 4h2v1h-2zM6 5h1v1h-1z");
});

test("payment links round-trip through the query string Send reads", () => {
  const url = new URL(payLink(BASE, "alice.boo", "1.5", "trick or treat & a+b 🧛"));
  assert.equal(url.origin + url.pathname, `${BASE}/send`);
  assert.equal(url.searchParams.get("to"), "alice.boo");
  assert.equal(url.searchParams.get("amount"), "1.5");
  assert.equal(url.searchParams.get("memo"), "trick or treat & a+b 🧛");
  assert.equal(payLink(`${BASE}/`, "alice.boo"), `${BASE}/send?to=alice.boo`);
  assert.equal(payLink(BASE, "alice.boo", " ", ""), `${BASE}/send?to=alice.boo`);
});

const TESTNET = { chainId: "blockula-testnet", genesisHash: "6e".repeat(32) };

test("payment links say which chain they were made for, as a check (audit main-11-1)", () => {
  const url = new URL(payLink(BASE, "alice.boo", "1.5", "m", TESTNET));
  assert.equal(url.searchParams.get("chain"), "blockula-testnet");
  assert.equal(url.searchParams.get("genesis"), "6e".repeat(8), "the first 16 hex digits of the genesis hash");
  assert.equal(payLink(BASE, "alice.boo", "", "", { chainId: "dev" }), `${BASE}/send?to=alice.boo&chain=dev`, "no genesis when the pin doesn't know it");
  assert.equal(payLink(BASE, "alice.boo", "", "", { genesisHash: "6e".repeat(32) }), `${BASE}/send?to=alice.boo`, "nothing without a chain id");
  const read = (link) => linkChain(new URL(link).searchParams);
  assert.deepEqual(read(payLink(BASE, "alice.boo", "1", "", TESTNET)), { chainId: "blockula-testnet", genesis: "6e".repeat(8) });
  assert.equal(read(`${BASE}/send?to=alice.boo&amount=1`), null, "an older link doesn't say");
  assert.equal(read(`${BASE}/send?to=alice.boo&chain=`), null);
  const check = (link, wallet = TESTNET) => checkLinkChain(link && read(link), wallet);
  assert.deepEqual(check(payLink(BASE, "a.boo", "1", "", TESTNET)), { state: "match" });
  assert.deepEqual(check(payLink(BASE, "a.boo", "1", "", { chainId: "blockula-1", genesisHash: "4d".repeat(32) })), { state: "mismatch", why: "chain" });
  assert.deepEqual(check(payLink(BASE, "a.boo", "1", "", { chainId: "blockula-testnet", genesisHash: "00".repeat(32) })), { state: "mismatch", why: "genesis" }, "another instance of the chain id");
  assert.deepEqual(check(`${BASE}/send?to=a.boo&chain=blockula-testnet&genesis=6e`), { state: "mismatch", why: "genesis" }, "a short genesis would match too much");
  assert.deepEqual(check(`${BASE}/send?to=a.boo&chain=blockula-testnet&genesis=6E6E6E6E6E6E6E6E`), { state: "match" }, "hex in either case");
  assert.deepEqual(check(`${BASE}/send?to=a.boo&chain=blockula-testnet`), { state: "match" }, "a chain id alone checks the chain id");
  assert.deepEqual(check(`${BASE}/send?to=a.boo&amount=1`), { state: "unchecked" });
  assert.deepEqual(check(payLink(BASE, "a.boo", "1", "", TESTNET), {}), { state: "pending" }, "the wallet doesn't know its chain yet");
  assert.deepEqual(check(payLink(BASE, "a.boo", "1", "", TESTNET), { chainId: "blockula-testnet" }), { state: "pending" }, "nor its genesis");
  assert.deepEqual(check(payLink(BASE, "a.boo", "1", "", TESTNET), { chainId: "blockula-1" }), { state: "mismatch", why: "chain" }, "a chain id mismatch needs no genesis");
});

test("payment requests take amounts the chain can hold, and every link they make fits a QR code (audit main-11-2)", () => {
  assert.equal(MAX_DROPS, 2n ** 64n - 1n);
  for (const ok of ["", " ", "1", "0.25", "1.000000001", " 2 ", "18446744073.709551615"]) assert.equal(requestAmountError(ok), null, ok);
  for (const [bad, why] of [["0", /more than zero/], ["0.000", /more than zero/], ["18446744073.709551616", /more FANG than there can ever be/], ["9".repeat(3000), /more FANG than there can ever be/], ["1.0000000001", /up to 9 decimals/], ["-1", /number/], ["1e3", /number/], ["0x10", /number/], [".5", /number/], ["1,5", /number/], ["Infinity", /number/]]) {
    assert.match(requestAmountError(bad), why, bad.slice(0, 30));
  }
  // The longest link /receive can make: a 63-character name, the largest amount, a 256-byte memo
  // of the costliest characters, a long chain id.
  const longest = payLink(BASE, `${"v".repeat(63)}.boo`, "18446744073.709551615", "🧛".repeat(64), { chainId: "c".repeat(64), genesisHash: "ab".repeat(32) });
  assert.ok(qrSvg(longest) !== null, `${longest.length} characters fit`);
});
