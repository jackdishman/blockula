// A blackjack table on the mock node (e2e/mock-node.ts): the program's record, its storage entries
// in the program's own layout (node/vm/guests/blackjack), its calls run the way the program runs
// them (the same checks, abort codes and events: a bet that carries its seed's hash, the seed's
// reveal, the timeout that ends a hand whose seed never came), and a dealer the tests drive. The rules, the
// hash chains and the cards are `@blockula/blackjack`'s, so every link revealed here is a real
// chain's and the page's own check (`verifyHand`) holds for every hand.
import {
  DEFAULT_LIMITS,
  DROPS_PER_FANG,
  Outcome,
  Phase,
  addressBytes,
  card,
  chainFromTop,
  exposure,
  handOf,
  input,
  natural,
  owedLinks,
  payout,
  seedHash,
  storageKey,
  toHex,
  total,
  type Game,
  type House,
  type LastHand,
  type Limits,
  type PlayerRecord,
} from "@blockula/blackjack";
import { interfaceHash, methodSelector, versionDigest, wasmHash } from "@blockula/sdk";
import { randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { addressOf, PROGRAMS_ON, type MockNode, type ProgramRun } from "./mock-node";

/** The table's address, its owner and its dealer. */
export const TABLE = addressOf("b7".repeat(32));
export const TABLE_OWNER = addressOf("b8".repeat(32));
export const DEALER = addressOf("b9".repeat(32));
export const RELAYER = addressOf("ba".repeat(32));

const F = DROPS_PER_FANG;
const le8 = (n: bigint) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, n, true);
  return toHex(b);
};
const u8 = (n: number) => n.toString(16).padStart(2, "0");
const addr = (a: string) => toHex(addressBytes(a));
const ZERO32 = "00".repeat(32);
const padded = (cards: readonly number[], n: number) => toHex(Uint8Array.from({ length: n }, (_, i) => cards[i] ?? 0));

/** A player's record as the mock keeps it: the library's, with every field set. */
type Seat = { -readonly [K in keyof PlayerRecord]: PlayerRecord[K] };

/** The first cards a chain would deal for a seed and a hand: what a test asks the next hand to be. */
export type Script = (cards: number[]) => boolean;

/** A hand the player can hit once without busting or reaching 21, so a hit and then a stand are both theirs to make. */
export const HIT_THEN_STAND: Script = (c) => total([c[0], c[2]]).total <= 11 && total([c[0], c[2], c[3]]).total <= 20;
/** A hand that is the player's turn after the deal (no natural). */
export const PLAYERS_TURN: Script = (c) => !natural([c[0], c[2]]);
/** A natural the dealer's upcard can't match: it settles on the deal. */
export const NATURAL: Script = (c) => natural([c[0], c[2]]) && total([c[1]]).total < 10;

export class MockTable {
  house: House;
  limits: Limits = { ...DEFAULT_LIMITS };
  players = new Map<string, Seat>();
  games = new Map<bigint, Game>();
  /** Every chain the dealer made, by its hand's id: `chain[0]` is the commitment. */
  chains = new Map<bigint, string[]>();
  relayers = new Set<string>([RELAYER]);
  /**
   * What the next hand's first cards must be. A real dealer commits before it could know the
   * seed; this one is told the seed ahead (`knownSeeds`: the spec reads it from the page's own
   * storage) so a test can ask for a hand, and commits to a chain that deals it. A bet whose seed
   * it wasn't told takes the chain committed ahead, as on a real table.
   */
  script: Script | null = null;
  /** Seeds the dealer was told before their bets (see `script`). */
  knownSeeds: string[] = [];
  /** The digest every call must bind. */
  readonly digest: string;

