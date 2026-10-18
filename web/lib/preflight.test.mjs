// lib/explorer/preflight.ts: effects and refusals before signing. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { preflight, preflightInput } from "./explorer/preflight.ts";

const F = 1_000_000_000n;
const label = (name) => ({ address: `boo1${name}`, name: `${name}.boo`, short: `boo1${name}…` });
const vlad = label("vlad"), mina = label("mina");
const owner = { key_id: "k-owner", label: "laptop", role: { type: "owner" }, added_at: 0, spent_this_period: "0", window_start: 0 };
const agent = (spent, window_start = 90, expires_at = 1_000_000) => ({ key_id: "k-agent", label: "claude", role: { type: "session", limit: String(10n * F), period_blocks: 86_400, expires_at }, added_at: 5, spent_this_period: String(spent), window_start });
const sender = (balance, keys = [owner], recent = [], extra = {}) => ({ label: vlad, balance: String(balance), transferable: String(balance), keys, recent, ...extra });
const transfer = (fang, fee = 4000n) => ({ from: "boo1vlad", fee: fee.toString(), nonce: 3, action: { type: "transfer", to: "boo1mina", amount: String(fang) } });

test("a transfer: the sender pays amount + fee, the recipient gets the amount, the balance after is exact", () => {
  const p = preflight({ tx: transfer(5n * F), sender: sender(100n * F), signer: "k-owner", head: 100, recipient: { label: mina, isNew: false } });
  assert.deepEqual(p.effects.changes.map((c) => [c.who.address, c.delta]), [["boo1vlad", -(5n * F + 4000n)], ["boo1mina", 5n * F]]);
  assert.equal(p.cost, 5n * F + 4000n);
  assert.equal(p.balanceAfter, 95n * F - 4000n);
  assert.deepEqual(p.refusals, []);
});

