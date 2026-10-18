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
  assert.equal(head, "block,status,kind,direction,counterparty,counterparty_address,amount_fang,fee_fang,exact,note,signed_by,memo,summary,tx_id");
  assert.equal(r1, `7,finalized,transfer,out,mina.boo,boo1mina,-1000.500004,0.000004,yes,,claude (agent),'+tip,"vlad.boo sent 1,000.5 FANG to mina.boo",a1`);
  assert.equal(r2, "8,finalized,transfer,in,mina.boo,boo1mina,2,,yes,,,,mina.boo sent 2 FANG to vlad.boo,a2");
});

test("rows a transaction can't fully account for say so: v1 kinds, a tip, an account it may have opened (audit main13-render-12)", () => {
  const fresh = `boo1${"q".repeat(58)}`;
  const val = label("val");
  const txs = [
    // A tipped transfer to an address it may have opened: the fee paid is the whole charge.
    { id: "b1", summary: "vlad.boo sent 1 FANG to boo1qq…", status: "finalized", height: 10, kind: "transfer", from: vlad, to: { address: fresh, name: null, short: "boo1qq…" }, to_name: null, amount: String(F), fee: "164440", max_fee: "164440", tip: "100000", action: { type: "transfer", to: fresh, amount: String(F) } },
    { id: "b2", summary: "vlad.boo delegated 20 FANG to validator val.boo", status: "finalized", height: 11, kind: "delegate", from: vlad, to: val, amount: String(20n * F), fee: "4000", action: { type: "delegate", validator: val.address, amount: String(20n * F) } },
    { id: "b3", summary: "vlad.boo claimed 1000 FANG from community distribution #0", status: "finalized", height: 12, kind: "claim_distribution", from: vlad, amount: String(1000n * F), fee: "4000", action: { type: "claim_distribution", distribution_id: 0 } },
    { id: "b4", summary: "vlad.boo renewed vlad.boo for 1 year", status: "finalized", height: 13, kind: "renew_name", from: vlad, fee: "4000", action: { type: "renew_name", name: "vlad", years: 1 } },
  ];
  const lines = activityCsv(txs, "boo1vlad").trim().split("\r\n");
  const [, opened, staked, claimed, renewed] = lines.map((r) => r.split(","));
  assert.deepEqual([opened[6], opened[7], opened[8]], ["-1.00016444", "0.00016444", "no"]);
  assert.match(lines[1], /,no,"If this payment opened boo1qq…'s account, the chain also burned the account-creation fee/);
  assert.deepEqual([staked[3], staked[4], staked[6], staked[8]], ["out", "Stake with val.boo", "-20.000004", "yes"]);
  assert.deepEqual([claimed[3], claimed[6], claimed[8]], ["in", "999.999996", "yes"]);
  assert.deepEqual([renewed[6], renewed[8]], ["-0.000004", "no"]);
  assert.match(lines[4], /renewal price went to the treasury/);
});