  constructor(private readonly node: MockNode, readonly address = TABLE, bankroll = 2000n * F) {
    this.house = { owner: TABLE_OWNER, dealer: DEALER, bankroll, reserved: 0n, stakes: 0n, table: 0n, nextCommit: 0n, nextGame: 0n, paused: false, open: 0 };
    const wasm = Uint8Array.from({ length: 1200 }, (_, i) => (i < 8 ? [0, 97, 115, 109, 1, 0, 0, 0][i] : (i * 11) % 251));
    const iface = readFileSync(join(__dirname, "../../node/vm/tests/programs/blackjack.iface.json"));
    const [wasm_hash, interface_hash] = [wasmHash(wasm), interfaceHash(Uint8Array.from([1, 7, 7, 7]))];
    this.digest = versionDigest(address, wasm_hash, interface_hash, 1);
    const methods = (JSON.parse(iface.toString("utf8")) as { methods: { name: string; kind: string; payable: boolean; args: { name: string; type: unknown }[]; returns: unknown }[] }).methods.map((m) => ({
      untrusted_name: m.name,
      selector: methodSelector(m.name),
      kind: m.kind,
      payable: m.payable,
      args: m.args.map((a) => ({ untrusted_name: a.name, type: a.type })),
      returns: m.returns,
    }));
    const label = (a: string) => node.label(a);
    node.fuel = PROGRAMS_ON;
    node.credit(address, "0");
    node.acct(address).balance = bankroll;
    node.programs.set(address, {
      address, creator: label(TABLE_OWNER), created_at: 3, code: { wasm_hash, size: wasm.length, interface_hash }, version: 1, version_digest: this.digest,
      authority: null, upgrade_delay_blocks: 0, policy: "open", upgradability: "immutable", accepts_transfers: false, balance: bankroll.toString(), balance_display: `${bankroll / F} FANG`,
      storage: { items: 3, bytes: 466, deposit: "46600000", sponsored: "0" }, record_deposit: "25600000", record_payer: label(TABLE_OWNER), methods,
      proof_path: `/api/proof/66/${Buffer.from(address).toString("hex")}`,
    });
    node.accountExtra.set(address, { program: { creator: label(TABLE_OWNER), created_at: 3, version_digest: this.digest, upgradability: "immutable", accepts_transfers: false, link: `/api/program/${address}` } });
    node.storage.set(address, () => this.entries());
    node.programRuns.set(address, (c, from) => this.run(c, from));
  }

  /** The dealer commits `n` chains ahead of the bets. */
  commit(n: number) {
    for (let i = 0; i < n; i++) this.chains.set(this.house.nextCommit++, chainFromTop(toHex(randomBytes(32))));
  }

  seat(account: string): Seat {
    let p = this.players.get(account);
    if (!p) this.players.set(account, (p = { balance: 0n, day: 0n, wagered: 0n, openGame: null, grant: null, last: null }));
    return p;
  }

  /** The program's storage, in its own layout. */
  entries(): Map<string, string> {
    const h = this.house;
    const out = new Map<string, string>();
    out.set(storageKey.house, addr(h.owner) + addr(h.dealer) + le8(h.bankroll) + le8(h.reserved) + le8(h.stakes) + le8(h.table) + le8(h.nextCommit) + le8(h.nextGame) + u8(h.paused ? 1 : 0) + u8(h.open));
    out.set(storageKey.limits, input.setLimits(this.limits));
    const open = [...this.games.keys()];
    out.set(storageKey.open, Array.from({ length: 32 }, (_, i) => le8(open[i] ?? 0n)).join(""));
    for (let k = h.nextGame; k < h.nextCommit; k++) out.set(storageKey.commitment(k), (this.chains.get(k) as string[])[0]);
    for (const g of this.games.values())
      out.set(storageKey.game(g.id), addr(g.player) + le8(g.stake) + (g.clientSeed ?? g.seedHash ?? ZERO32) + g.link + le8(g.deadline) + le8(g.postId) + u8(g.phase) + u8(g.table ? 1 : 0) + u8(g.dealt) + u8(g.playerCards.length) + u8(g.dealerCards.length) + padded(g.playerCards, 20) + padded(g.dealerCards, 12));
    for (const [a, p] of this.players) {
      const g = p.grant;
      const l = p.last;
      out.set(
        storageKey.player(a),
        le8(p.balance) + le8(p.day) + le8(p.wagered) + le8(p.openGame === null ? 0n : p.openGame + 1n) + (g ? addr(g.relayer) + le8(g.maxBet) + le8(g.dailyCap) + le8(g.expiresAt) + le8(g.day) + le8(g.wagered) : ZERO32 + le8(0n).repeat(5)) +
          (l ? le8(l.game) + u8(l.outcome) + u8(l.dealt) + u8(l.nPlayer) + le8(l.stake) + le8(l.payout) + (l.clientSeed ?? l.seedHash ?? ZERO32) + l.link : le8(0n) + u8(0).repeat(3) + le8(0n).repeat(2) + ZERO32.repeat(2)),
      );
    }
    for (const r of this.relayers) out.set(storageKey.relayer(r), "01");
    return out;
  }

