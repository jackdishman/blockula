/**
 * The blackjack program's bytes (`node/vm/guests/blackjack`): the inputs its methods take, the
 * records it stores, the events it emits. Everything here is a fixed little-endian layout the
 * program's doc comment gives; inputs are Borsh for the interface's types.
 *
 * Text that comes back from the chain is data. Nothing here acts on it.
 */
import { blake3 } from "@noble/hashes/blake3.js";
import { bech32m } from "@scure/base";
import { Outcome, Phase, hexBytes, isOutcome, seedHashOf, toHex, total } from "./rules.js";

export const DROPS_PER_FANG = 1_000_000_000n;
export const MAX_REVEAL_LINKS = 12;
export const MAX_COMMIT_BATCH = 64;
export const MAX_OPEN_GAMES = 32;

// ---------------------------------------------------------------------------------- bytes

/** An address's 32 bytes. */
export function addressBytes(address: string): Uint8Array {
  const { words } = bech32m.decode(address as `${string}1${string}`);
  const bytes = bech32m.fromWords(words);
  if (bytes.length !== 32) throw new Error(`${address} isn't an account address`);
  return Uint8Array.from(bytes);
}

/** The address with these 32 bytes on a chain whose addresses start with `prefix`. */
export function bytesAddress(bytes: Uint8Array, prefix: string): string {
  if (bytes.length !== 32) throw new Error("an address is 32 bytes");
  return bech32m.encode(prefix, bech32m.toWords(bytes));
}

/** An address's chain prefix ("boo", "tboo"). */
export function prefixOf(address: string): string {
  return bech32m.decode(address as `${string}1${string}`).prefix;
}

class Writer {
  private parts: Uint8Array[] = [];
  u8(n: number): this {
    this.parts.push(Uint8Array.of(n));
    return this;
  }
  u32(n: number): this {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, n, true);
    this.parts.push(b);
    return this;
  }
  u64(n: bigint): this {
    if (n < 0n || n >= 1n << 64n) throw new Error(`${n} isn't a u64`);
    const b = new Uint8Array(8);
    new DataView(b.buffer).setBigUint64(0, n, true);
    this.parts.push(b);
    return this;
  }
  hash(hex: string): this {
    this.parts.push(hexBytes(hex, 32));
    return this;
  }
  address(a: string): this {
    this.parts.push(addressBytes(a));
    return this;
  }
  bool(b: boolean): this {
    return this.u8(b ? 1 : 0);
  }
  hex(): string {
    return this.parts.map(toHex).join("");
  }
}

class Reader {
  private at = 0;
  private readonly view: DataView;
  constructor(private readonly bytes: Uint8Array, private readonly what: string) {
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }
  private take(n: number): number {
    if (this.at + n > this.bytes.length) throw new Error(`${this.what}: ${this.bytes.length} bytes is too short`);
    const at = this.at;
    this.at += n;
    return at;
  }
  u8(): number {
    return this.bytes[this.take(1)];
  }
  u32(): number {
    return this.view.getUint32(this.take(4), true);
  }
  u64(): bigint {
    return this.view.getBigUint64(this.take(8), true);
  }
  raw(n: number): Uint8Array {
    const at = this.take(n);
    return this.bytes.slice(at, at + n);
  }
  hash(): string {
    return toHex(this.raw(32));
  }
  address(prefix: string): string {
    return bytesAddress(this.raw(32), prefix);
  }
  get left(): number {
    return this.bytes.length - this.at;
  }
  end(): void {
    if (this.at !== this.bytes.length) throw new Error(`${this.what}: ${this.bytes.length - this.at} bytes left over`);
  }
}

const u64 = (n: bigint | number): bigint => {
  const b = BigInt(n);
  if (b < 0n || b >= 1n << 64n) throw new Error(`${n} isn't a u64`);
  return b;
};

// ---------------------------------------------------------------------------------- inputs

