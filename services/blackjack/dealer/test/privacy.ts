// Card privacy from the dealer's node (audit M-4; docs/proposals/blackjack-card-privacy.md).
//
// The dealer reveals the links a hand's record says it is owed. These tests put a node between
// the dealer and the true table that lies about records, drops what it is given, and keeps every
// byte. A monitor (`LeakMonitor`) holds those bytes against the truth: a link may leave the
// dealer only if the true table already holds it or the true hand is owed it at that moment.
//
//   1. Today's reads (the node's word): each lie gets cards nobody is owed. Asserted, so the
//      finding is a property on record; one lie is followed through to a hand the liar wins.
//   2. Proven reads (`ProvenTable`): the same lies, and subtler ones, get nothing.
//   3. With an honest node, both ways of reading send byte-for-byte the same calls.
//   4. Random play against a node that lies at random: no leak with proven reads.
//   5. Nothing secret reaches a witness or the log.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { Outcome, Phase, Table, bytesAddress, card, chainFromTop, hexBytes, prevLink, replay, storageKey, toHex, verifyHand, type TableCall } from "@blockula/blackjack";
import { ProofError, ProvenTable } from "@blockula/blackjack/proven";
import { encodeGame, storedEntry } from "@blockula/blackjack/testing";
import { Dealer, type DealerOptions, type TableReader } from "../src/dealer.js";
import { jsonLogger } from "../src/log.js";
import { isNodeFault } from "../src/reader.js";
import { FakeTable } from "./fake.js";
import { FakeNode, Ledger, LeakMonitor, MemoryStore, lieAboutMoves, lieAboutSeeds, phantomHand, type Lie } from "./fakenode.js";

const A = (n: number) => bytesAddress(new Uint8Array(32).fill(n), "boo");
const PROGRAM = A(1);
const DEALER = A(2);
const OWNER = A(3);
const PLAYERS = [A(11), A(12), A(13), A(14)];
const MALLORY = A(66);
const FAST = { waitMs: 40, retryMs: 2, attemptMs: 30, headTtlMs: 0 };
const OPTS: DealerOptions = { lowWater: 6, target: 12, batch: 64, dryRun: false, keepSettled: 2, lowBalance: 5_000_000_000n, txValidBlocks: 100, pruneBatch: 4 };
const h = (text: string): string => createHash("sha256").update(text).digest("hex");

/** Every log line of every world, and every store: looked through at the end. */
const allLogs: string[] = [];
const allStores: MemoryStore[] = [];
const allNodes: { node: FakeNode; witness: boolean }[] = [];

type Reads = "direct" | "plain" | "proven";

interface World {
  table: FakeTable;
  ledger: Ledger;
  store: MemoryStore;
  monitor: LeakMonitor;
  /** The node the dealer reads from and sends to. */
  node: FakeNode;
  witnesses: FakeNode[];
  dealer: Dealer;
  logs: Record<string, unknown>[];
  /** Whether the node passes on what it is given. A liar doesn't. */
  relays: boolean;
  /** One look at the table and everything it started; the error if the look failed. */
  step(): Promise<{ started: string[]; error?: unknown }>;
  /** Steps with an honest node, moving for the players (hit under 17), until no hand is open. */
  playOut(max?: number): Promise<void>;
  reveals(): { call: TableCall; game: bigint; links: string[] }[];
}

