// The explorer's ledger and preflight against a protocol v1 dev chain's own answers
// (v1-vectors.json, made by scripts/explorer-vectors.mjs): for each transaction, the reads the
// wallet's preflight took before signing it, the node's /api/tx once it ran (or its refusal), and
// the balances it changed. The preflight must clear what the chain took and refuse what it
// refused, its cost must be what left the sender, and a receipt's balance changes must be what
// the balances did whenever it calls them exact. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { deltaFor, effectsOf } from "./explorer/ledger.ts";
import { preflight, preflightInput } from "./explorer/preflight.ts";

const { vectors } = JSON.parse(readFileSync(new URL("./v1-vectors.json", import.meta.url), "utf8"));
const named = (name) => {
  const v = vectors.find((x) => x.name === name);
  assert.ok(v, `no vector "${name}" in v1-vectors.json`);
  return v;
};
const pf = (v) => preflight(preflightInput(v.tx, v.signer, v.reads));
/** What the balance of `who` did. */
const moved = (v, who) => BigInt(v.after[who]) - BigInt(v.before[who]);

const ran = vectors.filter((v) => v.result?.status === "finalized" && v.before);
const refused = vectors.filter((v) => v.refused);

test("the vectors cover what the explorer reads (v1-vectors.json)", () => {
  assert.ok(ran.length >= 20, `${ran.length} sealed`);
  assert.ok(refused.length >= 6, `${refused.length} refused`);
  for (const k of ["transfer", "register_name", "renew_name", "transfer_name", "release_name", "delegate", "undelegate", "propose", "vote", "fund_pool", "create_hunt", "fund_hunt", "award_hunt", "multisig_propose"]) assert.ok(ran.some((v) => v.result.kind === k), k);
});

test("the preflight clears every transaction the chain took, and its cost is what left the sender", () => {
  for (const v of ran) {
    const p = pf(v);
    assert.deepEqual(p.refusals, [], v.name);
    // Signed at most `max_fee`, charged `fee` (the including block's base fee + the tip).
    const charged = p.cost - (BigInt(v.result.max_fee) - BigInt(v.result.fee));
    assert.equal(-moved(v, v.tx.from), charged, `${v.name}: what left the sender`);
  }
});

test("the preflight refuses what the chain refused, for the same reason", () => {
  const why = {
    "pay more than a vote lock leaves": /is locked .*locked FANG can be staked, not moved out/,
    "cancel a hunt before it expires": /can be cancelled by its creator after block/,
    "award a hunt as someone else": /Only hunt #\d+'s creator can award it/,
    "can't pay the creation fee": /Not enough FANG: this needs/,
    "an agent opening an account goes over its budget": /Over the agent key's budget.*opening the recipient's account \(0\.1 FANG\) included/,
    "a multisig's owner key pays": /2-of-2 multisig: its owner keys can only propose/,
  };
  assert.deepEqual(refused.map((v) => v.name).sort(), Object.keys(why).sort());
  for (const v of refused) assert.match(pf(v).refusals.join(" "), why[v.name], `${v.name} (the node: ${v.refused})`);
});

test("opening an account: the creation fee leaves the sender, and counts against an agent's budget", () => {
  const v = named("transfer opening an account");
  const fee = BigInt(v.reads.recipient.creation_fee);
  assert.equal(v.reads.recipient.exists, false);
  assert.ok(fee > 0n);
  const p = pf(v);
  assert.equal(p.effects.burned, BigInt(v.result.fee) + fee);
  assert.match(p.notes.join(" "), /Opening its account burns 0\.1 FANG/);
  const agent = pf(named("an agent pays within its budget"));
  assert.ok(agent.budget && agent.budget.spentAfter <= agent.budget.limit);
  const award = named("award a hunt to a new account");
  assert.equal(-moved(award, award.tx.from), BigInt(award.result.fee) + BigInt(award.reads.recipient.creation_fee), "the creator burns it, the escrow pays the bounty");
});

test("a sealed transaction's balance changes are what the balances did, whenever they're called exact", () => {
  for (const v of ran) {
    for (const who of Object.keys(v.before)) {
      const d = deltaFor(v.result, who);
      if (d.exact) assert.equal(d.delta, moved(v, who), `${v.name}: ${who.slice(0, 12)}…`);
    }
  }
  // What's called inexact really is (the fields don't carry it), and says why.
  for (const name of ["transfer opening an account", "register a name", "renew a name", "award a hunt to a new account"]) {
    const v = named(name);
    const d = deltaFor(v.result, v.tx.from);
    assert.equal(d.exact, false, name);
    assert.notEqual(d.delta, moved(v, v.tx.from), name);
    assert.ok(effectsOf(v.result).unseen, name);
  }
  // Knowing the recipient was new settles it.
  const opened = named("transfer opening an account");
  const known = deltaFor(opened.result, opened.tx.from, { creationFee: BigInt(opened.reads.recipient.creation_fee) });
  assert.equal(known.exact, true);
  assert.equal(known.delta, moved(opened, opened.tx.from));
});

test("a tip goes to the block's proposer and the rest of the fee burns, exactly", () => {
  const v = named("transfer with a tip");
  const fx = effectsOf(v.result);
  assert.equal(BigInt(v.result.tip), 100_000n);
  assert.equal(v.result.fee, v.result.max_fee, "charged what it allowed: the usual case");
  assert.equal(fx.burned, BigInt(v.result.fee) - 100_000n);
  assert.deepEqual(fx.changes.find((c) => c.who.address === "holding:proposer")?.delta, 100_000n);
  // Nothing about the tip is left unknown; only whether paying the address opened its account
  // (the sender's side), until the recipient's history says it existed.
  assert.equal(fx.unseenFor, "sender");
  assert.equal(effectsOf(v.result, { recipientExisted: true }).unseen, null);
  assert.equal(deltaFor(v.result, v.tx.from, { recipientExisted: true }).delta, moved(v, v.tx.from));
});

test("a payment signed to a name is credited to whoever owned it when it ran", () => {
  const v = named("transfer signed to a name");
  assert.match(v.tx.action.to, /\.boo$/);
  assert.equal(v.result.to_name, v.tx.action.to);
  const owner = v.result.to.address;
  assert.equal(deltaFor(v.result, owner).delta, moved(v, owner));
  assert.equal(deltaFor(v.result, owner).direction, "in");
});

test("a vote locks without spending; staking locked FANG is fine", () => {
  const vote = named("vote");
  assert.equal(deltaFor(vote.result, vote.tx.from).delta, -BigInt(vote.result.fee));
  assert.match(effectsOf(vote.result).note, /locks 60 FANG/);
  const stake = named("stake locked FANG");
  assert.ok(BigInt(stake.reads.account.locked) > 0n);
  assert.deepEqual(pf(stake).refusals, []);
});

test("a transaction dropped unrun changed nothing", () => {
  const v = named("dropped in a race");
  assert.equal(v.result.status, "failed");
  assert.equal(v.result.dropped, true);
  assert.equal(v.result.from, undefined);
  const fx = effectsOf(v.result);
  assert.deepEqual(fx.changes, []);
  assert.equal(fx.unseen, null);
});
