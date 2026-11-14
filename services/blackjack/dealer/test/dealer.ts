// The dealer's decisions, offline, against a table in memory (fake.ts) and a real store on a
// temporary disk: commit planning, reveal planning for every phase, timeouts, transactions whose
// fate isn't known, restarts, a lost store, a dry run, and that no secret reaches the log.
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Outcome, Phase, call, chainFromTop, dealers, hexBytes, owedLinks, verifyHand, type Game, type TableCall } from "@blockula/blackjack";
import { Dealer, FUEL, inspect, type DealerOptions } from "../src/dealer.js";
import { jsonLogger, scrub } from "../src/log.js";
import { SeedStore } from "../src/store.js";
import { DEALER, FakeTable, type Fate } from "./fake.js";

const dir = mkdtempSync(join(tmpdir(), "bj-dealer-test."));
const TABLE = "boo1table";
const key = randomBytes(32);
/** Every log line of every test, and every secret any store ever held: the last test compares them. */
const allLogs: string[] = [];
const allTops = new Set<string>();
let places = 0;

interface Rig {
  table: FakeTable;
  store: SeedStore;
  dealer: Dealer;
  logs: Record<string, unknown>[];
  at: { path: string; backupPath: string; key: Buffer; table: string };
  /** One tick, then everything it started comes back. */
  step(): Promise<string[]>;
  /** A new dealer over the same table and the same store files: a restart. */
  restart(opts?: Partial<DealerOptions>): Rig;
}

const OPTS: DealerOptions = { lowWater: 8, target: 16, batch: 64, dryRun: false, keepSettled: 4, lowBalance: 5_000_000_000n, txValidBlocks: 100, pruneBatch: 4 };

function rig(opts: Partial<DealerOptions> = {}, from?: { table: FakeTable; at: Rig["at"] }): Rig {
  const table = from?.table ?? new FakeTable();
  const at = from?.at ?? { path: join(dir, `d${places}`, "seeds.enc.json"), backupPath: join(dir, `d${places++}`, "backup.enc.json"), key, table: TABLE };
  const store = SeedStore.open(at);
  for (const k of store.indexes()) allTops.add(store.top(k)!);
  const logs: Record<string, unknown>[] = [];
  const log = jsonLogger((line) => {
    allLogs.push(line);
    logs.push(JSON.parse(line) as Record<string, unknown>);
  });
  // Every secret the dealer draws is remembered, to look for in the logs at the end.
  const random = () => {
    const top = randomBytes(32).toString("hex");
    allTops.add(top);
    return top;
  };
  const all = { ...OPTS, random, ...opts };
  const dealer = new Dealer(table, table, store, log, all);
  const r: Rig = {
    table,
    store,
    dealer,
    logs,
    at,
    async step() {
      const t = await dealer.tick();
      await dealer.drain();
      return t.started;
    },
    restart(o = {}) {
      store.close();
      return rig({ ...opts, ...o }, { table, at });
    },
  };
  return r;
}

const actions = (r: Rig) => r.logs.map((l) => l.action as string);
const sent = (r: Rig, method: string) => r.table.sends.filter((s) => s.call.method === method);
const only = (method: string, fate: Fate) => (c: TableCall): Fate => (c.method === method ? fate : "ok");
const seed = () => randomBytes(32).toString("hex");
/** The chain's commitment for game k is the one the store's secret hashes to. */
const committed = (r: Rig, k: bigint) => (r.table.commitments.get(k) ?? r.table.commitOf.get(k)) === chainFromTop(r.store.top(k)!)[0];
/** Steps until the table has no open hand, moving for the players: hit below 17. */
async function playOut(r: Rig, max = 60): Promise<void> {
  for (let i = 0; i < max && r.table.open.length; i++) {
    await r.step();
    for (const id of [...r.table.open]) {
      const g = r.table.games.get(id)!;
      if (g.phase === Phase.Player) r.table.move(id, g.playerTotal < 17);
    }
  }
  assert.equal(r.table.open.length, 0, "every hand finished");
}