  private abort(code: number, message: string): ProgramRun {
    return { ok: false, outcome: { ok: false, number: 2, code: "aborted", program: this.address, abort_code: code, untrusted_message: message } };
  }

  /** A chain for hand `id` whose first cards, with the seed `from`'s bet committed to with `hash`, are what the script asks for. */
  private chainFor(id: bigint, from: string, hash: string): string[] {
    const script = this.script;
    const seed = this.knownSeeds.find((s) => seedHash(from, s) === hash);
    if (!script || !seed) return this.chains.get(id) as string[];
    for (let tries = 0; tries < 200_000; tries++) {
      const chain = chainFromTop(toHex(randomBytes(32)));
      if (script(Array.from({ length: 8 }, (_, i) => card(chain[i + 1], seed, id, i + 1)))) return chain;
    }
    throw new Error("no chain deals the scripted hand");
  }

  /** One call, as the program runs it (node/vm/guests/blackjack): the same checks, in the same order, and the same events. */
  private run(c: { method: string; input: string; value: bigint }, from: string): ProgramRun {
    const h = this.house;
    const l = this.limits;
    // The block the call runs in: the mock seals each transaction in the next one.
    const now = BigInt(this.node.height + 1);
    const gameOf = (hex: string) => (hex.length === 16 ? this.games.get(new DataView(Uint8Array.from(Buffer.from(hex, "hex")).buffer).getBigUint64(0, true)) : undefined);
    switch (c.method) {
      case "bet": {
        if (c.input.length !== 64) return this.abort(1, "bad input");
        const stake = c.value;
        const p = this.seat(from);
        if (h.paused) return this.abort(46, "new bets are paused");
        if (stake < l.minBet) return this.abort(47, "below the minimum bet");
        if (stake > l.maxBet || stake > h.bankroll / l.bankrollDivisor) return this.abort(48, "above the maximum bet");
        const reserve = exposure(stake);
        if (reserve > h.bankroll) return this.abort(49, "the bankroll can't cover this bet");
        if (h.reserved + reserve > l.maxExposure) return this.abort(69, "past the house's exposure limit");
        if (BigInt(h.open) >= l.maxOpenGames) return this.abort(51, "the table is full");
        if (p.openGame !== null) return this.abort(52, "finish the open hand first");
        const day = now / l.dayBlocks;
        const wagered = (p.day === day ? p.wagered : 0n) + stake;
        if (wagered > l.dailyCap) return this.abort(50, "past the daily cap");
        const id = h.nextGame;
        if (id >= h.nextCommit) return this.abort(53, "the dealer has no commitment ready");
        // The input is the seed's hash: the hand waits for the seed, and the dealer owes nothing yet.
        const chain = this.chainFor(id, from, c.input);
        const deadline = now + l.seedBlocks;
        const game: Game = { id, player: from, stake, clientSeed: null, seedHash: c.input, link: chain[0], deadline, postId: 0n, phase: Phase.Seed, table: false, dealt: 0, playerCards: [], dealerCards: [], playerTotal: 0, dealerTotal: 0 };
        return {
          ok: true,
          data: le8(id),
          events: [{ untrusted_name: "bet", data: le8(id) + addr(from) + le8(stake) + c.input + chain[0] + le8(deadline) + u8(0) + le8(0n) }],
          apply: () => {
            this.chains.set(id, chain);
            h.bankroll -= reserve;
            h.reserved += reserve;
            h.stakes += stake;
            h.nextGame = id + 1n;
            h.open += 1;
            Object.assign(p, { day, wagered, openGame: id });
            this.games.set(id, game);
          },
        };
      }
      case "reveal_seed": {
        if (c.input.length !== 80) return this.abort(1, "bad input");
        const g = gameOf(c.input.slice(0, 16));
        const seed = c.input.slice(16);
        if (!g) return this.abort(54, "no such hand");
        if (g.player !== from) return this.abort(55, "not this hand's player");
        if (g.phase !== Phase.Seed) return this.abort(70, "the hand isn't waiting for a seed");
        if (now > g.deadline) return this.abort(72, "past the seed's deadline");
        if (seedHash(from, seed) !== g.seedHash) return this.abort(71, "not the seed the bet committed to");
        const deadline = now + l.revealBlocks;
        return { ok: true, events: [{ untrusted_name: "seed", data: le8(g.id) + addr(g.player) + seed + le8(deadline) }], apply: () => Object.assign(g, { clientSeed: seed, seedHash: null, phase: Phase.Deal, deadline }) };
      }
      case "hit":
      case "stand": {
        const g = gameOf(c.input);
        if (!g) return this.abort(54, "no such hand");
        if (g.player !== from) return this.abort(55, "not this hand's player");
        if (g.phase !== Phase.Player) return this.abort(56, "not the player's turn");
        const deadline = now + l.revealBlocks;
        const hit = c.method === "hit";
        return { ok: true, events: [{ untrusted_name: c.method, data: le8(g.id) + addr(g.player) + le8(deadline) + (hit ? "" : u8(0)) }], apply: () => Object.assign(g, { phase: hit ? Phase.Hit : Phase.Dealer, deadline }) };
      }
      case "timeout": {
        const g = gameOf(c.input);
        if (!g) return this.abort(54, "no such hand");
        if (now <= g.deadline) return this.abort(60, "not past the deadline");
        // The seed never came: the stake is the house's.
        if (g.phase === Phase.Seed) return { ok: true, data: u8(Phase.Done) + u8(Outcome.NoSeed), events: [this.settledEvent(g, Outcome.NoSeed)], apply: () => this.settle(g, Outcome.NoSeed) };
        if (g.phase === Phase.Player) {
          const deadline = now + l.revealBlocks;
          return { ok: true, data: u8(Phase.Dealer) + u8(0), events: [{ untrusted_name: "stand", data: le8(g.id) + addr(g.player) + le8(deadline) + u8(1) }], apply: () => Object.assign(g, { phase: Phase.Dealer, deadline }) };
        }
        // The dealer missed a reveal: the player is paid the most the hand could still have paid.
        const outcome = g.phase === Phase.Deal || natural(g.playerCards) ? Outcome.ForfeitDeal : Outcome.Forfeit;
        return { ok: true, data: u8(Phase.Done) + u8(outcome), events: [this.settledEvent(g, outcome)], apply: () => this.settle(g, outcome) };
      }
      case "deposit": {
        if (c.value === 0n) return this.abort(44, "no value");
        const p = this.seat(from);
        if (p.balance + c.value > l.depositCap) return this.abort(62, "past the deposit cap");
        return { ok: true, events: [{ untrusted_name: "deposit", data: addr(from) + le8(c.value) + le8(p.balance + c.value) }], apply: () => ((p.balance += c.value), (h.table += c.value)) };
      }
      case "withdraw": {
        const amount = new DataView(Uint8Array.from(Buffer.from(c.input, "hex")).buffer).getBigUint64(0, true);
        const p = this.seat(from);
        if (amount > p.balance) return this.abort(63, "balance short");
        return {
          ok: true,
          events: [{ untrusted_name: "withdraw", data: addr(from) + le8(amount) + le8(p.balance - amount) }],
          apply: () => {
            p.balance -= amount;
            h.table -= amount;
            this.node.acct(from).balance += amount;
          },
        };
      }
      case "set_relayer": {
        const bytes = Uint8Array.from(Buffer.from(c.input, "hex"));
        const view = new DataView(bytes.buffer);
        const relayer = [...this.relayers].find((r) => addr(r) === c.input.slice(0, 64));
        if (!relayer) return this.abort(64, "not an approved relayer");
        const [maxBet, dailyCap, expiresAt] = [view.getBigUint64(32, true), view.getBigUint64(40, true), view.getBigUint64(48, true)];
        if (!(maxBet >= 1n && maxBet <= l.relayMaxBet && dailyCap >= maxBet && expiresAt > now && expiresAt - now <= l.grantMaxBlocks)) return this.abort(67, "bad grant");
        const p = this.seat(from);
        return { ok: true, events: [{ untrusted_name: "grant", data: addr(from) + addr(relayer) + le8(maxBet) + le8(dailyCap) + le8(expiresAt) }], apply: () => void (p.grant = { relayer, maxBet, dailyCap, expiresAt, day: 0n, wagered: 0n }) };
      }
      case "revoke": {
        const p = this.seat(from);
        return { ok: true, events: [{ untrusted_name: "revoke", data: addr(from) + (p.grant ? addr(p.grant.relayer) : ZERO32) }], apply: () => void (p.grant = null) };
      }
      default:
        return this.abort(1, "the mock table doesn't run this method");
    }
  }