/** The table's limits, in `set_limits`' order. Amounts are drops. */
export interface Limits {
  minBet: bigint;
  maxBet: bigint;
  /** The most an account may wager in a day. */
  dailyCap: bigint;
  /** Blocks in a day. */
  dayBlocks: bigint;
  maxOpenGames: bigint;
  /** A bet is at most the free bankroll divided by this. */
  bankrollDivisor: bigint;
  /** The most the house reserves over all open hands. */
  maxExposure: bigint;
  /** Blocks the dealer has to reveal a card it owes. */
  revealBlocks: bigint;
  /** Blocks a player has to move. */
  idleBlocks: bigint;
  /** The most a table balance may be deposited up to. */
  depositCap: bigint;
  /** The most a relayed bet may be. */
  relayMaxBet: bigint;
  /** The longest a relayer grant may last, in blocks. */
  grantMaxBlocks: bigint;
  /** Blocks a player has, from the bet, to reveal the seed it committed to. */
  seedBlocks: bigint;
}

/** The fewest blocks the program lets an owner give a player to move or to reveal a seed. */
export const MIN_PLAYER_BLOCKS = 20n;
/** The most blocks the program lets an owner give the dealer to reveal a card. */
export const MAX_DEALER_BLOCKS = 86_400n;

const LIMIT_KEYS = ["minBet", "maxBet", "dailyCap", "dayBlocks", "maxOpenGames", "bankrollDivisor", "maxExposure", "revealBlocks", "idleBlocks", "depositCap", "relayMaxBet", "grantMaxBlocks", "seedBlocks"] as const;

/** The limits `init` writes. */
export const DEFAULT_LIMITS: Limits = Object.freeze({
  minBet: DROPS_PER_FANG,
  maxBet: 50n * DROPS_PER_FANG,
  dailyCap: 500n * DROPS_PER_FANG,
  dayBlocks: 43_200n,
  maxOpenGames: 10n,
  bankrollDivisor: 20n,
  maxExposure: 750n * DROPS_PER_FANG,
  revealBlocks: 150n,
  idleBlocks: 1_800n,
  depositCap: 200n * DROPS_PER_FANG,
  relayMaxBet: 25n * DROPS_PER_FANG,
  grantMaxBlocks: 43_200n,
  seedBlocks: 300n,
});

function counted(head: Writer, items: readonly string[], max: number, what: string): string {
  if (items.length < 1 || items.length > max) throw new Error(`${what}: 1 to ${max} at a time, not ${items.length}`);
  head.u32(items.length);
  for (const h of items) head.hash(h);
  return head.hex();
}

/** Each method's input, as lowercase hex. */
export const input = {
  init: (dealer: string) => new Writer().address(dealer).hex(),
  fund: () => "",
  defund: (amount: bigint) => new Writer().u64(amount).hex(),
  setLimits: (l: Limits) => LIMIT_KEYS.reduce((w, k) => w.u64(u64(l[k])), new Writer()).hex(),
  pause: (on: boolean) => new Writer().bool(on).hex(),
  setDealer: (dealer: string) => new Writer().address(dealer).hex(),
  approveRelayer: (relayer: string, on: boolean) => new Writer().address(relayer).bool(on).hex(),
  commitBatch: (first: bigint, commitments: readonly string[]) => counted(new Writer().u64(first), commitments, MAX_COMMIT_BATCH, "commitments"),
  skipCommits: (count: number) => new Writer().u32(count).hex(),
  deposit: () => "",
  withdraw: (amount: bigint) => new Writer().u64(amount).hex(),
  setRelayer: (relayer: string, maxBet: bigint, dailyCap: bigint, expiresAt: bigint) => new Writer().address(relayer).u64(maxBet).u64(dailyCap).u64(expiresAt).hex(),
  revoke: () => "",
  /** A bet carries the seed's hash (`seedHash`), never the seed. */
  bet: (seedHash: string) => new Writer().hash(seedHash).hex(),
  revealSeed: (game: bigint, clientSeed: string) => new Writer().u64(game).hash(clientSeed).hex(),
  relayBet: (player: string, amount: bigint, postId: bigint) => new Writer().address(player).u64(amount).u64(postId).hex(),
  hit: (game: bigint) => new Writer().u64(game).hex(),
  stand: (game: bigint) => new Writer().u64(game).hex(),
  relayHit: (game: bigint, postId: bigint) => new Writer().u64(game).u64(postId).hex(),
  relayStand: (game: bigint, postId: bigint) => new Writer().u64(game).u64(postId).hex(),
  relayWithdraw: (player: string, postId: bigint) => new Writer().address(player).u64(postId).hex(),
  reveal: (game: bigint, links: readonly string[]) => counted(new Writer().u64(game), links, MAX_REVEAL_LINKS, "links"),
  timeout: (game: bigint) => new Writer().u64(game).hex(),
  // Views.
  game: (game: bigint) => new Writer().u64(game).hex(),
  player: (account: string) => new Writer().address(account).hex(),
  commitment: (index: bigint) => new Writer().u64(index).hex(),
} as const;

