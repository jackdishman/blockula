// The table's records as the program stores them, for tests that need a table's storage made
// from values (the package itself only ever decodes them). Layouts: node/vm/guests/blackjack's
// doc comment; `test/codec.ts` holds the decoders to bytes written out by hand.
import { GAME_BYTES, HOUSE_BYTES, LIMITS_BYTES, MAX_OPEN_GAMES, Phase, addressBytes, hexBytes, storageKey, toHex, type Game, type House, type Limits } from "../src/index.js";

class Writer {
  private readonly parts: Uint8Array[] = [];
  u8(n: number): this {
    this.parts.push(Uint8Array.of(n));
    return this;
  }
  u64(n: bigint): this {
    const b = new Uint8Array(8);
    new DataView(b.buffer).setBigUint64(0, n, true);
    this.parts.push(b);
    return this;
  }
  raw(b: Uint8Array): this {
    this.parts.push(b);
    return this;
  }
  hex(bytes: number): string {
    const out = toHex(Uint8Array.from(this.parts.flatMap((p) => [...p])));
    if (out.length !== 2 * bytes) throw new Error(`encoded ${out.length / 2} bytes, the record is ${bytes}`);
    return out;
  }
}

export function encodeHouse(h: House): string {
  return new Writer().raw(addressBytes(h.owner)).raw(addressBytes(h.dealer)).u64(h.bankroll).u64(h.reserved).u64(h.stakes).u64(h.table).u64(h.nextCommit).u64(h.nextGame).u8(h.paused ? 1 : 0).u8(h.open).hex(HOUSE_BYTES);
}

/** The limits, in the record's order (the order of `Limits`' keys). */
export function encodeLimits(l: Limits): string {
  const w = new Writer();
  for (const v of Object.values(l)) w.u64(v as bigint);
  return w.hex(LIMITS_BYTES);
}

export function encodeGame(g: Game): string {
  const padded = (cards: readonly number[], n: number) => Uint8Array.from({ length: n }, (_, i) => cards[i] ?? 0);
  // One field holds the seed's hash until the seed is revealed, and the seed after.
  const seedField = g.phase === Phase.Seed ? g.seedHash : g.clientSeed;
  if (seedField === null) throw new Error("a hand's record always holds a seed or its hash");
  return new Writer()
    .raw(addressBytes(g.player))
    .u64(g.stake)
    .raw(hexBytes(seedField, 32))
    .raw(hexBytes(g.link, 32))
    .u64(g.deadline)
    .u64(g.postId)
    .u8(g.phase)
    .u8(g.table ? 1 : 0)
    .u8(g.dealt)
    .u8(g.playerCards.length)
    .u8(g.dealerCards.length)
    .raw(padded(g.playerCards, 20))
    .raw(padded(g.dealerCards, 12))
    .hex(GAME_BYTES);
}

export function encodeOpenList(ids: readonly bigint[]): string {
  const w = new Writer();
  for (let i = 0; i < MAX_OPEN_GAMES; i++) w.u64(ids[i] ?? 0n);
  return w.hex(8 * MAX_OPEN_GAMES);
}

/** A table's whole storage, `key hex → value hex`, as the program would hold it. */
export function tableStorage(t: { house: House; limits: Limits; open: readonly bigint[]; games: readonly Game[]; commitments: ReadonlyMap<bigint, string> }): Map<string, string> {
  const out = new Map<string, string>();
  out.set(storageKey.house, encodeHouse(t.house));
  out.set(storageKey.limits, encodeLimits(t.limits));
  // The program keeps no list while no hand is open.
  if (t.open.length) out.set(storageKey.open, encodeOpenList(t.open));
  for (const g of t.games) out.set(storageKey.game(g.id), encodeGame(g));
  for (const [k, c] of t.commitments) out.set(storageKey.commitment(k), c);
  return out;
}
