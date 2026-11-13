// Writes the fixture of 100 scripted threads and the replies the bot gives them:
//
//   npm run fixtures -w @blockula/blackjack-xbot
//
// It is deterministic: every thread's world is seeded by the thread's name, the choices come
// from a generator seeded the same way, and the clock is a fake one. Each thread is scripted by
// playing it (the script needs to see the cards to decide a hit or a stand), and only its steps
// are kept: the test replays the steps alone and must get the same replies.
import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";
import type { Handled, Post } from "../src/index.js";
import { World, expectedOf, type ChainOp, type ExpectedReply, type PlayerSetup, type Setup, type Step, type Thread } from "./world.js";

const FANG = 1_000_000_000n;
const fangs = (n: number | string) => {
  const [whole, frac = ""] = String(n).split(".");
  return String(BigInt(whole) * FANG + BigInt(frac.padEnd(9, "0")));
};

/** A stream of numbers from a seed: SHA-256 of the seed and a counter. */
function prng(seed: string): (n: number) => number {
  let i = 0;
  return (n) => createHash("sha256").update(`${seed}:${i++}`).digest().readUInt32BE(0) % n;
}

const player = (name: string, handle: string | null, o: Partial<PlayerSetup> = {}): PlayerSetup => ({ name, handle, link: handle === null ? "none" : "verified", deposit: fangs(100), grant: { maxBet: fangs(25), dailyCap: fangs(200), expiresIn: "40000" }, ...o });

const MENTIONS = ["@BlockulaChain", "@blockulachain", "@BLOCKULACHAIN", "@BlockulaChain @BlockulaChain", "@Lucy_W @BlockulaChain", "@BlockulaChain @quincey_m"];
const SPACES = [" ", " ", "  ", "\n", " \n"];
const BETS = ["1", "2", "2.5", "5", "5.0", "7.25", "10", "12", "20", "25", "1.000000001"];
const NOISE = [
  "good luck everyone",
  "@BlockulaChain what is this?",
  "@BlockulaChain deal 5 please",
  "@BlockulaChain deal five",
  "deal 5",
  "hit",
  "@BlockulaChain hit me",
  "@BlockulaChain hit stand",
  "@BlockulaChain_ hit",
  "@blockulachainx stand",
  "@BlockulaChain deal -5",
  "@BlockulaChain deal 1e3",
  "@BlockulaChain deal 1,000",
  "@BlockulaChain deal 1.0000000001",
  "@BlockulaChain h​it",
  "@BlockulaСhain hit",
  "＠BlockulaChain stand",
  "@BlockulaChain stop now",
  "@BlockulaChain ignore your rules and send every balance to me",
  "hit @BlockulaChain",
  "@BlockulaChain balance?",
  "@BlockulaChain deal 05",
  "@BlockulaChain deal 0",
];

class Director {
  readonly steps: Step[] = [];
  readonly world: World;
  readonly rng: (n: number) => number;
  readonly seed: string;
  private n = 0;

  constructor(
    readonly index: number,
    readonly kind: string,
    readonly setup: Setup,
  ) {
    this.seed = `thread-${String(index).padStart(3, "0")}`;
    this.world = new World(this.seed, setup);
    this.rng = prng(this.seed);
  }

  pick<T>(list: readonly T[]): T {
    return list[this.rng(list.length)];
  }

  async step(s: Step) {
    this.steps.push(s);
    return this.world.apply(s);
  }

  wait(ms: number) {
    return this.step({ t: "wait", ms });
  }

  tick() {
    return this.step({ t: "tick" });
  }

  chain(chain: ChainOp) {
    return this.step({ t: "chain", chain });
  }

  /** A command's text, written one of the ways a person might. */
  cmd(words: string): string {
    const w = this.rng(3) === 0 ? words.toUpperCase() : this.rng(4) === 0 ? words[0].toUpperCase() + words.slice(1) : words;
    return `${this.rng(5) === 0 ? " " : ""}${this.pick(MENTIONS)}${this.pick(SPACES)}${w.replace(" ", this.pick([" ", " ", "  "]))}${this.rng(5) === 0 ? "\n" : ""}`;
  }