/**
 * What `account`'s bet commits to for `clientSeed`: BLAKE3("blockula/blackjack/seed" ‖ account ‖
 * seed). The bet carries this; the seed itself follows in `reveal_seed`, once the bet is final.
 */
export function seedHash(account: string, clientSeed: string): string {
  return seedHashOf(toHex(addressBytes(account)), clientSeed);
}

/** The client seed the program gives a relayed bet: BLAKE3("blockula/blackjack/relay" ‖ player ‖ post id). */
export function relaySeed(player: string, postId: bigint): string {
  const pre = new Uint8Array(64);
  pre.set(new TextEncoder().encode("blockula/blackjack/relay"), 0);
  pre.set(addressBytes(player), 24);
  new DataView(pre.buffer).setBigUint64(56, u64(postId), true);
  return toHex(blake3(pre));
}

// ---------------------------------------------------------------------------------- storage keys

const keyed = (prefix: string, tail: Uint8Array): string => toHex(new TextEncoder().encode(prefix)) + toHex(tail);
const le8 = (n: bigint): Uint8Array => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, u64(n), true);
  return b;
};

/** The program's storage keys, as lowercase hex. */
export const storageKey = {
  house: toHex(new TextEncoder().encode("H")),
  limits: toHex(new TextEncoder().encode("C")),
  open: toHex(new TextEncoder().encode("O")),
  commitment: (k: bigint) => keyed("c", le8(k)),
  game: (id: bigint) => keyed("g", le8(id)),
  player: (account: string) => keyed("p", addressBytes(account)),
  relayer: (account: string) => keyed("r", addressBytes(account)),
};

// ---------------------------------------------------------------------------------- records

export interface House {
  owner: string;
  dealer: string;
  /** The house's free FANG (drops). */
  bankroll: bigint;
  /** The open hands' exposure. */
  reserved: bigint;
  /** The open hands' stakes. */
  stakes: bigint;
  /** The players' table balances. */
  table: bigint;
  nextCommit: bigint;
  nextGame: bigint;
  paused: boolean;
  open: number;
}

export const HOUSE_BYTES = 114;
export const LIMITS_BYTES = 104;
export const GAME_BYTES = 157;
export const PLAYER_BYTES = 195;

export function decodeHouse(hex: string, prefix: string): House {
  const r = new Reader(hexBytes(hex, HOUSE_BYTES), "the house record");
  return { owner: r.address(prefix), dealer: r.address(prefix), bankroll: r.u64(), reserved: r.u64(), stakes: r.u64(), table: r.u64(), nextCommit: r.u64(), nextGame: r.u64(), paused: r.u8() !== 0, open: r.u8() };
}

