// lib/explorer/fields.ts: an action form's fields as typed. Addresses are checked against the
// chain's own prefix (DEC-7: boo on mainnet and dev chains, tboo on protocol v1 testnets), so a
// testnet address can be typed on a testnet and another network's is refused, never converted
// (audit main13-protocol-13, main13-render-14). Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { bech32m } from "@scure/base";
import { checkAccount, checkField } from "./explorer/fields.ts";

const key = new Uint8Array(32).fill(7);
const on = (prefix) => bech32m.encode(prefix, bech32m.toWords(key));

test("an address of the chain's network passes, another network's is refused", () => {
  assert.equal(checkAccount(on("tboo"), "tboo"), null, "a testnet address on a testnet");
  assert.equal(checkAccount(on("boo"), "boo"), null);
  assert.match(checkAccount(on("boo"), "tboo"), /another network \(boo1…\)\. Addresses here start with tboo1/);
  assert.match(checkAccount(on("tboo"), "boo"), /another network \(tboo1…\)\. Addresses here start with boo1/);
  assert.match(checkAccount(`${on("tboo").slice(0, -1)}q`, "tboo"), /doesn't look right/, "a typo breaks the checksum");
});

test("names pass, and the hint names the chain's prefix", () => {
  assert.equal(checkAccount("alice.boo", "tboo"), null);
  assert.equal(checkAccount("Alice", "boo"), null);
  assert.equal(checkAccount("al", "tboo"), "A .boo name or a tboo1… address.");
  // review-main13-5: a name may look like an address's start ('team1…', even 'boo1…'): only
  // something too long for a name, with a known network's prefix, is read as an address.
  for (const name of ["team1abcdefghijklmnopqrst", "boo1abcdefghijklmnopqrstuvwxyz", "tboo1abcdefghij"]) assert.equal(checkAccount(name, "boo"), null, name);
  assert.equal(checkAccount("cosmos1abcdefghijklmnopqrstuvwxyz0123456789", "boo"), "A .boo name or a boo1… address.");
});

test("other fields", () => {
  assert.equal(checkField({ type: "fang" }, "0.25", "boo"), null);
  assert.equal(checkField({ type: "fang" }, "0", "boo"), "More than zero.");
  assert.equal(checkField({ type: "validator" }, on("tboo"), "tboo"), null);
  assert.equal(checkField({ type: "days" }, "366", "boo"), "1 to 365 days.");
  assert.equal(checkField({ type: "years" }, "11", "boo"), "1 to 10 years.");
  assert.equal(checkField({ type: "text", max: 4 }, "ghoul", "boo"), "At most 4 bytes.");
  assert.equal(checkField({ type: "text", optional: true }, "  ", "boo"), null);
  assert.equal(checkField({ type: "key_id" }, "ab".repeat(32), "boo"), null);
});