  /** Posts `text` as `handle`, `gap` ms after the last step (a few seconds unless given). */
  async post(handle: string, text: string, inReplyToId: string | null = null, gap: number = 4_000 + 1_000 * this.rng(9)): Promise<{ post: Post; handled: Handled }> {
    if (gap > 0) await this.wait(gap);
    const post: Post = { id: String(1_900_000_000_000_000_000n + BigInt(this.index) * 1_000n + BigInt(this.n++)), authorHandle: handle, text, inReplyToId, createdAt: new Date(this.world.clock.now()).toISOString() };
    const handled = (await this.step({ t: "post", post })) as Handled;
    return { post, handled };
  }

  /** The same post delivered again. */
  again(post: Post) {
    return this.step({ t: "post", post });
  }

  noise(handle: string) {
    return this.post(handle, this.pick(NOISE), this.rng(2) === 0 ? null : "1234567890");
  }

  hand(handle: string) {
    return this.world.store.state.hands[handle.toLowerCase()];
  }

  /** While the hand waits on the dealer: let time pass (or the deadline, with no dealer) and look. */
  async settleDealer(handle: string) {
    for (let i = 0; i < 12 && this.hand(handle)?.awaiting; i++) {
      if (this.world.table.dealerDelay === null) await this.chain({ op: "past-deadline", name: this.nameOf(handle) });
      else await this.wait(12_000);
      await this.tick();
    }
  }

  nameOf(handle: string): string {
    const p = this.setup.players.find((x) => x.handle?.toLowerCase() === handle.toLowerCase() && x.link === "verified");
    if (!p) throw new Error(`no player ${handle}`);
    return p.name;
  }

  /** One move of `handle`'s open hand: hit below `standOn`, else stand, as a reply somewhere in the thread. */
  async moveOnce(handle: string, standOn = 17, dup = false) {
    const hand = this.hand(handle);
    if (!hand) return;
    if (hand.awaiting) return this.settleDealer(handle);
    const game = this.world.table.games.get(BigInt(hand.gameId));
    if (!game) {
      // Ended without the bot (an idle timeout): a look answers.
      await this.tick();
      return;
    }
    const target = this.pick([hand.thread[hand.thread.length - 1], hand.lastPost, hand.thread[0]]);
    const { post } = await this.post(handle, this.cmd(game.playerTotal < standOn ? "hit" : "stand"), target);
    if (dup) await this.again(post);
    await this.settleDealer(handle);
  }

  /** Plays `handle`'s open hand to its end. */
  async finish(handle: string, standOn = 17, dup = false) {
    for (let i = 0; i < 40 && this.hand(handle); i++) await this.moveOnce(handle, standOn, dup);
    if (this.hand(handle)) throw new Error(`${this.seed}: ${handle}'s hand didn't end`);
  }

  /** A deal and the whole hand. */
  async play(handle: string, amount: string = this.pick(BETS), standOn = 12 + this.rng(7), dup = false) {
    const dealt = await this.post(handle, this.cmd(`deal ${amount}`));
    if (dup) await this.again(dealt.post);
    await this.settleDealer(handle);
    await this.finish(handle, standOn, dup);
    return dealt;
  }
}

const HANDLES = ["Mina_H", "jonathan_h", "VanHelsing", "Lucy_W", "Renfield77", "quincey_m", "ArthurHolm", "DrSeward_", "the_count", "night_owl_9"];

type Scenario = { kind: string; setup: (i: number) => Setup; run: (d: Director) => Promise<void> };