export function decodeLimits(hex: string): Limits {
  const r = new Reader(hexBytes(hex, LIMITS_BYTES), "the limits");
  const out = {} as Record<(typeof LIMIT_KEYS)[number], bigint>;
  for (const k of LIMIT_KEYS) out[k] = r.u64();
  return out;
}

/** An open hand. */
export interface Game {
  id: bigint;
  player: string;
  stake: bigint;
  /** The player's seed, once revealed (a relayed hand: from the bet). `null` while the hand waits for it. */
  clientSeed: string | null;
  /** What the bet committed to, while the seed isn't revealed (`Phase.Seed`); `null` after. */
  seedHash: string | null;
  /** The newest link revealed (the commitment before any is). */
  link: string;
  /** The block after which `timeout` works: the player's deadline (the seed, a move) or the dealer's. */
  deadline: bigint;
  /** The X post a relayed bet came from; 0 for a bet from an account. */
  postId: bigint;
  phase: Phase;
  /** Played from the table balance: the payout goes back there. */
  table: boolean;
  /** Cards dealt: the chain index of `link`. */
  dealt: number;
  playerCards: number[];
  dealerCards: number[];
  playerTotal: number;
  dealerTotal: number;
}

function phase(n: number): Phase {
  if (!Number.isInteger(n) || n < 0 || n > 5) throw new Error(`${n} isn't a hand's phase`);
  return n as Phase;
}

export function decodeGame(id: bigint, hex: string, prefix: string): Game {
  const r = new Reader(hexBytes(hex, GAME_BYTES), "a hand's record");
  const player = r.address(prefix);
  const stake = r.u64();
  const seedField = r.hash();
  const link = r.hash();
  const deadline = r.u64();
  const postId = r.u64();
  const ph = phase(r.u8());
  // The same 32 bytes hold the seed's hash until the seed is revealed, and the seed after.
  const clientSeed = ph === Phase.Seed ? null : seedField;
  const seedHash = ph === Phase.Seed ? seedField : null;
  const flags = r.u8();
  const dealt = r.u8();
  const nPlayer = r.u8();
  const nDealer = r.u8();
  if (nPlayer > 20 || nDealer > 12) throw new Error("a hand's record holds more cards than a hand can");
  const playerCards = Array.from(r.raw(20).slice(0, nPlayer));
  const dealerCards = Array.from(r.raw(12).slice(0, nDealer));
  r.end();
  return { id, player, stake, clientSeed, seedHash, link, deadline, postId, phase: ph, table: (flags & 1) !== 0, dealt, playerCards, dealerCards, playerTotal: total(playerCards).total, dealerTotal: total(dealerCards).total };
}

/** A player's last settled hand: enough to recompute every card (`verifyHand`). */
export interface LastHand {
  game: bigint;
  outcome: Outcome;
  /** Cards dealt in all. */
  dealt: number;
  /** How many of them were the player's. */
  nPlayer: number;
  stake: bigint;
  payout: bigint;
  /** The player's seed. `null` for a hand whose seed was never revealed (`Outcome.NoSeed`). */
  clientSeed: string | null;
  /** For `Outcome.NoSeed`: what the bet committed to. `null` otherwise. */
  seedHash: string | null;
  /** The last link revealed (the commitment, when none was). */
  link: string;
}

/** The seed field of a settled hand, by its outcome: the seed, or for a hand whose seed never came the hash it committed to. */
function settledSeed(outcome: number, field: string): { clientSeed: string | null; seedHash: string | null } {
  return outcome === Outcome.NoSeed ? { clientSeed: null, seedHash: field } : { clientSeed: field, seedHash: null };
}

export interface Grant {
  relayer: string;
  maxBet: bigint;
  dailyCap: bigint;
  /** The last block the grant works in. */
  expiresAt: bigint;
  day: bigint;
  wagered: bigint;
}