/** A table, its true chain, a node in front of it and a dealer that reads the given way. */
function world(reads: Reads, name: string, opts: { witnesses?: number; quorum?: number } = {}): World {
  const table = new FakeTable(DEALER, OWNER);
  const ledger = new Ledger(PROGRAM, table);
  const store = new MemoryStore();
  const monitor = new LeakMonitor(table, store);
  const node = new FakeNode(`${name}-node`, ledger);
  const witnesses = Array.from({ length: opts.witnesses ?? 1 }, (_, i) => new FakeNode(`${name}-witness${i}`, ledger));
  allStores.push(store);
  allNodes.push({ node, witness: false }, ...witnesses.map((w) => ({ node: w, witness: true })));
  const reader: TableReader = reads === "direct" ? table : reads === "plain" ? new Table(PROGRAM, node.storageRoute()) : new ProvenTable(PROGRAM, node, witnesses, { ...FAST, quorum: opts.quorum });
  const logs: Record<string, unknown>[] = [];
  const log = jsonLogger((line) => {
    allLogs.push(line);
    logs.push(JSON.parse(line) as Record<string, unknown>);
  });
  let drawn = 0;
  const dealer = new Dealer(reader, table, store, log, { ...OPTS, random: () => h(`top:${drawn++}`) });
  let seen = 0;
  const seenBy = witnesses.map(() => 0);
  const w: World = {
    table,
    ledger,
    store,
    monitor,
    node,
    witnesses,
    dealer,
    logs,
    relays: true,
    async step() {
      let out: { started: string[]; error?: unknown };
      try {
        out = { started: (await dealer.tick()).started };
      } catch (error) {
        out = { started: [], error };
      }
      await dealer.drain();
      // Everything the nodes were asked during this look.
      monitor.requests(node, seen);
      seen = node.asked.length;
      witnesses.forEach((wn, i) => {
        monitor.requests(wn, seenBy[i]);
        seenBy[i] = wn.asked.length;
      });
      return out;
    },
    async playOut(max = 80) {
      node.honest();
      w.relays = true;
      for (let i = 0; i < max && table.open.length; i++) {
        const { error } = await w.step();
        assert.equal(error, undefined, `an honest node's read failed: ${String(error)}`);
        for (const id of [...table.open]) {
          const g = table.games.get(id)!;
          if (g.phase === Phase.Player) table.move(id, g.playerTotal < 17);
        }
      }
      assert.equal(table.open.length, 0, "every hand finished");
    },
    reveals() {
      return table.sends.filter((s) => s.call.method === "reveal").map((s) => {
        const b = hexBytes(s.call.input);
        const view = new DataView(b.buffer, b.byteOffset);
        return { call: s.call, game: view.getBigUint64(0, true), links: Array.from({ length: view.getUint32(8, true) }, (_, i) => toHex(b.subarray(12 + 32 * i, 44 + 32 * i))) };
      });
    },
  };
  // The node is handed every call the dealer sends: the monitor reads it first, against the table as it is then.
  table.fate = (c) => {
    monitor.call(c);
    return w.relays ? "ok" : "lost";
  };
  return w;
}

/** A started dealer with commitments on the table. */
async function ready(reads: Reads, name: string, opts: { witnesses?: number; quorum?: number } = {}): Promise<World> {
  const w = world(reads, name, opts);
  await w.dealer.start();
  for (let i = 0; i < 3; i++) await w.step();
  assert.equal(w.table.nextCommit, 12n, "commitments are on the table");
  return w;
}

/** Steps (honestly) until hand `id` is the player's to move. */
async function toPlayersTurn(w: World, id: bigint): Promise<boolean> {
  for (let i = 0; i < 6 && w.table.games.get(id)?.phase !== Phase.Player; i++) await w.step();
  return w.table.games.get(id)?.phase === Phase.Player;
}

/** A hand of `player`'s that reaches the player's turn (one dealt a natural is over at once: bet again). */
async function handInPlay(w: World, player: string, tag: string): Promise<{ id: bigint; seed: string }> {
  for (let i = 0; i < 40; i++) {
    const seed = h(`${tag}:${i}`);
    const id = w.table.bet(player, seed);
    if (await toPlayersTurn(w, id)) return { id, seed };
    await w.playOut();
  }
  throw new Error("no hand reached the player's turn");
}

/** The node turns liar: it answers with `lie` and passes nothing on. */
function turn(w: World, lie: Lie): void {
  w.node.honest();
  w.node.lie = lie;
  w.relays = false;
}

const leakedOf = (w: World, game: bigint): number[] => [...new Set(w.monitor.leaks.filter((l) => l.game === game).map((l) => l.index))].sort((a, b) => a - b);