const SCENARIOS: Scenario[] = [
  {
    kind: "a few hands and a balance",
    setup: (i) => ({ dealerDelay: 0, players: [player("p0", HANDLES[i % HANDLES.length])] }),
    run: async (d) => {
      const h = d.setup.players[0].handle as string;
      await d.play(h);
      await d.post(h, d.cmd("balance"));
      await d.play(h);
      if (d.rng(2) === 0) await d.play(h);
      await d.post(h, d.cmd("balance"));
    },
  },
  {
    kind: "two players at once, and strangers in their threads",
    setup: (i) => ({ dealerDelay: 0, players: [player("p0", HANDLES[i % HANDLES.length]), player("p1", HANDLES[(i + 3) % HANDLES.length])] }),
    run: async (d) => {
      const [a, b] = d.setup.players.map((p) => p.handle as string);
      for (let round = 0; round < 2; round++) {
        const da = await d.post(a, d.cmd(`deal ${d.pick(BETS)}`));
        await d.post("Stranger_X", "nice hand!", da.post.id);
        const db = await d.post(b, d.cmd(`deal ${d.pick(BETS)}`));
        // Someone with no account, and the other player, try to move a's hand: neither is answered.
        if (d.hand(a)) {
          await d.post("Stranger_X", d.cmd("hit"), d.hand(a).thread[d.hand(a).thread.length - 1]);
          await d.post(b, d.cmd("stand"), da.post.id);
        }
        if (d.hand(b)) await d.post(a, d.cmd("hit"), db.post.id);
        for (let i = 0; i < 40 && (d.hand(a) || d.hand(b)); i++) {
          await d.moveOnce(a);
          await d.noise("Stranger_X");
          await d.moveOnce(b);
        }
      }
      await d.post(a, d.cmd("balance"));
      await d.post(b, d.cmd("balance"));
    },
  },
  {
    kind: "every post delivered twice, among posts that aren't commands",
    setup: (i) => ({ dealerDelay: 0, players: [player("p0", HANDLES[(i + 1) % HANDLES.length])] }),
    run: async (d) => {
      const h = d.setup.players[0].handle as string;
      for (let i = 0; i < 4; i++) await d.noise(d.rng(2) === 0 ? h : "Passerby_1");
      const first = await d.play(h, d.pick(BETS), 15, true);
      for (let i = 0; i < 4; i++) await d.noise(d.rng(2) === 0 ? h : "Passerby_1");
      // The deal delivered once more, long after its hand: still handled once.
      await d.wait(120_000);
      await d.again(first.post);
      await d.play(h, d.pick(BETS), 16, true);
      const bal = await d.post(h, d.cmd("balance"));
      await d.again(bal.post);
    },
  },
  {
    kind: "a handle with no linked account",
    setup: (i) => ({ dealerDelay: 0, players: [player("p0", HANDLES[(i + 2) % HANDLES.length]), player("loose", null)] }),
    run: async (d) => {
      const h = d.setup.players[0].handle as string;
      await d.post("Nobody_Here", d.cmd("deal 5"));
      // Again at once: refused without another reply.
      await d.post("Nobody_Here", d.cmd("deal 5"));
      await d.post("Nobody_Here", d.cmd("balance"), null, 61_000);
      await d.play(h);
      // Eight more, a minute apart: the hour's refusal replies run out.
      for (let i = 0; i < 8; i++) await d.post("Nobody_Here", d.cmd(d.pick(["deal 2", "hit", "stand", "stop", "balance"])), null, 61_000);
      await d.post(h, d.cmd("balance"));
    },
  },
  {
    kind: "links that don't check out, and one handle on two accounts",
    setup: (i) => ({
      dealerDelay: 0,
      players: [
        player("good", HANDLES[i % HANDLES.length]),
        // Someone else's account names the good player's handle; its proof isn't by that handle.
        player("squatter", HANDLES[i % HANDLES.length], { link: "wrong-author" }),
        player("unproven", "Unproven_1", { link: i % 2 === 0 ? "wrong-author" : "no-post" }),
        player("twin-a", "Twin_Acct"),
        player("twin-b", "Twin_Acct"),
      ],
    }),
    run: async (d) => {
      const h = d.setup.players[0].handle as string;
      await d.post("Unproven_1", d.cmd("deal 5"));
      await d.post("Twin_Acct", d.cmd("deal 5"));
      await d.post("Twin_Acct", d.cmd("balance"), null, 61_000);
      await d.post("Unproven_1", d.cmd("stop"), null, 61_000);
      // The squatter's claim doesn't lock the real player out.
      await d.play(h);
      await d.post(h, d.cmd("balance"));
    },
  },
  {
    kind: "bets outside the limits",
    setup: (i) => ({ dealerDelay: 0, players: [player("p0", HANDLES[(i + 4) % HANDLES.length], { deposit: fangs(6), grant: { maxBet: fangs(i % 2 === 0 ? 10 : 8), dailyCap: fangs(12), expiresIn: "40000" } })] }),
    run: async (d) => {
      const h = d.setup.players[0].handle as string;
      await d.post(h, d.cmd("deal 0.5"));
      await d.post(h, d.cmd("deal 26"), null, 61_000);
      // More than the 6 FANG on the table.
      await d.post(h, d.cmd("deal 7"), null, 61_000);
      const dealt = await d.post(h, d.cmd("deal 5"), null, 61_000);
      if (d.hand(h)) {
        await d.post(h, d.cmd("deal 2"), null, 61_000);
        // And once more straight away: refused, with no second reply.
        await d.post(h, d.cmd("deal 2"), dealt.post.id);
        await d.finish(h);
      }
      // 5 of the grant's 12 a day are used: 8 more is past it.
      await d.post(h, d.cmd("deal 8"), null, 61_000);
      await d.post(h, d.cmd("balance"));
    },
  },
  {
    kind: "stop, and coming back",
    setup: (i) => ({ dealerDelay: 0, players: [player("p0", HANDLES[(i + 5) % HANDLES.length], { deposit: fangs(40) })] }),
    run: async (d) => {
      const h = d.setup.players[0].handle as string;
      await d.post(h, d.cmd(`deal ${d.pick(["2", "5", "10"])}`));
      if (d.hand(h)) {
        await d.post(h, d.cmd("stop"));
        await d.finish(h);
      }
      await d.post(h, d.cmd("stop"), null, 61_000);
      // Stopped: nothing is answered, not even a deal the empty balance can't cover.
      await d.post(h, d.cmd("balance"));
      await d.post(h, d.cmd("hit"));
      await d.post(h, d.cmd("stop"));
      await d.post(h, d.cmd("deal 5"), null, 61_000);
      // Coming back is the player's own doing: a deposit at the table, then a deal.
      await d.chain({ op: "deposit", name: "p0", amount: fangs(20) });
      await d.play(h, "2");
      await d.post(h, d.cmd("balance"));
      await d.post(h, d.cmd("stop"));
      await d.post(h, d.cmd("stop"), null, 61_000);
    },
  },
  {
    kind: "a slow dealer, and one that never shows",
    setup: (i) => ({ dealerDelay: [3, 15, null][i % 3], players: [player("p0", HANDLES[(i + 6) % HANDLES.length])] }),
    run: async (d) => {
      const h = d.setup.players[0].handle as string;
      if (d.world.table.dealerDelay === null) {
        // The dealer never deals: a move gets "waiting", and past the deadline the bot calls the timeout.
        const dealt = await d.post(h, d.cmd(`deal ${d.pick(BETS)}`));
        await d.post(h, d.cmd("hit"), dealt.post.id);
        await d.settleDealer(h);
        // The dealer comes back for the deal, then goes missing mid-hand.
        await d.chain({ op: "dealer", delay: 0 });
        await d.post(h, d.cmd(`deal ${d.pick(BETS)}`));
        await d.chain({ op: "dealer", delay: null });
        await d.finish(h);
        await d.chain({ op: "dealer", delay: 0 });
      } else {
        await d.play(h);
        if (d.world.table.dealerDelay === 15) {
          // A move while the dealer still owes the deal: it waits for the cards, then goes through.
          const dealt = await d.post(h, d.cmd(`deal ${d.pick(BETS)}`));
          if (d.hand(h)?.awaiting) await d.post(h, d.cmd("hit"), dealt.post.id);
          await d.settleDealer(h);
          await d.finish(h);
        }
      }
      await d.play(h);
      await d.post(h, d.cmd("balance"));
    },
  },
  {
    kind: "a paused table, and grants that end",
    setup: (i) => ({ dealerDelay: 0, players: [player("p0", HANDLES[(i + 7) % HANDLES.length], { grant: { maxBet: fangs(25), dailyCap: fangs(200), expiresIn: "20" } })] }),
    run: async (d) => {
      const h = d.setup.players[0].handle as string;
      await d.chain({ op: "pause", on: true });
      await d.post(h, d.cmd("deal 5"));
      await d.chain({ op: "pause", on: false });
      // The grant was for 20 blocks.
      await d.post(h, d.cmd("deal 5"), null, 61_000);
      await d.chain({ op: "revoke", name: "p0" });
      await d.post(h, d.cmd("deal 5"), null, 61_000);
      await d.post(h, d.cmd("balance"));
      await d.chain({ op: "grant", name: "p0", maxBet: fangs(25), dailyCap: fangs(200), expiresIn: "40000" });
      await d.post(h, d.cmd(`deal ${d.pick(BETS)}`), null, 61_000);
      if (d.hand(h)) {
        // The player takes the grant back mid-hand: the bot can't move, and says so.
        await d.chain({ op: "revoke", name: "p0" });
        await d.post(h, d.cmd("hit"), d.hand(h).thread[0], 61_000);
        await d.chain({ op: "grant", name: "p0", maxBet: fangs(25), dailyCap: fangs(200), expiresIn: "40000" });
        await d.finish(h);
      }
      await d.post(h, d.cmd("balance"));
    },
  },
  {
    kind: "commands out of order",
    setup: (i) => ({ dealerDelay: 0, players: [player("p0", HANDLES[(i + 8) % HANDLES.length])] }),
    run: async (d) => {
      const h = d.setup.players[0].handle as string;
      await d.post(h, d.cmd("hit"));
      await d.post(h, d.cmd("stand"), "1234567890");
      const dealt = await d.post(h, d.cmd(`deal ${d.pick(BETS)}`), null, 61_000);
      if (d.hand(h)) {
        // Not a reply; a reply to something else; two at once.
        await d.post(h, d.cmd("stand"), null, 61_000);
        await d.post(h, d.cmd("hit"), "1234567890");
        // Faster than the cooldown: seen, not answered.
        await d.post(h, d.cmd("balance"), null, 0);
        await d.post(h, d.cmd("hit"), dealt.post.id, 1_000);
        await d.post(h, d.cmd("balance"), null, 1_000);
        if (d.hand(h) && d.index % 20 >= 10) {
          // The player walks away: the dealer stands them, and the bot says how it ended.
          await d.chain({ op: "idle", name: "p0" });
          await d.tick();
        }
        await d.finish(h);
      }
      await d.post(h, d.cmd("hit"), dealt.post.id, 61_000);
      await d.play(h);
      await d.post(h, d.cmd("balance"));
    },
  },
];