export interface PlayerRecord {
  /** Table balance, drops. */
  balance: bigint;
  /** The day (height / day blocks) `wagered` counts. */
  day: bigint;
  wagered: bigint;
  /** The open hand's id, if any. */
  openGame: bigint | null;
  grant: Grant | null;
  last: LastHand | null;
}

export function decodePlayer(hex: string, prefix: string): PlayerRecord {
  const r = new Reader(hexBytes(hex, PLAYER_BYTES), "a player's record");
  const balance = r.u64();
  const day = r.u64();
  const wagered = r.u64();
  const open = r.u64();
  const relayerBytes = r.raw(32);
  const grant = { maxBet: r.u64(), dailyCap: r.u64(), expiresAt: r.u64(), day: r.u64(), wagered: r.u64() };
  const game = r.u64();
  const outcome = r.u8();
  const dealt = r.u8();
  const nPlayer = r.u8();
  const stake = r.u64();
  const payout = r.u64();
  const seedField = r.hash();
  const link = r.hash();
  r.end();
  const hasRelayer = relayerBytes.some((b) => b !== 0);
  if (outcome !== 0 && !isOutcome(outcome)) throw new Error(`${outcome} isn't an outcome`);
  return {
    balance,
    day,
    wagered,
    openGame: open === 0n ? null : open - 1n,
    grant: hasRelayer ? { relayer: bytesAddress(relayerBytes, prefix), ...grant } : null,
    last: outcome === 0 ? null : { game, outcome: outcome as Outcome, dealt, nPlayer, stake, payout, ...settledSeed(outcome, seedField), link },
  };
}

/** A player who never touched the table. */
export const EMPTY_PLAYER: PlayerRecord = Object.freeze({ balance: 0n, day: 0n, wagered: 0n, openGame: null, grant: null, last: null });

/** The open hands' ids, from the stored list and the house's count. */
export function decodeOpenList(hex: string, open: number): bigint[] {
  const r = new Reader(hexBytes(hex, 8 * MAX_OPEN_GAMES), "the open list");
  if (open > MAX_OPEN_GAMES) throw new Error("more open hands than the list holds");
  return Array.from({ length: open }, () => r.u64());
}

/** A view's `bytes` return: the `u32` length, then the bytes (as hex). */
export function viewBytes(hex: string): string {
  const b = hexBytes(hex);
  const r = new Reader(b, "a view's return");
  const n = r.u32();
  if (n !== r.left) throw new Error("a view's return isn't the length it says");
  return toHex(r.raw(n));
}

// ---------------------------------------------------------------------------------- events

export type TableEvent =
  /** A bet from an account carries `seedHash` (the seed follows in a `seed` event); a relayed one (`table`) its `clientSeed`. */
  | { name: "bet"; game: bigint; player: string; stake: bigint; seedHash: string | null; clientSeed: string | null; commit: string; deadline: bigint; table: boolean; postId: bigint }
  /** The player revealed the seed their bet committed to: the dealer owes the deal by `deadline`. */
  | { name: "seed"; game: bigint; player: string; clientSeed: string; deadline: bigint }
  | { name: "cards"; game: bigint; player: string; first: number; cards: { link: string; card: number }[]; phase: Phase; playerTotal: number; dealerTotal: number; deadline: bigint }
  | { name: "hit"; game: bigint; player: string; deadline: bigint }
  | { name: "stand"; game: bigint; player: string; deadline: bigint; idle: boolean }
  | { name: "settled"; game: bigint; player: string; outcome: Outcome; stake: bigint; payout: bigint; toTable: boolean; dealt: number; nPlayer: number; clientSeed: string | null; seedHash: string | null; link: string; playerTotal: number; dealerTotal: number }
  | { name: "relayed"; player: string; relayer: string; postId: bigint; action: "bet" | "hit" | "stand" | "withdraw"; game: bigint }
  | { name: "fund"; from: string; amount: bigint; bankroll: bigint }
  | { name: "defund"; amount: bigint; bankroll: bigint }
  | { name: "limits"; limits: Limits }
  | { name: "paused"; on: boolean }
  | { name: "dealer"; dealer: string }
  | { name: "relayer"; relayer: string; on: boolean }
  | { name: "commit"; first: bigint; commitments: string[] }
  | { name: "skipped"; first: bigint; count: number }
  | { name: "deposit" | "withdraw"; player: string; amount: bigint; balance: bigint }
  | { name: "grant"; player: string; relayer: string; maxBet: bigint; dailyCap: bigint; expiresAt: bigint }
  | { name: "revoke"; player: string; relayer: string | null };