// ====================================================================== 1. today's reads leak
{
  // ---- "the player stood": the hole card and the dealer's draws, while the player is still choosing
  {
    const w = await ready("plain", "stood");
    const { id, seed } = await handInPlay(w, PLAYERS[0], "stood");
    const g = w.table.games.get(id)!;
    assert.deepEqual([g.phase, g.dealt, w.monitor.leaks.length], [Phase.Player, 3, 0], "an honest node so far: nothing but what was owed");
    turn(w, lieAboutMoves(Phase.Dealer));
    await w.step();
    const got = leakedOf(w, id);
    assert.ok(got.length >= 1 && got[0] === 4, `the node was handed link 4 on (${got}) of a hand whose player hasn't moved`);
    // What the liar does with it: link 4 is the dealer's hole card if the player stands, and the player's next card if they hit.
    const link4 = w.monitor.leaks.find((l) => l.game === id && l.index === 4)!.link;
    assert.equal(prevLink(link4), g.link, "it is the chain's next link: anyone can check that against the hand's record");
    const next = card(link4, seed, id, 4);
    // The truth hasn't moved: the player now chooses, knowing.
    assert.equal(w.table.games.get(id)!.phase, Phase.Player);
    w.table.move(id, true);
    await w.playOut();
    const settled = w.table.settled.find((s) => s.game === id)!;
    const cards = verifyHand(settled).cards;
    assert.equal(cards[3], next, "the card the liar computed is the card the player was then dealt");
    console.log(`✓ today: "the player stood" gets the node links ${got.join(", ")} while the player is still choosing, and with them the next card`);
  }

  // ---- "the player hit": the next card, one at a time
  {
    const w = await ready("plain", "hit");
    const { id } = await handInPlay(w, PLAYERS[1], "hit");
    turn(w, lieAboutMoves(Phase.Hit));
    await w.step();
    assert.deepEqual(leakedOf(w, id), [4], "the next card's link, for a hit nobody made");
    await w.playOut();
    console.log('✓ today: "the player hit" gets the node the next card\'s link');
  }

  // ---- "the seed is revealed": the deal, before the player has to commit to playing it
  {
    const w = await ready("plain", "seed");
    const seed = h("seed-lie");
    const id = w.table.commitBet(PLAYERS[2], seed);
    await w.step();
    assert.deepEqual([w.reveals().length, w.monitor.leaks.length], [0, 0], "a hand waiting for its seed is left alone");
    // The player, in on it, tells the node the seed without putting it on the chain.
    turn(w, lieAboutSeeds(seed));
    await w.step();
    const got = leakedOf(w, id);
    assert.ok(got.length >= 3 && got[0] === 1, `the deal's links (${got}) left for a hand whose seed isn't on the chain`);
    assert.equal(w.table.games.get(id)!.phase, Phase.Seed);
    w.table.revealSeed(id, PLAYERS[2], seed);
    await w.playOut();
    console.log(`✓ today: "the seed is revealed" gets the node links ${got.join(", ")} of a hand still waiting for its seed`);
  }

  // ---- a hand nobody bet: the liar learns an unused commitment's links, then picks the seed that wins
  {
    const w = await ready("plain", "phantom");
    const k = w.table.nextGame;
    const commit = w.table.commitments.get(k)!;
    turn(w, phantomHand(MALLORY, h("any seed at all")));
    await w.step();
    const got = leakedOf(w, k);
    assert.ok(got.length >= 3 && got[0] === 1, `links ${got} of game ${k}, which nobody has bet on`);
    assert.equal(w.table.games.size, 0);
    // The links don't depend on the seed; the cards do. So the liar looks for a seed that makes them a natural.
    const links = [1, 2, 3].map((i) => w.monitor.leaks.find((l) => l.game === k && l.index === i)!.link);
    let winning: string | undefined;
    for (let i = 0; i < 5000 && winning === undefined; i++) {
      const s = h(`grind:${i}`);
      if (replay(commit, s, k, links).outcome === Outcome.Blackjack) winning = s;
    }
    assert.ok(winning, "a seed that makes the leaked links a natural is found in a few thousand tries");
    // And bets with it, on the true table, through an honest node. The dealer deals the hand it was always going to.
    w.node.honest();
    w.relays = true;
    assert.equal(w.table.bet(MALLORY, winning), k);
    await w.playOut();
    const settled = w.table.settled.find((s) => s.game === k)!;
    assert.equal(settled.outcome, Outcome.Blackjack);
    assert.equal(settled.payout, settled.stake + (settled.stake * 3n) / 2n, "paid 3:2, on a hand whose cards the player chose");
    assert.equal(verifyHand(settled).outcome, Outcome.Blackjack, "and the hand verifies: the chain can't tell");
    console.log(`✓ today: a hand nobody bet gets the node links ${got.join(", ")} of an unused commitment; a seed chosen to fit them wins 3:2, and verifies`);
  }
}