test("not enough FANG, counting what pending transactions will take", () => {
  const pend = { id: "p", summary: "", status: "pending", height: null, kind: "transfer", from: vlad, to: mina, amount: String(4n * F), fee: "4000", action: { type: "transfer", to: "boo1mina", amount: String(4n * F) } };
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

test("pending reservations mirror the node's tx_debit: a name's price and a claim bond count too", () => {
  const pend = (action) => ({ id: action.type, summary: "", status: "pending", height: null, kind: action.type, from: vlad, fee: "4000", action });
  const p = preflight({ tx: transfer(1n * F), sender: sender(30n * F, [owner], [pend({ type: "register_name", name: "bat" }), pend({ type: "submit_claim", repo: "a/b", pr: 1 })]), head: 1, recipient: { label: mina, isNew: false } });
  assert.equal(p.pending, 100n * F + F + 8000n, "a 3-letter name costs 100 FANG");
  assert.match(p.refusals[0], /Not enough FANG/);
});

test("agent keys: only transfers, or claims from a registered Familiar", () => {
  const keys = [owner, agent(0n)];
  const claim = { from: "boo1vlad", fee: "4000", nonce: 1, action: { type: "submit_claim", repo: "a/b", pr: 2 } };
  const bond = { drops: F, to: { address: "holding:claim-bond", name: "Claim bond", short: "Claim bond", holding: true }, why: "refundable claim bond" };
  assert.match(preflight({ tx: claim, sender: sender(50n * F, keys), signer: "k-agent", head: 100, extra: bond }).refusals[0], /not a registered Familiar/);
  const familiar = preflight({ tx: claim, sender: { ...sender(50n * F, keys), familiars: ["k-agent"] }, signer: "k-agent", head: 100, extra: bond });
  assert.deepEqual(familiar.refusals, []);
  assert.equal(familiar.budget.spentAfter, F + 4000n, "a Familiar's claim counts its bond against the budget");
  const fund = { from: "boo1vlad", fee: "4000", nonce: 1, action: { type: "fund_pool", amount: String(F) } };
  assert.match(preflight({ tx: fund, sender: sender(50n * F, keys), signer: "k-agent", head: 100 }).refusals[0], /can only send transfers/);
});

test("an agent's own pending payments count against its budget (blocks apply them first)", () => {
  const pend = { id: "q", summary: "", status: "pending", height: null, kind: "transfer", from: vlad, to: mina, fee: "4000", signer_key: "k-agent", amount: String(6n * F), action: { type: "transfer", to: "boo1mina", amount: String(6n * F) } };
  const p = preflight({ tx: transfer(5n * F), sender: sender(50n * F, [owner, agent(0n)], [pend]), signer: "k-agent", head: 100, recipient: { label: mina, isNew: false } });
  assert.equal(p.budget.spentAfter, 11n * F + 8000n);
  assert.match(p.refusals[0], /counting its pending payments/);
});

test("funding a hunt that doesn't exist", () => {
  const fund = { from: "boo1vlad", fee: "4000", nonce: 1, action: { type: "fund_hunt", hunt_id: 9, amount: String(F) } };
  assert.match(preflight({ tx: fund, sender: sender(50n * F), head: 1, hunt: { id: 9, missing: true } }).refusals[0], /no hunt #9/);
});

// ---- protocol v1 vectors (node/src/chain.rs admit, node/src/econ.rs check_locks and charge_new_accounts) ----

const fee = 64_920n;
const CREATION = F / 10n;
const fresh = { address: "boo1fresh", name: null, short: "boo1fresh…" };

test("opening the recipient's account burns the creation fee from the sender (audit main13-protocol-2)", () => {
  // 0.5 FANG, sending 0.45 to a new address: 0.45 + fee + 0.1 doesn't fit.
  const short = preflight({ tx: transfer(45n * F / 100n, fee), sender: sender(F / 2n), signer: "k-owner", head: 10, recipient: { label: fresh, isNew: true, creationFee: CREATION } });
  assert.equal(short.cost, 45n * F / 100n + fee + CREATION);
  assert.match(short.refusals[0], /Not enough FANG: this needs 0\.55006492 FANG/);
  const ok = preflight({ tx: transfer(F, fee), sender: sender(2n * F), signer: "k-owner", head: 10, recipient: { label: fresh, isNew: true, creationFee: CREATION } });
  assert.deepEqual(ok.refusals, []);
  assert.deepEqual(ok.effects.changes.map((c) => [c.who.address, c.delta]), [["boo1vlad", -(F + fee + CREATION)], ["boo1fresh", F]]);
  assert.equal(ok.effects.burned, fee + CREATION);
  assert.equal(ok.balanceAfter, 2n * F - F - fee - CREATION);
  assert.match(ok.notes[0], /Opening its account burns 0\.1 FANG/);
  // An existing recipient: nothing extra.
  assert.equal(preflight({ tx: transfer(F, fee), sender: sender(2n * F), head: 10, recipient: { label: mina, isNew: false, creationFee: 0n } }).cost, F + fee);
});

test("an agent key's budget counts the creation fee too (audit main13-render-4)", () => {
  const spent = 4_250_323_090n;
  const p = preflight({ tx: transfer(57n * F / 10n, fee), sender: sender(50n * F, [owner, agent(spent)]), signer: "k-agent", head: 100, recipient: { label: fresh, isNew: true, creationFee: CREATION } });
  assert.equal(p.budget.spentAfter, spent + 57n * F / 10n + CREATION + fee);
  assert.match(p.refusals[0], /Over the agent key's budget.*opening the recipient's account \(0\.1 FANG\) included/);
  const known = preflight({ tx: transfer(57n * F / 10n, fee), sender: sender(50n * F, [owner, agent(spent)]), signer: "k-agent", head: 100, recipient: { label: mina, isNew: false, creationFee: 0n } });
  assert.deepEqual(known.refusals, []);
});

test("locked FANG (votes, vesting) can't leave, nor pay a tip, but can be staked (audit main13-render-5)", () => {
  const locked = { transferable: String(14_149_677_470n), locked: String(80n * F), lockReason: "vote on proposal #1 until block #9,000" };
  const s = sender(94_149_677_470n, [owner], [], locked);
  const p = preflight({ tx: transfer(50n * F, fee), sender: s, signer: "k-owner", head: 10, recipient: { label: mina, isNew: false } });
  assert.match(p.refusals[0], /80 FANG of this account is locked \(vote on proposal #1 until block #9,000\).*14\.14967747 FANG can move now/);
  assert.equal(p.locked, 80n * F);
  assert.deepEqual(preflight({ tx: transfer(14n * F, fee), sender: s, signer: "k-owner", head: 10, recipient: { label: mina, isNew: false } }).refusals, []);
  const stake = { from: "boo1vlad", fee: String(fee), nonce: 1, action: { type: "delegate", validator: "boo1val", amount: String(90n * F) } };
  assert.deepEqual(preflight({ tx: stake, sender: s, signer: "k-owner", head: 10 }).refusals, [], "staking locked tokens is fine");
  // A fee may come out of locked tokens, a tip never.
  const all = sender(10n * F, [owner], [], { transferable: "0", locked: String(10n * F) });
  const name = { from: "boo1vlad", fee: String(fee), nonce: 1, action: { type: "unlock" } };
  assert.deepEqual(preflight({ tx: name, sender: all, head: 1 }).refusals, []);
  assert.match(preflight({ tx: { ...name, fee: String(fee + 5000n), tip: "5000" }, sender: all, head: 1 }).refusals[0], /The tip must come from FANG that can move/);
  // A node that doesn't report locks: said, not guessed.
  assert.match(preflight({ tx: transfer(F, fee), sender: { label: vlad, balance: String(5n * F), keys: [owner] }, head: 1, recipient: { label: mina, isNew: false } }).notes.at(-1), /doesn't report locked FANG/);
});

test("a multisig's owner keys only propose and approve (audit main13-render-5)", () => {
  const s = sender(10n * F, [owner], [], { multisig: { threshold: 2, owners: 2 } });
  assert.match(preflight({ tx: transfer(F, fee), sender: s, signer: "k-owner", head: 1, recipient: { label: mina, isNew: false } }).refusals[0], /2-of-2 multisig.*multisig_propose/);
  const propose = { from: "boo1vlad", fee: String(fee), nonce: 1, action: { type: "multisig_propose", action: { type: "transfer", to: "boo1mina", amount: String(F) } } };
  assert.deepEqual(preflight({ tx: propose, sender: s, signer: "k-owner", head: 1 }).refusals, []);
});

test("hunts: only the creator awards, and cancels only after the expiry (admins any time) (audit main13-protocol-4)", () => {
  const hunt = { id: 3, status: "open", expires_at: 500, creator: "boo1vlad" };
  const act = (type) => ({ from: "boo1vlad", fee: String(fee), nonce: 1, action: { type, hunt_id: 3, to: "boo1mina", pr: 7 } });
  assert.match(preflight({ tx: act("cancel_hunt"), sender: sender(F), head: 100, hunt }).refusals[0], /can be cancelled by its creator after block #500/);
  assert.deepEqual(preflight({ tx: act("cancel_hunt"), sender: sender(F), head: 500, hunt: { ...hunt, status: "expired" } }).refusals, []);
  assert.deepEqual(preflight({ tx: act("cancel_hunt"), sender: sender(F), head: 100, hunt: { ...hunt, creator: "boo1mina" }, admins: ["boo1vlad"] }).refusals, [], "an admin of a dev chain");
  assert.match(preflight({ tx: act("cancel_hunt"), sender: sender(F), head: 600, hunt: { ...hunt, creator: "boo1mina" } }).refusals[0], /Only hunt #3's creator can cancel it/);
  assert.match(preflight({ tx: act("award_hunt"), sender: sender(F), head: 100, hunt: { ...hunt, creator: "boo1mina" }, recipient: { label: mina, isNew: false } }).refusals[0], /Only hunt #3's creator can award it/);
  assert.deepEqual(preflight({ tx: act("award_hunt"), sender: sender(F), head: 900, hunt: { ...hunt, status: "expired" }, recipient: { label: mina, isNew: false } }).refusals, [], "an expired hunt can still be awarded");
  assert.match(preflight({ tx: act("award_hunt"), sender: sender(F), head: 900, hunt: { ...hunt, status: "awarded" }, recipient: { label: mina, isNew: false } }).refusals[0], /awarded already/);
  // Awarding to a new account burns the creation fee from the creator.
  const opening = preflight({ tx: act("award_hunt"), sender: sender(F), head: 100, hunt, recipient: { label: fresh, isNew: true, creationFee: CREATION } });
  assert.equal(opening.cost, fee + CREATION);
});

test("pending reservations include v1's staking and deposits", () => {
  const pend = (action) => ({ id: action.type, summary: "", status: "pending", height: null, kind: action.type, from: vlad, fee: "4000", action });
  const p = preflight({ tx: transfer(F), sender: sender(30n * F, [owner], [pend({ type: "delegate", validator: "boo1val", amount: String(10n * F) }), pend({ type: "propose", deposit: String(15n * F) })]), head: 1, recipient: { label: mina, isNew: false } });
  assert.equal(p.pending, 25n * F + 8000n);
});

test("transfer_name to oneself says what the chain will do with the name, and refuses the form it isn't (audit review-main13-2)", () => {
  const toSelf = { from: "boo1vlad", fee: "4000", nonce: 3, action: { type: "transfer_name", name: "castle", to: "boo1vlad" } };
  const run = (name, intent) => preflight({ tx: toSelf, sender: sender(F), signer: "k-owner", head: 100, name: { name: "castle.boo", ...name }, intent });
  // Offered to this account: accepting it, whichever form was opened; the "withdraw" form refuses.
  const offered = { owner: "boo1mallory", offeredTo: "boo1vlad", status: "active" };
  assert.equal(run(offered, "accept_name").headline, "Accept castle.boo: it becomes this account's name");
  assert.deepEqual(run(offered, "accept_name").refusals, []);
  const lured = run(offered, "withdraw_name_offer");
  assert.equal(lured.headline, "Accept castle.boo: it becomes this account's name");
  assert.match(lured.refusals.join(" "), /is offered to this account, so this would accept it, not withdraw an offer/);
  // Owned by this account: withdrawing its offer; the "accept" form refuses.
  const mine = { owner: "boo1vlad", offeredTo: "boo1mina", status: "active" };
  assert.equal(run(mine, "withdraw_name_offer").headline, "Withdraw your offer of castle.boo");
  assert.match(run(mine, "accept_name").refusals.join(" "), /owns castle.boo, so this would withdraw its offer/);
  // Neither: the chain refuses it.
  assert.match(run({ owner: "boo1mina", offeredTo: null }, "accept_name").refusals.join(" "), /isn't this account's, and it isn't offered to it/);
  // An expired name can't be accepted, offered, or have its offer withdrawn.
  assert.match(run({ ...offered, status: "grace" }, "accept_name").refusals.join(" "), /has expired/);
  assert.match(run({ ...mine, status: "grace" }, "withdraw_name_offer").refusals.join(" "), /has expired.*renew it first/);
  // TransferName to an owner without an open offer is refused, not a successful no-op.
  const noneOpen = run({ ...mine, offeredTo: undefined }, "withdraw_name_offer");
  assert.match(noneOpen.refusals.join(" "), /no open offer to withdraw/);
  assert.equal(noneOpen.headline, "No open offer of castle.boo to withdraw");
  const offer = { ...toSelf, action: { type: "transfer_name", name: "castle", to: "boo1mina" } };
  assert.match(preflight({ tx: offer, sender: sender(F), signer: "k-owner", head: 100, name: { name: "castle.boo", owner: "boo1mina" } }).refusals.join(" "), /isn't this account's to offer/);
  assert.deepEqual(preflight({ tx: offer, sender: sender(F), signer: "k-owner", head: 100, name: { name: "castle.boo", owner: "boo1vlad", status: "active" } }).refusals, []);
});

test("a signed name resolving to oneself previews acceptance or withdrawal without rewriting it (audit polish P2)", () => {
  const tx = { from: vlad.address, fee: "4000", nonce: 3, action: { type: "transfer_name", name: "castle", to: "vlad.boo" } };
  const account = { account: vlad, balance: String(F), transferable: String(F), nonce: 3, keys: [owner], recent_txs: [] };
  const reads = { status: { height: 100 }, account, recipient: { ...account, exists: true }, name: { name: "castle.boo", owner: mina, offered_to: vlad, status: "active" } };
  const input = preflightInput(tx, "k-owner", reads);
  const accepts = preflight(input);
  assert.equal(accepts.headline, "Accept castle.boo: it becomes this account's name");
  assert.deepEqual(accepts.refusals, []);
  const withdraws = preflight({ ...input, name: { name: "castle.boo", owner: vlad.address, offeredTo: mina.address, status: "active" } });
  assert.equal(withdraws.headline, "Withdraw your offer of castle.boo");
  assert.equal(input.tx.action.to, "vlad.boo", "resolution is forecast evidence, not a replacement signed recipient");
});

test("a lapsed name still in the node's records cannot be offered, accepted or withdrawn (audit polish P3)", () => {
  const tx = { from: vlad.address, fee: "4000", nonce: 3, action: { type: "transfer_name", name: "castle", to: vlad.address } };
  const run = (ownerAddress, offeredTo, to) => preflight({ tx: { ...tx, action: { ...tx.action, to } }, sender: sender(F), head: 100, name: { name: "castle.boo", owner: ownerAddress, offeredTo, status: "available" } });
  assert.match(run(mina.address, vlad.address, vlad.address).refusals.join(" "), /has expired/);
  assert.match(run(vlad.address, mina.address, vlad.address).refusals.join(" "), /has expired/);
  assert.match(run(vlad.address, mina.address, mina.address).refusals.join(" "), /has expired/);
});
