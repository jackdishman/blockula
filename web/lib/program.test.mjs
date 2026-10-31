// Programs in the explorer's ledger (lib/explorer/ledger.ts), held to real answers: program-vectors.json
// (scripts/explorer-program-vectors.mjs) is a dev chain running programs: each transaction as /api/tx served
// it once it ran, receipt included, and the balance of every account it touched before and after. The ledger
// must say every account's change exactly, and what burned. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { deltaFor, effectsOf, knownKind } from "./explorer/ledger.ts";

const { vectors } = JSON.parse((await import("node:fs")).readFileSync(new URL("./program-vectors.json", import.meta.url), "utf8"));
const named = (name) => vectors.find((v) => v.name === name) ?? assert.fail(`no vector "${name}" in program-vectors.json`);
const changeOf = (fx, address) => fx.changes.filter((c) => c.who.address === address).reduce((s, c) => s + c.delta, 0n);

test("the vectors cover uploads, creates, calls (ok, trapped, aborted, payable) and a payout", () => {
  for (const name of ["upload", "create", "a call that traps for want of a storage limit", "a call that writes", "uploading it again", "a payable call", "a payout to a new account", "a call that aborts with a message"]) named(name);
  assert.deepEqual([...new Set(vectors.map((v) => v.tx.kind))].sort(), ["call_program", "create_program", "upload_code"]);
  assert.ok(vectors.every((v) => knownKind(v.tx.kind)));
});

test("every account's change is what the ledger says: the signer's follows the receipt's identity, the others' the VM's diff", () => {
  for (const v of vectors) {
    const fx = effectsOf(v.tx);
    for (const a of v.accounts) {
      const actual = BigInt(v.after[a]) - BigInt(v.before[a]);
      assert.equal(changeOf(fx, a), actual, `${v.name}: ${a === v.signer ? "the signer" : a}'s balance moved ${actual}`);
      assert.equal(deltaFor(v.tx, a).delta, actual, `${v.name}: deltaFor`);
    }
    assert.equal(fx.unseen, null, `${v.name}: a receipt leaves nothing unseen`);
  }
});

test("a failed program transaction costs only its fee, the fuel it reserved included", () => {
  for (const name of ["a call that traps for want of a storage limit", "a call that aborts with a message"]) {
    const v = named(name);
    assert.equal(v.tx.status, "failed");
    const fx = effectsOf(v.tx);
    assert.deepEqual(fx.changes.map((c) => c.delta), [-BigInt(v.tx.fee)]);
    assert.ok(BigInt(v.tx.program.facts.fee.fuel) > 0n, "it paid for what it reserved");
  }
});

test("what burned: the fee's bytes and fuel, the account creation fee, and what a program's own sends burned", () => {
  // These ran with nothing else burning (a validator's own transactions burn too: the chain's counter can run ahead of a ledger's).
  for (const name of ["upload", "create", "a call that writes", "a payable call", "a payout to a new account"]) {
    const v = named(name);
    assert.equal(effectsOf(v.tx).burned.toString(), v.burned, name);
  }
  for (const v of vectors) assert.ok(effectsOf(v.tx).burned <= BigInt(v.burned), `${v.name}: never more than the chain burned`);
  // The payout opened an account from inside the VM: 0.1 FANG came out of the amount, which the receipt states (`burned_by_programs`).
  const payout = effectsOf(named("a payout to a new account").tx);
  assert.equal(payout.burned - BigInt(named("a payout to a new account").tx.fee), 100_000_000n);
  assert.equal(named("a payout to a new account").tx.program.facts.burned_by_programs, "100000000");
  assert.ok(vectors.filter((v) => v.name !== "a payout to a new account").every((v) => v.tx.program.facts.burned_by_programs === "0"), "nothing else burned inside a VM diff");
});

test("the burn the receipt states must be the one conservation implies; a node that predates the field is read by conservation alone", () => {
  const v = named("a payout to a new account").tx;
  const wrong = structuredClone(v);
  wrong.program.facts.burned_by_programs = "99999999";
  assert.match(effectsOf(wrong).unseen, /don't balance/, "a stated burn that isn't what the facts conserve to is never read as money");
  const zero = structuredClone(v);
  zero.program.facts.burned_by_programs = "0";
  assert.match(effectsOf(zero).unseen, /don't balance/, "a burn left out of a receipt that needs one");
  const junk = structuredClone(v);
  junk.program.facts.burned_by_programs = "<img src=x onerror=alert(1)>";
  assert.match(effectsOf(junk).unseen, /don't balance/);
  const old = structuredClone(v);
  delete old.program.facts.burned_by_programs;
  assert.equal(effectsOf(old).unseen, null, "absent: derived from conservation, as before");
  assert.equal(effectsOf(old).burned, effectsOf(v).burned);
});

test("deposits are held, not paid to anyone; the creation fee is in the signer's change; claims move nothing", () => {
  const create = effectsOf(named("create").tx);
  assert.deepEqual(create.changes.map((c) => [c.who.name ?? c.who.short, c.who.holding === true]).slice(1), [["Program record deposit, bonded", true]]);
  assert.equal(create.changes[0].delta, -(65_820n + 25_600_000n + 100_000_000n));
  assert.match(create.changes[0].why, /fee, record deposit and account-creation fee/);
  const upload = effectsOf(named("upload").tx);
  assert.equal(upload.changes[1].who.name, "Code storage deposit, bonded");
  assert.equal(effectsOf(named("uploading it again").tx).changes.length, 1, "stored already: only its fee");
  const open = named("a payable call");
  const fx = effectsOf(open.tx);
  assert.equal(changeOf(fx, open.accounts.find((a) => a !== open.signer && BigInt(open.after[a]) - BigInt(open.before[a]) === 1_000_000_000n)), 1_000_000_000n, "the program holds the FANG sent with the call");
  for (const v of vectors) assert.ok(v.tx.program.claims.return_data.every((r) => typeof r.untrusted_method === "string"), "claims are marked as the deployer's");
});

test("a program transaction that is pending, or whose block is gone, says what it can't show", () => {
  const v = named("a call that writes").tx;
  const pending = effectsOf({ ...v, status: "pending", height: null, program: null });
  assert.equal(pending.pending, true);
  assert.deepEqual(pending.changes.map((c) => c.delta), [-BigInt(v.fee)]);
  assert.match(pending.unseen, /pays for the fuel it reserves when it runs/);
  const pruned = effectsOf({ ...v, program: null });
  assert.match(pruned.unseen, /no longer holds the block it ran in/);
  assert.equal(pruned.unseenFor, "all");
});

test("a receipt that doesn't balance, or has malformed amounts, is never read as money", () => {
  const v = named("a payout to a new account").tx;
  const skewed = structuredClone(v);
  skewed.program.facts.balance_changes[0].change = "100000000000";
  assert.match(effectsOf(skewed).unseen, /don't balance/);
  const junk = structuredClone(v);
  junk.program.facts.balance_changes = [{ account: "boo1x", change: "<img src=x onerror=alert(1)>" }];
  junk.program.facts.outside_vm.record_deposit = "1e30";
  assert.doesNotThrow(() => effectsOf(junk));
});