  private settledEvent(g: Game, outcome: Outcome) {
    return { untrusted_name: "settled", data: le8(g.id) + addr(g.player) + u8(outcome) + le8(g.stake) + le8(payout(g.stake, outcome)) + u8(g.table ? 1 : 0) + u8(g.dealt) + u8(g.playerCards.length) + (g.clientSeed ?? g.seedHash ?? ZERO32) + g.link + u8(total(g.playerCards).total) + u8(total(g.dealerCards).total) };
  }

  /** Ends a hand: frees its reserve, removes its record, keeps it as the player's last hand and pays what it pays. */
  private settle(g: Game, outcome: Outcome): LastHand {
    const h = this.house;
    const paid = payout(g.stake, outcome);
    h.reserved -= exposure(g.stake);
    h.stakes -= g.stake;
    h.bankroll += exposure(g.stake) + g.stake - paid;
    h.open -= 1;
    this.games.delete(g.id);
    const p = this.seat(g.player);
    const last: LastHand = { game: g.id, outcome, dealt: g.dealt, nPlayer: g.playerCards.length, stake: g.stake, payout: paid, clientSeed: g.clientSeed, seedHash: g.clientSeed === null ? g.seedHash : null, link: g.link };
    p.openGame = null;
    p.last = last;
    this.node.acct(g.player).balance += paid;
    return last;
  }

