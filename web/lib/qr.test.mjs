// Known-answer tests for lib/qr.ts. Expected hashes come from Project Nayuki's reference
// generator (python/qrcodegen.py: QrSegment.make_bytes, Ecc.MEDIUM, boostecl=False): SHA-256 of
// the module rows joined by "\n", "1" = dark. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { encodeQr, qrPath } from "./qr.ts";
import { payLink } from "./paylink.ts";

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
