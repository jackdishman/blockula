// lib/explorer/preflight.ts: effects and refusals before signing. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { preflight } from "./explorer/preflight.ts";

const F = 1_000_000_000n;
const label = (name) => ({ address: `boo1${name}`, name: `${name}.boo`, short: `boo1${name}…` });
const vlad = label("vlad"), mina = label("mina");
const owner = { key_id: "k-owner", label: "laptop", role: { type: "owner" }, added_at: 0, spent_this_period: "0", window_start: 0 };
const agent = (spent, window_start = 90, expires_at = 1_000_000) => ({ key_id: "k-agent", label: "claude", role: { type: "session", limit: String(10n * F), period_blocks: 86_400, expires_at }, added_at: 5, spent_this_period: String(spent), window_start });
const sender = (balance, keys = [owner], recent = []) => ({ label: vlad, balance: String(balance), keys, recent });
const transfer = (fang, fee = 4000n) => ({ from: "boo1vlad", fee: fee.toString(), nonce: 3, action: { type: "transfer", to: "boo1mina", amount: String(fang) } });

test("a transfer: the sender pays amount + fee, the recipient gets the amount, the balance after is exact", () => {
  const p = preflight({ tx: transfer(5n * F), sender: sender(100n * F), signer: "k-owner", head: 100, recipient: { label: mina, isNew: false } });
  assert.deepEqual(p.effects.changes.map((c) => [c.who.address, c.delta]), [["boo1vlad", -(5n * F + 4000n)], ["boo1mina", 5n * F]]);
  assert.equal(p.cost, 5n * F + 4000n);
  assert.equal(p.balanceAfter, 95n * F - 4000n);
  assert.deepEqual(p.refusals, []);
});

test("not enough FANG, counting what pending transactions will take", () => {
  const pend = { id: "p", summary: "", status: "pending", height: null, kind: "transfer", from: vlad, to: mina, amount: String(4n * F), fee: "4000" };
  const p = preflight({ tx: transfer(5n * F), sender: sender(8n * F, [owner], [pend]), signer: "k-owner", head: 100, recipient: { label: mina, isNew: false } });
  assert.equal(p.pending, 4n * F + 4000n);
  assert.match(p.refusals[0], /Not enough FANG: this needs 5\.000004 FANG.*after your pending/);
});

test("a first payment to a new account must be at least 0.01 FANG", () => {
  const small = preflight({ tx: transfer(F / 1000n), sender: sender(F), head: 1, recipient: { label: mina, isNew: true } });
  assert.match(small.refusals[0], /new to the chain.*0\.01 FANG/);
  const fine = preflight({ tx: transfer(F), sender: sender(2n * F), head: 1, recipient: { label: mina, isNew: true } });
  assert.deepEqual(fine.refusals, []);
  assert.match(fine.notes[0], /hasn't been seen/);
});

test("an agent key: the budget after counts amount + fee, and the window resets on its own", () => {
  const ok = preflight({ tx: transfer(2n * F), sender: sender(50n * F, [owner, agent(3n * F)]), signer: "k-agent", head: 100, recipient: { label: mina, isNew: false } });
  assert.equal(ok.budget.spentAfter, 5n * F + 4000n);
  assert.deepEqual(ok.refusals, []);
  const over = preflight({ tx: transfer(8n * F), sender: sender(50n * F, [owner, agent(3n * F)]), signer: "k-agent", head: 100, recipient: { label: mina, isNew: false } });
  assert.match(over.refusals[0], /Over the agent key's budget: “claude” has used 3 FANG of 10 FANG/);
  // The period ended: the spent counter starts over in the block the transaction lands in.
  const fresh = preflight({ tx: transfer(8n * F), sender: sender(50n * F, [owner, agent(3n * F, 0)]), signer: "k-agent", head: 86_399, recipient: { label: mina, isNew: false } });
  assert.deepEqual(fresh.refusals, []);
  const expired = preflight({ tx: transfer(F), sender: sender(50n * F, [owner, agent(0n, 90, 100)]), signer: "k-agent", head: 100, recipient: { label: mina, isNew: false } });
  assert.match(expired.refusals[0], /expired at block #100/);
});

test("a known cost the fields don't carry (a name's price) is counted and shown", () => {
  const treasury = { address: "holding:treasury", name: "Treasury", short: "Treasury", holding: true };
  const p = preflight({ tx: { from: "boo1vlad", fee: "4000", nonce: 1, action: { type: "register_name", name: "ghoul" } }, sender: sender(100n * F), head: 1, extra: { drops: 5n * F, to: treasury, why: "the name's price" } });
  assert.equal(p.cost, 5n * F + 4000n);
  assert.equal(p.effects.unseen, null);
  assert.equal(p.effects.changes[1].delta, 5n * F);
});

test("hunt rules: minimums, and a closed or expired hunt can't take funding", () => {
  const fund = (fang) => ({ from: "boo1vlad", fee: "4000", nonce: 1, action: { type: "fund_hunt", hunt_id: 2, amount: String(fang) } });
  assert.match(preflight({ tx: fund(F / 2n), sender: sender(100n * F), head: 1 }).refusals[0], /at least 1 FANG/);
  assert.match(preflight({ tx: fund(F), sender: sender(100n * F), head: 1, hunt: { id: 2, status: "awarded", expires_at: 99 } }).refusals[0], /awarded/);
  assert.match(preflight({ tx: fund(F), sender: sender(100n * F), head: 99, hunt: { id: 2, status: "open", expires_at: 99 } }).refusals[0], /expired at block #99/);
  assert.match(preflight({ tx: { ...fund(F), action: { type: "create_hunt", amount: String(9n * F) } }, sender: sender(100n * F), head: 1 }).refusals[0], /at least 10 FANG/);
});