  /**
   * The dealer's turn on hand `id`: it reveals every link the hand is owed now (`owedLinks`), in a
   * block of its own, as the dealer daemon's `reveal` does. Returns the hand's outcome once that
   * settles it, else `null`.
   */
  deal(id: bigint): Outcome | null {
    const g = this.games.get(id);
    if (!g) throw new Error(`hand ${id} isn't open`);
    const chain = this.chains.get(id) as string[];
    const links = owedLinks(g, chain);
    if (!links.length) throw new Error(`the dealer owes hand ${id} no card`);
    this.node.height += 1;
    const hand = handOf(g);
    let outcome: Outcome | null = null;
    for (const link of links) {
      const step = hand.deal(card(link, g.clientSeed as string, id, hand.dealt + 1));
      if (step?.kind === "settled") outcome = step.outcome;
    }
    Object.assign(g, { link: links[links.length - 1], dealt: hand.dealt, phase: hand.phase, playerCards: [...hand.player], dealerCards: [...hand.dealer], playerTotal: total(hand.player).total, dealerTotal: total(hand.dealer).total });
    // The player's turn: they have the idle blocks to move.
    if (hand.phase === Phase.Player) g.deadline = BigInt(this.node.height) + this.limits.idleBlocks;
    if (outcome !== null) this.settle(g, outcome);
    return outcome;
  }

  /** A bet by `account` that no page made (another player at the table), both transactions: the hand's id. `reveal: false` leaves it waiting for its seed. */
  place(account: string, stake: bigint, seed = toHex(randomBytes(32)), reveal = true): bigint {
    this.knownSeeds.push(seed);
    const run = this.run({ method: "bet", input: seedHash(account, seed), value: stake }, account);
    if (!run.ok) throw new Error(`the table refused the bet: ${JSON.stringify(run.outcome)}`);
    this.node.height += 1;
    run.apply?.();
    const id = this.seat(account).openGame as bigint;
    if (reveal) {
      const opened = this.run({ method: "reveal_seed", input: le8(id) + seed, value: 0n }, account);
      if (!opened.ok) throw new Error(`the table refused the seed: ${JSON.stringify(opened.outcome)}`);
      this.node.height += 1;
      opened.apply?.();
    }
    return id;
  }

  /** The one open hand of `account`, as the table has it. */
  openHand(account: string): Game {
    const id = this.seat(account).openGame;
    const g = id === null ? undefined : this.games.get(id);
    if (!g) throw new Error(`${account} has no open hand`);
    return g;
  }
}

/** A funded table on `node`, with `commits` chains committed ahead. */
export function addTable(node: MockNode, opts: { commits?: number; bankroll?: bigint } = {}): MockTable {
  const t = new MockTable(node, TABLE, opts.bankroll);
  t.commit(opts.commits ?? 4);
  return t;
}