// ====================================================================== 2. proven reads don't
{
  /** A look at the table while the node lies: it fails as a node fault, nothing is sent, nothing leaks. */
  async function refusedLook(w: World, what: string, kinds: readonly string[] = ["disagreement"]): Promise<void> {
    const before = w.table.sends.length;
    for (let i = 0; i < 3; i++) {
      const { started, error } = await w.step();
      assert.ok(error instanceof ProofError, `${what}: the look wasn't refused (${String(error)})`);
      assert.ok(kinds.includes(error.kind), `${what}: refused as ${error.kind}: ${error.message}`);
      assert.deepEqual(started, []);
    }
    assert.equal(w.table.sends.length, before, `${what}: something was sent`);
    assert.deepEqual(w.monitor.leaks, [], `${what}: a link nobody is owed left the dealer`);
  }

  // ---- the four lies of section 1
  {
    const w = await ready("proven", "p-stood");
    const { id } = await handInPlay(w, PLAYERS[0], "stood");
    turn(w, lieAboutMoves(Phase.Dealer));
    await refusedLook(w, '"the player stood"');
    turn(w, lieAboutMoves(Phase.Hit));
    await refusedLook(w, '"the player hit"');
    await w.playOut();
    assert.ok(!verifyHand(w.table.settled.find((s) => s.game === id)!).forfeited, "and the hand is dealt once the node is honest again");

    const seed = h("p-seed");
    const waiting = w.table.commitBet(PLAYERS[2], seed);
    await w.step();
    turn(w, lieAboutSeeds(seed));
    await refusedLook(w, '"the seed is revealed"');
    turn(w, phantomHand(MALLORY, h("any")));
    await refusedLook(w, "a hand nobody bet");
    w.table.revealSeed(waiting, PLAYERS[2], seed);
    await w.playOut();
    assert.deepEqual(w.monitor.leaks, []);
    assert.ok(w.monitor.allowed > 6, "(the monitor saw the owed links go by, and let them)");
    console.log("✓ proven: the same four lies are refused as the node's fault; nothing is sent, nothing leaks, and play goes on afterwards");
  }

  // ---- subtler liars: honest about everything but one record; a changed value; a moved height; an absent record
  {
    const w = await ready("proven", "p-subtle");
    const { id } = await handInPlay(w, PLAYERS[1], "subtle");
    const gameKey = storageKey.game(id);
    // Honest about the house, the limits and the list (so their root is confirmed); a lie for the hand alone.
    turn(w, lieAboutMoves(Phase.Dealer));
    w.node.lieFor = (key) => key === gameKey;
    await refusedLook(w, "a lie for one record only");
    // The true proof, with the record's phase changed on the way out.
    w.node.honest();
    w.relays = false;
    const stood = toHex(storedEntry(encodeGame({ ...w.table.games.get(id)!, phase: Phase.Dealer })));
    w.node.bend = (r) => (r.key.endsWith(gameKey) ? { ...r, value: stood } : r);
    await refusedLook(w, "a changed value under a real proof", ["bad_proof"]);
    // The lie's own consistent state, said to be the next block's (which no witness has yet).
    turn(w, lieAboutMoves(Phase.Dealer));
    w.node.bend = (r) => ({ ...r, height: r.height + 1 });
    await refusedLook(w, "a lie dated one block ahead", ["unconfirmed"]);
    await w.playOut();

    // "That hand doesn't exist" can't be used to hide one either: absence is proven or refused.
    const { id: hidden } = await handInPlay(w, PLAYERS[1], "hidden");
    w.node.honest();
    w.relays = false;
    w.node.bend = (r) => (r.key.endsWith(storageKey.game(hidden)) ? { ...r, value: null } : r);
    await refusedLook(w, "a hand said to be gone", ["bad_proof"]);
    await w.playOut();
    assert.deepEqual(w.monitor.leaks, []);
    console.log("✓ proven: a lie for one record, a changed value, a lie dated ahead and a record said to be absent get nothing either");
  }

  // ---- old state, genuine and proven: it can only owe what is already out
  {
    const w = await ready("proven", "p-old");
    const { id } = await handInPlay(w, PLAYERS[0], "old");
    const then = w.table.height;
    // The hand moves on honestly: the player hits and is dealt.
    w.table.move(id, true);
    await w.step();
    await w.step();
    const now = w.table.games.get(id);
    // The node goes back to when the dealer owed nothing, then to when it owed the hit, and relays.
    for (const height of w.ledger.heights().filter((x) => x >= then - 3)) {
      w.node.honest();
      w.node.pinned = height;
      const { error } = await w.step();
      assert.ok(error === undefined || (error instanceof ProofError && error.kind === "stale"), String(error));
    }
    assert.deepEqual(w.monitor.leaks, [], "a genuine old record owes no link the present doesn't");
    assert.deepEqual(w.table.games.get(id), now, "and a reveal built on it changes nothing");
    // Far enough back, it is refused as stale rather than acted on.
    w.table.mine(40);
    w.node.pinned = w.ledger.heights().filter((x) => x <= then).pop()!;
    const { error } = await w.step();
    assert.ok(error instanceof ProofError && error.kind === "stale" && !isNodeFault(error));
    await w.playOut();
    console.log("✓ proven: a genuine old state gets the node nothing new, and past maxLagBlocks is refused as stale");
  }

  // ---- the witnesses: a quorum, one down, and the limit of the whole idea
  {
    // Two of three must confirm; one is down. Play goes on, and a lie is still refused.
    const w = await ready("proven", "p-quorum", { witnesses: 3, quorum: 2 });
    w.witnesses[1].down = true;
    const { id } = await handInPlay(w, PLAYERS[3], "quorum");
    turn(w, lieAboutMoves(Phase.Dealer));
    await refusedLook(w, "a lie with one witness down");
    await w.playOut();
    assert.ok(!verifyHand(w.table.settled.find((s) => s.game === id)!).forfeited);
    // Two down: nothing is confirmed, so nothing is dealt. It fails closed, and it isn't called the node's fault.
    w.witnesses[0].down = true;
    w.table.bet(PLAYERS[3], h("closed"));
    const before = w.table.sends.length;
    const { error } = await w.step();
    assert.ok(error instanceof ProofError && error.kind === "unconfirmed" && !isNodeFault(error));
    assert.equal(w.table.sends.length, before);
    w.witnesses[0].down = w.witnesses[1].down = false;
    await w.playOut();
    assert.deepEqual(w.monitor.leaks, []);

    // What proven reads don't claim: a witness that lies along with the node. Then it is one liar again.
    const c = await ready("proven", "p-collusion");
    const hand = await handInPlay(c, PLAYERS[0], "collusion");
    turn(c, lieAboutMoves(Phase.Dealer));
    c.witnesses[0].lie = lieAboutMoves(Phase.Dealer);
    await c.step();
    assert.deepEqual(leakedOf(c, hand.id).slice(0, 1), [4], "(with every witness in on the lie, the links leave as they do today)");
    c.witnesses[0].honest();
    // The block they lied about stays refused (the node now gives another root for it); the next one is read as usual.
    c.node.honest();
    assert.ok(isNodeFault((await c.step()).error));
    c.table.mine();
    await c.playOut();
    console.log("✓ proven: one witness down inside the quorum changes nothing; too few and nothing is dealt; witnesses that all lie with the node are the limit");
  }
}