const RELAY_ACTIONS = ["bet", "hit", "stand", "withdraw"] as const;

/**
 * One of the program's events, decoded; `null` for a name it doesn't emit. Throws on data that
 * isn't the event's layout. An event is the program's claim: decode only events whose `program`
 * is the table you mean.
 */
export function decodeEvent(name: string, dataHex: string, prefix: string): TableEvent | null {
  const r = new Reader(hexBytes(dataHex), `a ${name} event`);
  const done = <T>(e: T): T => {
    r.end();
    return e;
  };
  switch (name) {
    case "bet": {
      const game = r.u64();
      const player = r.address(prefix);
      const stake = r.u64();
      const seedField = r.hash();
      const commit = r.hash();
      const deadline = r.u64();
      const table = (r.u8() & 1) !== 0;
      // A relayed bet's seed is derived and known at once; a bet from an account shows only its hash.
      return done({ name, game, player, stake, seedHash: table ? null : seedField, clientSeed: table ? seedField : null, commit, deadline, table, postId: r.u64() });
    }
    case "seed":
      return done({ name, game: r.u64(), player: r.address(prefix), clientSeed: r.hash(), deadline: r.u64() });
    case "cards": {
      const game = r.u64();
      const player = r.address(prefix);
      const first = r.u8();
      const n = r.u8();
      const cards = Array.from({ length: n }, () => ({ link: r.hash(), card: r.u8() }));
      return done({ name, game, player, first, cards, phase: phase(r.u8()), playerTotal: r.u8(), dealerTotal: r.u8(), deadline: r.u64() });
    }
    case "hit":
      return done({ name, game: r.u64(), player: r.address(prefix), deadline: r.u64() });
    case "stand":
      return done({ name, game: r.u64(), player: r.address(prefix), deadline: r.u64(), idle: r.u8() === 1 });
    case "settled": {
      const game = r.u64();
      const player = r.address(prefix);
      const outcome = r.u8();
      if (!isOutcome(outcome)) throw new Error(`${outcome} isn't an outcome`);
      const stake = r.u64();
      const payout = r.u64();
      const toTable = r.u8() === 1;
      const dealt = r.u8();
      const nPlayer = r.u8();
      const seed = settledSeed(outcome, r.hash());
      return done({ name, game, player, outcome, stake, payout, toTable, dealt, nPlayer, ...seed, link: r.hash(), playerTotal: r.u8(), dealerTotal: r.u8() });
    }
    case "relayed": {
      const player = r.address(prefix);
      const relayer = r.address(prefix);
      const postId = r.u64();
      const action = RELAY_ACTIONS[r.u8() - 1];
      if (!action) throw new Error("a relayed event names no action");
      return done({ name, player, relayer, postId, action, game: r.u64() });
    }
    case "fund":
      return done({ name, from: r.address(prefix), amount: r.u64(), bankroll: r.u64() });
    case "defund":
      return done({ name, amount: r.u64(), bankroll: r.u64() });
    case "limits":
      return { name, limits: decodeLimits(dataHex) };
    case "paused":
      return done({ name, on: r.u8() === 1 });
    case "dealer":
      return done({ name, dealer: r.address(prefix) });
    case "relayer":
      return done({ name, relayer: r.address(prefix), on: r.u8() === 1 });
    case "commit": {
      const first = r.u64();
      if (r.left % 32 !== 0) throw new Error("a commit event's commitments aren't whole");
      return { name, first, commitments: Array.from({ length: r.left / 32 }, () => r.hash()) };
    }
    case "skipped":
      return done({ name, first: r.u64(), count: r.u32() });
    case "deposit":
    case "withdraw":
      return done({ name, player: r.address(prefix), amount: r.u64(), balance: r.u64() });
    case "grant":
      return done({ name, player: r.address(prefix), relayer: r.address(prefix), maxBet: r.u64(), dailyCap: r.u64(), expiresAt: r.u64() });
    case "revoke": {
      const player = r.address(prefix);
      const relayer = r.raw(32);
      return done({ name, player, relayer: relayer.some((b) => b !== 0) ? bytesAddress(relayer, prefix) : null });
    }
    default:
      return null;
  }
}