try {
  // ---- commits ahead of demand, in batches, each secret on disk (and in the backup) first
  {
    const r = rig({ lowWater: 100, target: 150, batch: 64 });
    // At the moment a batch is sent, a store opened from the files alone holds every one of its secrets.
    r.table.beforeApply = (c) => {
      if (c.method !== "commit_batch") return;
      for (const file of [r.at.path, r.at.backupPath]) {
        const onDisk = SeedStore.open({ ...r.at, path: file, backupPath: `${file}.other`, readOnly: true });
        const b = hexBytes(c.input);
        const first = new DataView(b.buffer, b.byteOffset).getBigUint64(0, true);
        const n = new DataView(b.buffer, b.byteOffset).getUint32(8, true);
        for (let i = 0; i < n; i++) {
          const top = onDisk.top(first + BigInt(i));
          assert.ok(top !== undefined, `game ${first + BigInt(i)}'s secret is in ${file} before its commitment is sent`);
          assert.equal(chainFromTop(top)[0], Buffer.from(b.subarray(12 + 32 * i, 44 + 32 * i)).toString("hex"));
        }
      }
    };
    await r.dealer.start();
    assert.deepEqual(await r.step(), ["commit:0"]);
    assert.equal(r.table.nextCommit, 64n);
    assert.deepEqual(await r.step(), ["commit:64"]);
    assert.deepEqual(await r.step(), ["commit:128"]);
    assert.equal(r.table.nextCommit, 150n, "topped up to the target, 64 at most at a time");
    assert.deepEqual(await r.step(), [], "enough: nothing more");
    assert.deepEqual(sent(r, "commit_batch").map((s) => s.call.input.length), [(12 + 32 * 64) * 2, (12 + 32 * 64) * 2, (12 + 32 * 22) * 2]);
    for (let k = 0n; k < 150n; k++) assert.ok(committed(r, k));
    // Bets take commitments; nothing is committed until fewer than lowWater are left.
    r.table.beforeApply = () => undefined;
    for (let i = 0; i < 50; i++) {
      r.table.bet("boo1alice", seed());
      await playOut(r);
    }
    assert.equal(r.table.nextCommit, 150n, "100 left: not under the low-water mark");
    r.table.bet("boo1alice", seed());
    await playOut(r);
    assert.equal(r.table.nextCommit, 201n, "99 left: topped up to 150 unused");
    assert.equal(actions(r).filter((a) => a === "commit").length, 4);
    console.log("✓ commitments are topped up under lowWater to target, in batches, each secret stored and backed up first");
  }

  // ---- a batch someone already committed (abort 61): re-read and carry on with the stored secrets
  {
    const r = rig();
    await r.step();
    assert.equal(r.table.nextCommit, 16n);
    // An earlier run of the daemon stored secrets for 16..23 and its batch is about to land,
    // unseen by this one, which plans the same indexes.
    for (let i = 0; i < 12; i++) r.table.bet("boo1alice", seed());
    const earlier = new Map(Array.from({ length: 8 }, (_, i) => [16n + BigInt(i), seed()] as const));
    r.store.put(earlier);
    for (const top of earlier.values()) allTops.add(top);
    let raced = false;
    r.table.beforeApply = (c) => {
      if (c.method !== "commit_batch" || raced) return;
      raced = true;
      r.table.call(call.commitBatch(16n, [...earlier.values()].map((top) => chainFromTop(top)[0])), DEALER);
    };
    const started = await r.step();
    assert.ok(started.includes("commit:16"));
    const refused = r.logs.find((l) => l.action === "commit_refused")!;
    assert.deepEqual([refused.abort, refused.moved, refused.level], [61, true, "info"]);
    assert.equal(r.table.nextCommit, 24n, "the earlier batch stands");
    // The next look starts where the chain is now, with the secrets the store already had.
    const before = sent(r, "commit_batch").length;
    for (let i = 0; i < 5; i++) r.table.bet("boo1bob", seed());
    await playOut(r);
    assert.ok(sent(r, "commit_batch").length > before);
    for (let k = r.table.nextGame; k < r.table.nextCommit; k++) assert.ok(committed(r, k), `game ${k}'s commitment is its stored secret's`);
    for (const [k, top] of earlier) assert.equal(r.store.top(k), top, "a stored secret is never replaced");
    console.log("✓ abort 61 (someone committed first): re-read, and the same secrets serve the same indexes");
  }

  // ---- a commit whose fate isn't known is followed by id, never blindly resent
  {
    // It landed, but the answer was lost.
    let r = rig();
    r.table.fate = only("commit_batch", "ghost");
    await r.step();
    assert.equal(r.table.nextCommit, 16n);
    assert.deepEqual(actions(r), ["commit_unconfirmed"]);
    r.table.fate = () => "ok";
    await r.step();
    assert.equal(sent(r, "commit_batch").length, 1, "found final by its id: not sent again");
    assert.equal(r.logs.at(-1)!.action, "commit");
    assert.equal(r.logs.at(-1)!.count, 16);

    // The node holds it: waited for, however long, until it lands.
    r = rig();
    r.table.fate = only("commit_batch", "held");
    await r.step();
    for (let i = 0; i < 10; i++) assert.deepEqual(await r.step(), [], "a held commit isn't sent again");
    assert.equal(sent(r, "commit_batch").length, 1);
    r.table.release(r.table.sends[0].id);
    await r.step();
    assert.equal(r.table.nextCommit, 16n);
    assert.equal(sent(r, "commit_batch").length, 1);

    // The node holds it and then it expires unrun: planned again, with the same commitments.
    r = rig({ txValidBlocks: 5 });
    r.table.fate = only("commit_batch", "held");
    await r.step();
    r.table.drop(r.table.sends[0].id);
    r.table.fate = () => "held";
    r.table.mine(6);
    r.table.fate = () => "ok";
    await r.step(); // the follow-up finds it expired…
    await r.step(); // …and the batch is planned again
    assert.equal(sent(r, "commit_batch").length, 2);
    assert.equal(sent(r, "commit_batch")[1].call.input, sent(r, "commit_batch")[0].call.input, "the same chains, not new ones");
    assert.equal(r.table.nextCommit, 16n);

    // It never reached the node: after a few checks it counts as lost and is planned again, the same.
    r = rig({ lostAfterChecks: 3 });
    r.table.fate = only("commit_batch", "lost");
    await r.step();
    r.table.fate = () => "ok";
    assert.deepEqual(await r.step(), []);
    assert.deepEqual(await r.step(), []);
    assert.equal(sent(r, "commit_batch").length, 1, "not resent while it may still arrive");
    await r.step();
    assert.ok(actions(r).includes("commit_lost"));
    await r.step();
    assert.equal(sent(r, "commit_batch").length, 2);
    assert.equal(sent(r, "commit_batch")[1].call.input, sent(r, "commit_batch")[0].call.input);
    assert.equal(r.table.nextCommit, 16n);

    // The node refused it, or the sender broke: backed off, then the same batch again.
    for (const fate of ["unsent", "throw"] as const) {
      r = rig();
      r.table.fate = only("commit_batch", fate);
      await r.step();
      assert.deepEqual(actions(r), ["commit_not_sent"]);
      assert.deepEqual(await r.step(), [], "a failed call waits before it's tried again");
      r.table.fate = () => "ok";
      for (let i = 0; i < 4 && r.table.nextCommit === 0n; i++) await r.step();
      assert.equal(r.table.nextCommit, 16n);
      assert.equal(sent(r, "commit_batch")[1].call.input, sent(r, "commit_batch")[0].call.input);
    }
    console.log("✓ an unconfirmed commit is followed by its id; a lost or expired one is planned again with the same chains");
  }

  // ---- reveals: exactly the owed links, for every phase, and hands play to the end
  {
    const r = rig({ lowWater: 20, target: 40 });
    await r.step();
    const seen = new Set<number>();
    const outcomes = new Set<number>();
    for (let n = 0; n < 300; n++) {
      const id = r.table.bet("boo1alice", seed());
      for (;;) {
        const g = r.table.games.get(id);
        if (!g) break;
        seen.add(g.phase);
        if (dealers(g.phase)) {
          const owed = owedLinks(g, chainFromTop(r.store.top(id)!));
          const started = await r.step();
          assert.ok(started.includes(`reveal:${id}:${g.phase}:${g.dealt}`));
          const s = sent(r, "reveal").at(-1)!;
          assert.equal(s.call.input, call.reveal(id, owed).input, "one reveal, with exactly the links owed");
          const after = r.table.games.get(id);
          assert.ok(!after || after.phase === Phase.Player, "one reveal takes the hand to the player's move or its end");
        } else {
          // The player's turn, inside their deadline: nothing is sent.
          const before = r.table.sends.length;
          await r.step();
          assert.equal(r.table.sends.filter((s) => s.call.method !== "commit_batch").length, r.table.sends.slice(0, before).filter((s) => s.call.method !== "commit_batch").length, "nothing for a hand that is the player's to move");
          r.table.move(id, g.playerTotal < 17);
        }
      }
    }
    assert.deepEqual([...seen].sort(), [Phase.Deal, Phase.Player, Phase.Hit, Phase.Dealer]);
    assert.equal(r.table.settled.length, 300);
    for (const h of r.table.settled) {
      outcomes.add(h.outcome);
      assert.equal(verifyHand(h).forfeited, false, "no hand ended by a missed deadline");
      assert.equal(h.by, DEALER);
    }
    for (const o of [Outcome.Blackjack, Outcome.Win, Outcome.Push, Outcome.Lose, Outcome.Bust]) assert.ok(outcomes.has(o), `outcome ${o} came up in 300 hands`);
    assert.equal(sent(r, "timeout").length, 0);
    // The log of a reveal: the game, the tx, the height, the cards and the outcome.
    const settledLine = [...r.logs].reverse().find((l) => l.action === "reveal" && l.outcome !== undefined)!;
    assert.match(String(settledLine.tx), /^[0-9a-f]{64}$/);
    for (const field of ["time", "level", "game", "height", "cards", "phase", "stake", "payout"]) assert.ok(field in settledLine, `a reveal's log line has ${field}`);
    console.log(`✓ 300 hands dealt by exactly the owed links through every phase (outcomes ${[...outcomes].sort().join(", ")}), each verified`);
  }

  // ---- fuel: each call reserves what it uses and a margin; one that runs out goes again with the maximum
  {
    const r = rig();
    await r.step();
    assert.equal(sent(r, "commit_batch")[0].fuelLimit, FUEL.commit(16));
    const id = r.table.bet("boo1alice", seed());
    r.table.fate = only("reveal", "dry");
    await r.step();
    assert.equal(sent(r, "reveal")[0].fuelLimit, FUEL.reveal(owedLinks({ ...r.table.games.get(id)!, phase: Phase.Deal, dealt: 0, playerCards: [], dealerCards: [] }, chainFromTop(r.store.top(id)!)).length));
    assert.ok(actions(r).includes("reveal_out_of_fuel"));
    assert.equal(r.table.games.get(id)!.dealt, 0);
    // The very next look sends it again, with no limit of its own, and it lands.
    await r.step();
    assert.equal(sent(r, "reveal")[1].fuelLimit, undefined);
    assert.equal(sent(r, "reveal")[1].call.input, sent(r, "reveal")[0].call.input);
    assert.ok((r.table.games.get(id)?.dealt ?? 3) >= 3);
    r.table.fate = () => "ok";
    await playOut(r);
    // `fullFuel` reserves the maximum for every call.
    const full = rig({ fullFuel: true });
    await full.step();
    full.table.bet("boo1alice", seed());
    await playOut(full);
    assert.ok(full.table.sends.every((s) => s.fuelLimit === undefined));
    console.log("✓ calls reserve a measured fuel limit; one that runs out is sent again with the chain's maximum");
  }

  // ---- several hands: the one closest to its deadline goes first
  {
    const r = rig();
    await r.step();
    const a = r.table.bet("boo1alice", seed());
    r.table.mine(3);
    const b = r.table.bet("boo1bob", seed());
    const c = r.table.bet("boo1carol", seed());
    // Carol's deadline is made the nearest.
    r.table.games.set(c, { ...r.table.games.get(c)!, deadline: r.table.games.get(a)!.deadline - 1n });
    const from = r.table.sends.length;
    const started = await r.step();
    assert.deepEqual(started.slice(0, 3), [`reveal:${c}:1:0`, `reveal:${a}:1:0`, `reveal:${b}:1:0`]);
    assert.deepEqual(r.table.sends.slice(from, from + 3).map((s) => new DataView(hexBytes(s.call.input).buffer).getBigUint64(0, true)), [c, a, b]);
    await playOut(r);
    console.log("✓ open hands are dealt oldest deadline first");
  }

  // ---- the table moved between the read and the call: no loop, the next read sorts it out
  {
    const r = rig();
    await r.step();
    const id = r.table.bet("boo1alice", seed());
    const chain = chainFromTop(r.store.top(id)!);
    // Someone who knows the links (an earlier run's transaction arriving late) reveals first.
    let raced = false;
    r.table.beforeApply = (c) => {
      if (c.method !== "reveal" || raced) return;
      raced = true;
      r.table.call(call.reveal(id, [chain[1]]), "boo1stranger");
    };
    await r.step();
    // The program passes over the link the hand already holds and deals the rest (BJ-08): the
    // dealer's transaction isn't refused, and the deal is done in that one call.
    assert.equal(raced, true);
    assert.equal(r.logs.find((l) => l.action === "reveal_refused"), undefined);
    const dealt = r.logs.filter((l) => l.action === "reveal").at(-1)!;
    assert.equal((dealt.cards as number[]).length, 2, "the two cards left of the deal");
    const g = r.table.games.get(id);
    if (g) assert.deepEqual([g.dealt, g.phase], [3, Phase.Player]);
    assert.equal(sent(r, "reveal").length, 1);

    // Someone reveals every link the dealer was about to: its call changes nothing and isn't an
    // error, and the next read finds the hand moved on.
    const ra = rig();
    await ra.step();
    const all = ra.table.bet("boo1alice", seed());
    const ca = chainFromTop(ra.store.top(all)!);
    let copied = false;
    ra.table.beforeApply = (c) => {
      if (c.method !== "reveal" || copied) return;
      copied = true;
      ra.table.call(c, "boo1stranger");
    };
    await ra.step();
    assert.equal(copied, true);
    assert.equal(ra.logs.find((l) => l.action === "reveal_refused"), undefined);
    assert.equal(ra.logs.filter((l) => l.action === "reveal").at(-1)!.cards, undefined, "nothing dealt by the dealer's own call");
    const ga = ra.table.games.get(all);
    if (ga) assert.equal(ga.link, ca[3]);
    // A link that is neither held nor next is still refused, as it was.
    const rb = rig();
    await rb.step();
    const other = rb.table.bet("boo1alice", seed());
    const cb = chainFromTop(rb.store.top(other)!);
    assert.throws(() => rb.table.call(call.reveal(other, [cb[2]]), "boo1stranger"));
    assert.throws(() => rb.table.call(call.reveal(other, [cb[0]]), "boo1stranger"));

    // A reveal that keeps being refused with the table unchanged (a broken node, say) backs off
    // instead of being sent every tick.
    const r2 = rig();
    await r2.step();
    r2.table.bet("boo1alice", seed());
    r2.table.fate = only("reveal", "unsent");
    for (let i = 0; i < 40; i++) await r2.step();
    assert.ok(sent(r2, "reveal").length <= 7, `40 ticks, ${sent(r2, "reveal").length} tries: it backs off`);
    r2.table.fate = () => "ok";
    await playOut(r2, 200);

    // A hand settled by someone else while our reveal was on its way: abort 54, and nothing more.
    const r3 = rig();
    await r3.step();
    const gone = r3.table.bet("boo1alice", seed());
    const c3 = chainFromTop(r3.store.top(gone)!);
    r3.table.beforeApply = (c) => {
      if (c.method !== "reveal" || !r3.table.games.has(gone)) return;
      // The whole hand, to its end, from another sender.
      for (;;) {
        const g3 = r3.table.games.get(gone);
        if (!g3) break;
        if (g3.phase === Phase.Player) r3.table.move(gone, false);
        else r3.table.call(call.reveal(gone, owedLinks(g3, c3)), "boo1stranger");
      }
    };
    await r3.step();
    assert.equal(r3.logs.find((l) => l.action === "reveal_refused")!.abort, 54);
    const tries = sent(r3, "reveal").length;
    for (let i = 0; i < 5; i++) await r3.step();
    assert.equal(sent(r3, "reveal").length, tries, "a settled hand is left alone");
    console.log("✓ a reveal someone got ahead of deals the rest (links already held are passed over); one for a settled hand (54) isn't retried; a failing one backs off");
  }

  // ---- an unconfirmed reveal is followed by id; the hand still finishes
  {
    const r = rig();
    await r.step();
    const id = r.table.bet("boo1alice", seed());
    r.table.fate = only("reveal", "ghost");
    await r.step();
    assert.ok(actions(r).includes("reveal_unconfirmed"));
    r.table.fate = () => "ok";
    await playOut(r);
    assert.equal(r.table.settled.at(-1)!.game, id);
    assert.equal(sent(r, "reveal").filter((s) => s.fate === "ghost").length, 1);

    const lost = r.table.bet("boo1alice", seed());
    r.table.fate = only("reveal", "lost");
    await r.step();
    r.table.fate = () => "ok";
    await playOut(r);
    assert.equal(r.table.settled.at(-1)!.game, lost);
    assert.equal(verifyHand(r.table.settled.at(-1)!).forfeited, false);
    console.log("✓ a reveal whose answer was lost, or that never arrived, still ends in a dealt hand");
  }

  // ---- timeouts: an idle player is stood; a hand in the dealer's phase is never timed out
  {
    const r = rig();
    await r.step();
    let id = 0n;
    // A hand that reaches the player's move.
    for (;;) {
      id = r.table.bet("boo1alice", seed());
      await r.step();
      if (r.table.games.get(id)?.phase === Phase.Player) break;
      await playOut(r);
    }
    const g = r.table.games.get(id)!;
    r.table.mine(Number(g.deadline) - r.table.height - 1);
    assert.deepEqual(await r.step(), [], "one block before the deadline: nothing");
    r.table.mine(1);
    assert.deepEqual(await r.step(), [`timeout:${id}:${g.dealt}`], "at the deadline: the timeout lands in the block after it");
    const t = sent(r, "timeout")[0];
    assert.equal(t.call.input, call.timeout(id).input);
    // Short-lived: half of reveal_blocks, so a late copy can't meet the hand past a dealer's deadline.
    assert.equal(t.validForBlocks, 10);
    const stood = r.logs.find((l) => l.action === "timeout")!;
    assert.deepEqual([stood.stood, stood.game], ["idle", id.toString()]);
    assert.equal(r.table.games.get(id)!.phase, Phase.Dealer);
    await r.step();
    assert.equal(r.table.settled.at(-1)!.game, id);
    assert.equal(verifyHand(r.table.settled.at(-1)!).forfeited, false, "the idle player stood; the dealer finished the hand");

    // Hands in the dealer's phases, past their deadlines, with every way a reveal can fail:
    // whatever happens, the dealer never sends a timeout for them (it would pay the player).
    for (const fate of ["lost", "held", "unsent", "throw"] as const) {
      const before = sent(r, "timeout").length;
      const stuck = r.table.bet("boo1alice", seed());
      r.table.fate = only("reveal", fate);
      for (let i = 0; i < 12; i++) {
        r.table.mine(5);
        await r.step();
        assert.equal(r.table.games.get(stuck)!.phase, Phase.Deal);
      }
      assert.equal(sent(r, "timeout").length, before, `a ${fate} reveal past its deadline: no timeout from the dealer`);
      assert.ok(actions(r).includes("past_deadline"));
      for (const s of r.table.sends.filter((x) => x.fate === "held")) r.table.drop(s.id);
      r.table.fate = () => "ok";
      await playOut(r, 300);
    }
    // And across every test: each timeout the dealer ever sent found its hand in the player's phase.
    assert.ok(r.table.settled.every((h) => h.by === DEALER && !verifyHand(h).forfeited));
    console.log("✓ an idle player is stood at their deadline; a hand in the dealer's phase is never timed out");
  }

  // ---- a restart: a new dealer over the same store and chain does the same, and never commits twice
  {
    // Killed between storing a batch's secrets and sending it.
    let r = rig();
    r.table.fate = only("commit_batch", "throw");
    await r.step();
    assert.equal(r.table.nextCommit, 0n);
    const stored = new Map(r.store.indexes().map((k) => [k, r.store.top(k)!] as const));
    assert.equal(stored.size, 16);
    r.table.fate = () => "ok";
    r = r.restart();
    const h = await r.dealer.start();
    assert.deepEqual([h.unused, h.uncommitted, h.stored], [0, 16, 16]);
    await r.step();
    assert.equal(r.table.nextCommit, 16n);
    for (const [k, top] of stored) assert.equal(r.table.commitments.get(k), chainFromTop(top)[0], "the restart committed the chains the first run stored");

    // Killed after a batch was sent and before its answer: the batch landed.
    r.table.fate = only("commit_batch", "ghost");
    for (let i = 0; i < 10; i++) r.table.bet("boo1alice", seed());
    await r.step();
    assert.equal(r.table.nextCommit, 26n);
    const commits = sent(r, "commit_batch").length;
    r.table.fate = () => "ok";
    r = r.restart();
    await r.dealer.start();
    await playOut(r);
    assert.equal(sent(r, "commit_batch").length, commits, "nothing is committed twice");
    for (let k = r.table.nextGame; k < r.table.nextCommit; k++) assert.ok(committed(r, k));

    // Killed with hands in every state: the new dealer picks each up where the chain has it.
    const ids = [r.table.bet("boo1alice", seed()), r.table.bet("boo1bob", seed()), r.table.bet("boo1carol", seed())];
    await r.step();
    r.table.bet("boo1dave", seed());
    for (const id of ids) {
      const g = r.table.games.get(id);
      if (g?.phase === Phase.Player) r.table.move(id, id % 2n === 0n);
    }
    r = r.restart();
    await r.dealer.start();
    const plan = (await r.dealer.tick()).started;
    await r.dealer.drain();
    assert.ok(plan.length >= 1);
    await playOut(r);
    assert.ok(r.table.settled.every((s) => !verifyHand(s).forfeited));
    console.log("✓ a restart commits the stored chains, never a second set, and finishes every open hand");
  }

  // ---- two fresh dealers over the same state plan the same calls
  {
    const r = rig();
    await r.step();
    r.table.bet("boo1alice", seed());
    r.table.bet("boo1bob", seed());
    r.table.fate = () => "lost";
    const first = await r.step();
    const inputs = r.table.sends.slice(-first.length).map((s) => s.call.input);
    const again = r.restart();
    again.table.fate = () => "lost";
    const second = await again.step();
    assert.deepEqual(second, first);
    assert.deepEqual(again.table.sends.slice(-second.length).map((s) => s.call.input), inputs);
    console.log("✓ a new dealer over the same store and chain sends byte-for-byte the same calls");
  }

  // ---- a lost store: say so, loudly, and keep dealing what can be dealt
  {
    let r = rig();
    await r.step();
    const kept = r.table.bet("boo1alice", seed());
    await r.step();
    // The store and its backup are gone; the daemon starts on an empty one.
    r.store.close();
    rmSync(r.at.path);
    rmSync(r.at.backupPath);
    const table = r.table;
    r = rig({}, { table, at: r.at });
    const h = await r.dealer.start();
    assert.deepEqual([h.unused, h.servable, h.missing], [15, 0, [{ from: 1n, to: 15n }]]);
    // (A natural paid at the deal leaves no open hand: one hand in twenty or so.)
    if (h.open.length) assert.equal(h.open[0].hasSecret, false);
    await r.step();
    const loud = r.logs.find((l) => l.action === "lost_secrets")!;
    assert.equal(loud.level, "error");
    assert.equal(loud.games, "1–15");
    assert.match(String(loud.advice), /skip_commits/);
    assert.match(String(loud.advice), /until game 16 is next/);
    // It commits new chains past the lost ones, so the table works again once the owner skips.
    assert.equal(r.table.nextCommit, 32n, "16 it can deal, on top of the 15 it can't");
    // The hand whose secret is gone: an error, no reveal, and never a timeout from the dealer.
    if (table.games.get(kept) && dealers(table.games.get(kept)!.phase)) assert.ok(actions(r).includes("hand_without_secret"));
    const lostHand = table.bet("boo1bob", seed());
    table.mine(40);
    for (let i = 0; i < 5; i++) await r.step();
    assert.ok(actions(r).includes("hand_without_secret"));
    assert.equal(sent(r, "reveal").filter((s) => s.call.input.startsWith(call.timeout(lostHand).input)).length, 0);
    assert.equal(sent(r, "timeout").filter((s) => s.call.input === call.timeout(lostHand).input).length, 0, "never a timeout for a hand the dealer owes cards on");
    assert.equal(table.games.get(lostHand)!.phase, Phase.Deal);
    // Someone else calls the timeout: the player is paid, as the rules say.
    table.call(call.timeout(lostHand), "boo1stranger");
    assert.equal(table.settled.at(-1)!.outcome, Outcome.ForfeitDeal);
    table.mine(25);
    if (table.games.has(kept)) table.call(call.timeout(kept), "boo1stranger");
    // The owner drops the lost commitments; the dealer serves every hand after that.
    table.skip(14);
    assert.equal(table.nextGame, 16n);
    const served = table.bet("boo1carol", seed());
    await playOut(r);
    const last = table.settled.at(-1)!;
    assert.equal(last.game, served);
    assert.equal(verifyHand(last).forfeited, false);

    // A store whose secrets are another chain's (a wrong file put back): found at start, said, not used.
    const r2 = rig();
    await r2.step();
    r2.store.close();
    rmSync(r2.at.path);
    rmSync(r2.at.backupPath);
    const wrong = SeedStore.open(r2.at);
    wrong.put(new Map(Array.from({ length: 16 }, (_, i) => [BigInt(i), seed()] as const)));
    wrong.close();
    const r3 = rig({}, { table: r2.table, at: r2.at });
    const h3 = await r3.dealer.start();
    assert.equal(h3.mismatched.length, 16);
    assert.equal(h3.servable, 0);
    assert.ok(actions(r3).includes("secrets_mismatch"));
    const bad = r3.table.bet("boo1alice", seed());
    await r3.step();
    assert.equal(sent(r3, "reveal").length, 0, "a secret that isn't the chain's is never revealed");
    assert.equal(r3.table.games.get(bad)!.phase, Phase.Deal);
    console.log("✓ a lost store: a loud error naming the games and skip_commits, no timeout, and dealing resumes once they're skipped");
  }

  // ---- not the table's dealer: no commits, but hands it holds secrets for are still dealt
  {
    const r = rig();
    await r.step();
    const id = r.table.bet("boo1alice", seed());
    r.table.dealer = "boo1someoneelse";
    for (let i = 0; i < 10; i++) r.table.bet(`boo1p${i}`, seed());
    await playOut(r);
    assert.ok(actions(r).includes("not_the_dealer"));
    assert.equal(sent(r, "commit_batch").length, 1, "no commit from an account that isn't the dealer");
    assert.ok(r.table.settled.some((s) => s.game === id));
    console.log("✓ an account that isn't the table's dealer doesn't commit, and still deals");
  }

  // ---- a dry run reads and logs, sends nothing and writes nothing
  {
    const live = rig();
    await live.step();
    live.table.bet("boo1alice", seed());
    live.table.bet("boo1bob", seed());
    const idle = live.table.bet("boo1carol", seed());
    await live.step();
    if (live.table.games.get(idle)?.phase === Phase.Player) live.table.mine(40);
    for (let i = 0; i < 12; i++) live.table.bet(`boo1q${i}`, seed());
    const sends = live.table.sends.length;
    const before = readFileSync(live.at.path);
    const dry = live.restart({ dryRun: true });
    await dry.dealer.start();
    for (let i = 0; i < 5; i++) await dry.step();
    assert.equal(dry.table.sends.length, sends, "a dry run sends nothing");
    assert.ok(readFileSync(dry.at.path).equals(before), "and writes no secret");
    const would = new Set(actions(dry).filter((a) => a.startsWith("would_")));
    assert.ok(would.has("would_reveal") && would.has("would_commit"));
    if (dry.table.games.get(idle)?.phase === Phase.Player) assert.ok(would.has("would_timeout"));
    assert.equal(dry.logs.filter((l) => l.action === "would_commit").length, 1, "said once, not every tick");
    // A dry run on a table with nothing stored yet makes no store at all.
    const fresh = rig({ dryRun: true });
    await fresh.step();
    assert.equal(existsSync(fresh.at.path), false);
    assert.equal(fresh.store.size, 0);
    assert.equal(fresh.table.sends.length, 0);
    assert.deepEqual(actions(fresh), ["would_commit"]);
    console.log("✓ a dry run logs what it would commit, reveal and time out; nothing is sent or stored");
  }

  // ---- settled secrets are pruned, only once the chain shows the hand gone
  {
    const r = rig({ keepSettled: 4, pruneBatch: 4, lowWater: 4, target: 8 });
    await r.step();
    for (let i = 0; i < 40; i++) {
      r.table.bet("boo1alice", seed());
      await playOut(r);
    }
    await r.step();
    assert.ok(actions(r).includes("pruned"));
    const held = r.store.indexes();
    assert.ok(held.length <= 4 + 4 + 8 + 8, `the store stays small (${held.length} secrets after 40 hands)`);
    assert.ok(held[0] >= r.table.nextGame - 4n - 4n, "the newest settled secrets are kept a while");
    for (let k = r.table.nextGame; k < r.table.nextCommit; k++) assert.ok(r.store.has(k), "an unused commitment's secret is never pruned");
    // An old hand still open holds the line: nothing at or after it is pruned.
    const old = r.table.bet("boo1bob", seed());
    await r.step();
    for (let i = 0; i < 30; i++) {
      const id = r.table.bet("boo1alice", seed());
      for (let j = 0; j < 60 && r.table.games.has(id); j++) {
        await r.step();
        const g = r.table.games.get(id);
        if (g?.phase === Phase.Player) r.table.move(id, false);
        if (r.table.games.get(old)?.phase === Phase.Player && j === 0 && i === 29) r.table.move(old, false);
      }
    }
    assert.ok(r.store.has(old) || !r.table.games.has(old), "an open hand's secret is kept");
    // The file itself shrank: pruned secrets are gone from disk too.
    r.store.close();
    const onDisk = SeedStore.open({ ...r.at, readOnly: true });
    assert.equal(onDisk.size, r.store.size);
    console.log("✓ secrets of settled hands are pruned (a tail kept); open and unused ones never");
  }

  // ---- the node is down: a tick throws (the loop retries), nothing is sent, and it recovers
  {
    const r = rig();
    await r.step();
    const id = r.table.bet("boo1alice", seed());
    r.table.down = true;
    await assert.rejects(r.dealer.tick(), /didn't answer/);
    r.table.down = false;
    await playOut(r);
    assert.ok(r.table.settled.some((s) => s.game === id));
    // A low balance is said (once).
    r.table.balance_ = 1_000_000_000n;
    const low = r.restart();
    await low.dealer.start();
    const warned = low.logs.filter((l) => l.action === "low_balance");
    assert.equal(warned.length, 1);
    assert.deepEqual([warned[0].balance, warned[0].under], ["1", "5"]);
    console.log("✓ a node that doesn't answer is retried; a low balance is warned about");
  }

  // ---- a bet is two transactions: nothing is dealt before the seed, and a seed that never comes ends the hand
  {
    const r = rig();
    await r.step();
    const reveals = () => sent(r, "reveal").length;
    const timeouts = () => sent(r, "timeout").length;
    // The fake's own rules for a seed, as the program's: only the player, only the committed seed, once, in time.
    const s = seed();
    const id = r.table.commitBet("boo1alice", s);
    const g = r.table.games.get(id)!;
    assert.deepEqual([g.phase, g.clientSeed, g.deadline], [Phase.Seed, null, BigInt(r.table.height) + 25n]);
    assert.deepEqual(owedLinks(g, chainFromTop(r.store.top(id)!)), [], "nothing is owed before the seed");
    assert.throws(() => r.table.revealSeed(id, "boo1bob", s), (e) => e === 55);
    assert.throws(() => r.table.revealSeed(id, "boo1alice", seed()), (e) => e === 71);
    // Waiting for the seed, before its deadline: the dealer does nothing, tick after tick, and a restart changes nothing.
    assert.deepEqual(await r.step(), []);
    r.table.mine(5);
    assert.deepEqual(await r.step(), []);
    let again = r.restart();
    await again.dealer.start();
    assert.deepEqual(await again.step(), [], "a restarted dealer does nothing for it either");
    assert.deepEqual([reveals(), timeouts()], [0, 0]);
    assert.ok(!actions(r).concat(actions(again)).some((a) => /hand_without|lost|refused/.test(a)), "and says nothing alarming about it");
    // The seed arrives: the next look deals it, exactly the deal's links.
    r.table.revealSeed(id, "boo1alice", s);
    assert.throws(() => r.table.revealSeed(id, "boo1alice", s), (e) => e === 70);
    assert.deepEqual(await again.step(), [`reveal:${id}:${Phase.Deal}:0`]);
    assert.equal(reveals(), 1);
    assert.equal(sent(r, "reveal")[0].call.input, call.reveal(id, owedLinks({ ...g, clientSeed: s, phase: Phase.Deal }, chainFromTop(again.store.top(id)!))).input);
    // A restart with the seed revealed and the deal owed deals it too (here: it's already dealt, so nothing twice).
    again = again.restart();
    await again.dealer.start();
    await playOut(again);
    assert.equal(verifyHand(again.table.settled.at(-1)!).forfeited, false);

    // A seed that never comes. One block before the deadline: nothing. At it: the timeout, which lands past it.
    const lost = again.table.commitBet("boo1bob", seed());
    const gl = again.table.games.get(lost)!;
    again.table.mine(Number(gl.deadline) - again.table.height - 1);
    assert.deepEqual(await again.step(), []);
    again.table.mine(1);
    const before = timeouts();
    // A dealer restarted at this very point plans the same call, and only one of them lands.
    const twin = again.restart();
    await twin.dealer.start();
    assert.deepEqual(await twin.step(), [`timeout:seed:${lost}`]);
    assert.equal(timeouts(), before + 1);
    const t = sent(r, "timeout").at(-1)!;
    assert.deepEqual([t.call.input, t.validForBlocks], [call.timeout(lost).input, 10]);
    const ended = twin.table.settled.at(-1)!;
    assert.deepEqual([ended.game, ended.outcome, ended.payout, ended.clientSeed, ended.dealt, ended.by], [lost, Outcome.NoSeed, 0n, null, 0, DEALER]);
    const v = verifyHand(ended);
    assert.deepEqual([v.unrevealed, v.forfeited, v.cards], [true, false, []]);
    assert.deepEqual(await twin.step(), [], "once: the hand is gone");
    const line = twin.logs.find((l) => l.action === "timeout")!;
    assert.deepEqual([line.outcome, line.turn, line.game], ["no_seed", "seed", lost.toString()]);
    assert.throws(() => twin.table.revealSeed(lost, "boo1bob", seed()), (e) => e === 54);

    // A seed past its deadline is refused (72) and the dealer's timeout still ends the hand.
    const s3 = seed();
    const late = twin.table.commitBet("boo1carol", s3);
    twin.table.mine(26);
    assert.throws(() => twin.table.revealSeed(late, "boo1carol", s3), (e) => e === 72);
    assert.deepEqual(await twin.step(), [`timeout:seed:${late}`]);
    assert.equal(twin.table.settled.at(-1)!.outcome, Outcome.NoSeed);

    // A stale read: the seed landed in the deadline's own block, but the dealer's node still showed the hand
    // waiting. Its timeout is refused (60: the dealer now has its own deadline), pays nobody, and the hand is dealt.
    const s4 = seed();
    const raced = twin.table.commitBet("boo1dave", s4);
    const gr = twin.table.games.get(raced)!;
    twin.table.mine(Number(gr.deadline) - twin.table.height - 1);
    twin.table.revealSeed(raced, "boo1dave", s4);
    assert.equal(BigInt(twin.table.height), gr.deadline);
    const read = twin.table.game.bind(twin.table);
    twin.table.game = async (id) => (id === raced ? { ...gr } : read(id));
    assert.deepEqual(await twin.step(), [`timeout:seed:${raced}`]);
    twin.table.game = read;
    const refusedLine = twin.logs.filter((l) => l.action === "timeout_refused").at(-1)!;
    assert.deepEqual([refusedLine.abort, refusedLine.moved], [60, true]);
    assert.equal(twin.table.games.get(raced)!.phase, Phase.Deal, "the seed got there first: the hand is the dealer's to deal");
    assert.deepEqual(await twin.step(), [`reveal:${raced}:${Phase.Deal}:0`]);
    await playOut(twin);
    const last = twin.table.settled.at(-1)!;
    assert.deepEqual([last.game, verifyHand(last).forfeited, verifyHand(last).unrevealed], [raced, false, false]);

    // A dry run says what it would do and sends nothing.
    const dryId = twin.table.commitBet("boo1erin", seed());
    twin.table.mine(26);
    const dry = twin.restart({ dryRun: true });
    const n = dry.table.sends.length;
    assert.deepEqual(await dry.step(), [`timeout:seed:${dryId}`]);
    assert.ok(actions(dry).includes("would_timeout"));
    assert.equal(dry.table.sends.length, n);
    const live = dry.restart({ dryRun: false });
    await live.step();
    assert.equal(live.table.open.length, 0);
    console.log("✓ a bet waiting for its seed is left alone, dealt once the seed is revealed, and ended by timeout when it never comes");
  }

  // ---- `inspect`: what `status` shows
  {
    const r = rig();
    await r.step();
    const id = r.table.bet("boo1alice", seed());
    const h = await inspect(r.table, r.store, DEALER);
    assert.deepEqual([h.isDealer, h.unused, h.servable, h.missing, h.mismatched, h.stored, h.nextGame, h.nextCommit], [true, 15, 15, [], [], 16, 1n, 16n]);
    assert.deepEqual(h.open, [{ id, phase: Phase.Deal, deadline: r.table.games.get(id)!.deadline, dealers: true, waitsForSeed: false, hasSecret: true }]);
    const s = seed();
    const waits = r.table.commitBet("boo1bob", s);
    const h2 = await inspect(r.table, r.store, DEALER);
    assert.deepEqual(h2.open[1], { id: waits, phase: Phase.Seed, deadline: r.table.games.get(waits)!.deadline, dealers: false, waitsForSeed: true, hasSecret: true });
    r.table.revealSeed(waits, "boo1bob", s);
    await playOut(r);
  }

  // ---- no secret, and no link, ever reaches the log
  {
    // The log scrubs long hex out of free text, whatever put it there.
    const link = seed();
    const lines: string[] = [];
    const log = jsonLogger((l) => lines.push(l));
    log.error("oops", { error: `the node said: bad input ${link} (and ${link.toUpperCase()})`, nested: { why: [link] }, tx: "ab".repeat(32), game: 7n });
    assert.ok(!lines[0].toLowerCase().includes(link));
    assert.ok(lines[0].includes("ab".repeat(32)), "a transaction's id is public and kept");
    assert.equal(scrub(`x ${link} y`), "x [hex removed] y");
    // A sender whose error quotes the call it was given (links included): scrubbed.
    const r = rig();
    await r.step();
    r.table.bet("boo1alice", seed());
    const orig = r.table.send.bind(r.table);
    r.table.send = async (c, o) => {
      if (c.method === "reveal") throw new Error(`couldn't send reveal with input ${c.input}`);
      return orig(c, o);
    };
    await r.step();
    assert.ok(actions(r).includes("reveal_not_sent"));
    r.table.send = orig;
    await playOut(r, 200);

    // Across every test above: no timeout the dealer sent ever reached a hand in the dealer's phase.
    assert.deepEqual(FakeTable.violations, []);

    // Every line every test logged, against every secret and every link of every chain.
    assert.ok(allTops.size > 1000 && allLogs.length > 1000, `${allTops.size} chains and ${allLogs.length} log lines to check`);
    const text = allLogs.join("\n").toLowerCase();
    const hexRuns = new Set(text.match(/[0-9a-f]{64}/g) ?? []);
    let checked = 0;
    for (const top of allTops) {
      for (const l of chainFromTop(top)) {
        checked++;
        assert.ok(!hexRuns.has(l), "a chain's link (or its secret) is in the log");
      }
    }
    // The only long hex in the log is transaction ids (the fake's are small numbers, zero-padded).
    for (const run of hexRuns) assert.match(run, /^0{48}[0-9a-f]{16}$/);
    assert.ok(!/[0-9a-f]{65,}/.test(text));
    console.log(`✓ ${allLogs.length} log lines hold none of ${checked} links and secrets`);
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}
console.log("✅ the dealer tests passed");