// ====================================================================== 3. the same calls
{
  /** The same players, seeds and moves, whatever the dealer reads through. */
  async function script(reads: Reads): Promise<World> {
    const w = await ready(reads, `same-${reads}`);
    for (let i = 0; i < 48; i++) {
      const player = PLAYERS[i % PLAYERS.length];
      const seed = h(`same:${i}`);
      if (i % 8 === 5) {
        // A bet whose seed never comes: left alone, then ended by the dealer's timeout.
        const id = w.table.commitBet(player, seed);
        await w.step();
        w.table.mine(Number(w.table.limits_.seedBlocks) + 1);
        for (let n = 0; n < 4 && w.table.games.has(id); n++) await w.step();
        assert.ok(!w.table.games.has(id));
      } else if (i % 8 === 6) {
        // A player who walks away at their turn: stood by the dealer's timeout, then dealt.
        const id = w.table.bet(player, seed);
        if (await toPlayersTurn(w, id)) {
          w.table.mine(Number(w.table.limits_.idleBlocks) + 1);
          for (let n = 0; n < 6 && w.table.games.has(id); n++) await w.step();
        }
        await w.playOut();
      } else if (i % 8 === 7) {
        // Two hands open at once.
        w.table.bet(player, seed);
        w.table.bet(PLAYERS[(i + 1) % PLAYERS.length], h(`same:${i}:b`));
        await w.playOut();
      } else {
        w.table.bet(player, seed);
        await w.playOut();
      }
    }
    assert.equal(w.table.open.length, 0);
    return w;
  }
  const [direct, plain, proven] = [await script("direct"), await script("plain"), await script("proven")];
  const calls = (w: World) => w.table.sends.map((s) => ({ method: s.call.method, input: s.call.input, value: s.call.value, storageLimit: s.call.storageLimit, validForBlocks: s.validForBlocks, fuelLimit: s.fuelLimit, id: s.id }));
  const lines = (w: World) => w.logs.map(({ time: _time, ...rest }) => rest);
  assert.ok(calls(proven).length > 100 && proven.table.settled.length === 48 + 6, `${calls(proven).length} calls, ${proven.table.settled.length} hands`);
  assert.deepEqual(calls(proven), calls(plain), "proven reads send byte-for-byte what today's reads send");
  assert.deepEqual(calls(plain), calls(direct));
  assert.deepEqual(proven.table.settled, plain.table.settled, "and the same hands settle the same way");
  assert.deepEqual(plain.table.settled, direct.table.settled);
  assert.deepEqual(lines(proven), lines(plain), "and the dealer logs the same lines");
  assert.equal(proven.table.height, plain.table.height);
  const outcomes = new Set<number>();
  for (const s of proven.table.settled) {
    const v = verifyHand(s);
    assert.ok(!v.forfeited, "no hand was forfeited");
    outcomes.add(s.outcome);
  }
  assert.ok(outcomes.has(Outcome.NoSeed) && outcomes.size >= 4, `outcomes ${[...outcomes]}`);
  for (const w of [plain, proven]) assert.deepEqual(w.monitor.leaks, [], "an honest node is handed only what is owed, either way");
  // What it cost: one proof a record, and one header a witness for each block looked at.
  const reader = proven.dealer["reader" as keyof Dealer] as unknown as ProvenTable;
  assert.equal(proven.node.asked.filter((a) => a.startsWith("proof/")).length, reader.proofs);
  assert.ok(proven.witnesses[0].asked.filter((a) => a.startsWith("block/")).length <= proven.table.height, "at most one header a block");
  console.log(`✓ with an honest node, proven reads and today's reads send the same ${calls(proven).length} calls, settle the same ${proven.table.settled.length} hands and log the same ${lines(proven).length} lines`);
}

