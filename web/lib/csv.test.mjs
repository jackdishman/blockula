// lib/explorer/csv.ts: activity export. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { activityCsv, cell } from "./explorer/csv.ts";

const F = 1_000_000_000n;
const label = (name) => ({ address: `boo1${name}`, name: `${name}.boo`, short: `boo1${name}…` });
const vlad = label("vlad"), mina = label("mina");
const keys = [{ key_id: "k-agent", label: "claude", role: { type: "session", limit: "1", period_blocks: 1, expires_at: 9 }, added_at: 1, spent_this_period: "0", window_start: 0 }];

test("cells: quoted when needed, and formulas from strangers are defused", () => {
  assert.equal(cell("plain"), "plain");
  assert.equal(cell('say "boo", twice'), '"say ""boo"", twice"');
  assert.equal(cell("=HYPERLINK(1)"), "'=HYPERLINK(1)");
  assert.equal(cell("-5", false), "-5", "numbers keep their sign");
  assert.equal(cell(null), "");
});

test("rows are signed from the account's side, exact, and name the key that signed", () => {
  const out = { id: "a1", summary: "vlad.boo sent 1,000.5 FANG to mina.boo", status: "finalized", height: 7, kind: "transfer", from: vlad, to: mina, amount: String(1000n * F + F / 2n), fee: "4000", signer_key: "k-agent", memo: "+tip" };
  const inn = { id: "a2", summary: "mina.boo sent 2 FANG to vlad.boo", status: "finalized", height: 8, kind: "transfer", from: mina, to: vlad, amount: String(2n * F), fee: "4000" };
  const [head, r1, r2] = activityCsv([out, inn], "boo1vlad", keys).trim().split("\r\n");
  assert.equal(head, "block,status,kind,direction,counterparty,counterparty_address,amount_fang,fee_fang,signed_by,memo,summary,tx_id");
  assert.equal(r1, `7,finalized,transfer,out,mina.boo,boo1mina,-1000.500004,0.000004,claude (agent),'+tip,"vlad.boo sent 1,000.5 FANG to mina.boo",a1`);
  assert.equal(r2, "8,finalized,transfer,in,mina.boo,boo1mina,2,,,,mina.boo sent 2 FANG to vlad.boo,a2");
});