/** `reveal`'s and `timeout`'s return: the hand's phase and, once settled, its outcome. */
export function decodeStep(hex: string): { phase: Phase; outcome: Outcome | null } {
  const b = hexBytes(hex, 2);
  if (b[1] !== 0 && !isOutcome(b[1])) throw new Error(`${b[1]} isn't an outcome`);
  return { phase: phase(b[0]), outcome: b[1] === 0 ? null : (b[1] as Outcome) };
}

/** `bet`'s and `relay_bet`'s return: the hand's id. */
export function decodeGameId(hex: string): bigint {
  return new Reader(hexBytes(hex, 8), "a hand's id").u64();
}

// ---------------------------------------------------------------------------------- aborts

/** The program's abort codes, in plain words for a player. The chain's own message is data and isn't shown as ours. */
export const ABORTS: Readonly<Record<number, string>> = Object.freeze({
  1: "The call's input isn't what this method takes.",
  2: "This method takes no FANG.",
  40: "The table is already set up.",
  41: "The table isn't set up yet.",
  42: "Only the table's owner can do that.",
  43: "Only the dealer can do that.",
  44: "Send some FANG with it.",
  45: "Those limits don't fit together.",
  46: "New bets are paused. Open hands play on, and withdrawals work.",
  47: "That's below the minimum bet.",
  48: "That's above the maximum bet right now.",
  49: "The house's bankroll can't cover that bet right now.",
  50: "That would pass your daily limit at this table.",
  51: "The table is full. Try again when a hand finishes.",
  52: "Finish your open hand first.",
  53: "The dealer isn't ready for a new hand yet. Try again in a moment.",
  54: "There's no such open hand.",
  55: "That hand is someone else's.",
  56: "It isn't your move.",
  57: "The dealer owes no card on that hand.",
  58: "That isn't the next link of the hand's chain.",
  59: "That's more links than the hand takes.",
  60: "The deadline hasn't passed yet.",
  61: "That isn't the next batch of commitments.",
  62: "That would pass the table balance limit.",
  63: "There isn't that much to take.",
  64: "That account isn't this player's relayer.",
  65: "The relayer's grant has expired.",
  66: "That's past what the grant allows.",
  67: "That grant doesn't fit the table's limits.",
  68: "That hand wasn't bet from a table balance.",
  69: "The house has too much at stake on open hands right now.",
  70: "That hand isn't waiting for a seed.",
  71: "That isn't the seed the bet was placed with.",
  72: "The time to reveal that bet's seed has passed.",
});

/** What an abort code means, in the table's words. */
export function abortWords(code: number | undefined): string {
  return (code !== undefined && ABORTS[code]) || "The table refused that.";
}

/** Drops as FANG text without the unit: "1.5". */
export function fang(drops: bigint): string {
  const whole = drops / DROPS_PER_FANG;
  const frac = (drops % DROPS_PER_FANG).toString().padStart(9, "0").replace(/0+$/, "");
  return frac ? `${whole}.${frac}` : `${whole}`;
}