// ====================================================================== 4. random play, a node lying at random
{
  /** A small deterministic generator (mulberry32). */
  function rng(seed: number): () => number {
    let a = seed >>> 0;
    return () => {
      a = (a + 0x6d2b79f5) >>> 0;
      let t = a;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  async function round(reads: Reads, n: number): Promise<World> {
    const w = await ready(reads, `rand-${reads}-${n}`);
    const rand = rng(n * 7919 + 1);
    const pick = <T>(xs: readonly T[]): T => xs[Math.floor(rand() * xs.length)];
    const seeds = new Map<bigint, { player: string; seed: string }>();
    for (let tick = 0; tick < 70; tick++) {
      // ---- the players
      if (rand() < 0.3 && w.table.open.length < 5 && w.table.commitments.has(w.table.nextGame)) {
        const player = pick(PLAYERS);
        const seed = h(`rand:${n}:${tick}`);
        seeds.set(w.table.commitBet(player, seed), { player, seed });
      }
      for (const id of [...w.table.open]) {
        const g = w.table.games.get(id)!;
        try {
          if (g.phase === Phase.Seed && rand() < 0.5) w.table.revealSeed(id, g.player, seeds.get(id)!.seed);
          else if (g.phase === Phase.Player && rand() < 0.6) w.table.move(id, rand() < 0.45);
        } catch (e) {
          if (typeof e !== "number") throw e; // An abort (a seed past its deadline) is the table's answer.
        }
      }
      if (rand() < 0.1) w.table.mine(1 + Math.floor(rand() * 3));

      // ---- the node
      w.node.honest();
      w.relays = true;
      const mood = rand();
      if (mood >= 0.45) {
        w.relays = false;
        // "The seed is revealed", of each waiting hand with its own seed (the players are in on it).
        const waitingSeeds: Lie = (storage, truth) => {
          for (const g of [...truth.games.values()].filter((x) => x.phase === Phase.Seed)) {
            const one = new FakeTable(DEALER, OWNER);
            one.games.set(g.id, g);
            lieAboutSeeds(seeds.get(g.id)!.seed)(storage, one);
          }
        };
        const lie = pick<Lie>([lieAboutMoves(Phase.Dealer), lieAboutMoves(Phase.Hit), waitingSeeds, phantomHand(MALLORY, h(`phantom:${n}:${tick}`)), (s, t) => (lieAboutMoves(Phase.Dealer)(s, t), waitingSeeds(s, t))]);
        const how = rand();
        if (how < 0.5) w.node.lie = lie;
        else if (how < 0.65) {
          // A lie for the hands' records only.
          w.node.lie = lie;
          w.node.lieFor = (key) => key.startsWith("67");
        } else if (how < 0.8) {
          // A lie dated a block ahead.
          w.node.lie = lie;
          w.node.bend = (r) => ({ ...r, height: r.height + 1 });
        } else {
          // The genuine state of an old block, and a node that may or may not pass things on.
          const heights = w.ledger.heights();
          w.node.pinned = pick(heights.slice(-12));
          w.relays = rand() < 0.5;
        }
      }
      await w.step();
    }
    // The node is honest from here: every hand is finished, seeds revealed or timed out.
    w.node.honest();
    w.relays = true;
    for (let i = 0; i < 200 && w.table.open.length; i++) {
      for (const id of [...w.table.open]) {
        const g = w.table.games.get(id)!;
        try {
          if (g.phase === Phase.Seed) w.table.revealSeed(id, g.player, seeds.get(id)!.seed);
          else if (g.phase === Phase.Player) w.table.move(id, g.playerTotal < 17);
        } catch (e) {
          if (typeof e !== "number") throw e;
          w.table.mine(4); // Past the seed's deadline: the dealer's timeout ends it.
        }
      }
      const { error } = await w.step();
      assert.equal(error, undefined, String(error));
    }
    assert.equal(w.table.open.length, 0, "every hand finished once the node was honest");
    for (const s of w.table.settled) verifyHand(s);
    return w;
  }

  const ROUNDS = Number(process.env.PRIVACY_ROUNDS ?? 16);
  let [hands, allowed, scanned, plainLeaks] = [0, 0, 0, 0];
  for (let n = 0; n < ROUNDS; n++) {
    const w = await round("proven", n);
    assert.deepEqual(w.monitor.leaks, [], `round ${n}: a link nobody was owed left the dealer`);
    hands += w.table.settled.length;
    allowed += w.monitor.allowed;
    scanned += w.monitor.scanned;
    // The same round on today's reads: the monitor isn't blind.
    plainLeaks += (await round("plain", n)).monitor.leaks.length;
  }
  assert.ok(hands > 8 * ROUNDS && allowed > 40 * ROUNDS, `${hands} hands, ${allowed} owed links seen`);
  assert.ok(plainLeaks > 5 * ROUNDS, `today's reads leaked ${plainLeaks} links over the same rounds`);
  console.log(`✓ ${ROUNDS} rounds of random play against a node lying at random: with proven reads, ${scanned} bytes handed to the node held ${allowed} owed links and no other (${hands} hands); today's reads leaked ${plainLeaks}`);
}

// ====================================================================== 5. witnesses and the log
{
  const links = new Set<string>();
  for (const store of allStores) for (const top of store.ever.values()) for (const l of chainFromTop(top)) links.add(l);
  // A witness is asked for its head and for block headers. Nothing else, ever.
  let asked = 0;
  for (const { node, witness } of allNodes) {
    if (!witness) continue;
    for (const a of node.asked) {
      asked++;
      assert.match(a, /^(status|block\/\d+)$/, "a witness is asked for its head and for headers only");
    }
  }
  assert.ok(asked > 2000, `${asked} requests to witnesses`);
  // A node's read requests (either way of reading) hold storage keys, never a link or a secret.
  for (const { node } of allNodes) for (const a of node.asked) for (const run of a.match(/[0-9a-f]{64}/g) ?? []) assert.ok(!links.has(run), "a request held a link");
  // The log: no link, no secret, in any line of any world, the refused looks' errors included.
  const text = allLogs.join("\n").toLowerCase();
  const runs = new Set(text.match(/[0-9a-f]{64}/g) ?? []);
  for (const l of links) assert.ok(!runs.has(l), "a chain's link (or its secret) is in the log");
  for (const run of runs) assert.match(run, /^0{48}[0-9a-f]{16}$/, "the only long hex in the log is transaction ids");
  assert.ok(allLogs.length > 1000 && links.size > 1000, `${allLogs.length} log lines, ${links.size} links`);
  console.log(`✓ ${asked} requests to witnesses were for heads and headers only; ${allLogs.length} log lines hold none of ${links.size} links and secrets`);
}

console.log("✅ the privacy tests passed");