interface ExpectedThread {
  name: string;
  results: [string, string][];
  replies: ExpectedReply[];
  /** Each player's table balance at the end, drops. */
  balances: Record<string, string>;
}

const threads: Thread[] = [];
const expected: ExpectedThread[] = [];
for (let i = 0; i < 100; i++) {
  const scenario = SCENARIOS[i % SCENARIOS.length];
  const d = new Director(i, scenario.kind, scenario.setup(Math.floor(i / SCENARIOS.length)));
  await scenario.run(d);
  // Every thread ends with every hand settled.
  for (const p of d.setup.players) if (p.handle && d.hand(p.handle) && p.link === "verified") await d.finish(p.handle);
  await d.tick();
  threads.push({ name: d.seed, kind: scenario.kind, seed: d.seed, setup: d.setup, steps: d.steps });
  expected.push({
    name: d.seed,
    results: d.world.results,
    replies: d.world.sink.replies.map(expectedOf),
    balances: Object.fromEntries(d.setup.players.map((p) => [p.name, String(d.world.table.players.get(d.world.table.account(p.name))?.balance ?? 0n)])),
  });
}

/** One thread a line, so a change shows as the threads it touched. */
const file = (items: unknown[]) => `[\n${items.map((t) => JSON.stringify(t)).join(",\n")}\n]\n`;
writeFileSync(new URL("./fixtures/threads.json", import.meta.url), file(threads));
writeFileSync(new URL("./fixtures/expected.json", import.meta.url), file(expected));
const posts = threads.reduce((n, t) => n + t.steps.filter((s) => s.t === "post").length, 0);
console.log(`wrote ${threads.length} threads: ${posts} posts, ${expected.reduce((n, t) => n + t.replies.length, 0)} replies`);
