// The agent's daily spending cap (BLOCKULA_MAX_DAILY), as a ledger in BLOCKULA_HOME that holds
// across calls, restarts and concurrent servers.
//
// A spend is reserved *before* its tx is sent: the amount, anything else it costs (a name's
// price, a claim bond, an account-opening fee) and the tx's max fee. The only thing ever given
// back is the unused part of the max fee once the tx is final. A tx that is pending, timed out,
// unknown, dropped, failed or even refused stays counted: it may still execute (or did, and paid
// its fee), and a retry must not find a fresh budget (audit F68, F72). A refusal is the node's
// word, and a node can answer "refused" while it relays the signed tx anyway.
//
// What does come back is double counting. Each reservation is filed under its tx's (chain,
// account, nonce) slot, and at most one tx per slot can ever execute. So when this MCP signs
// another tx with a nonce it already used today (which is what happens after a real refusal: the
// node's next nonce didn't move), the slot counts the larger of the two, not their sum.
//
// Each chain has its own daily total (AGENT-13): a dev chain and a chain someone launched share
// the `boo` prefix, so the same key has the same address on both, and spending on one mustn't use
// up the other's budget. The file stays readable by 0.5.1, which knew one total: `spent` is still
// the sum over every chain, and `chains` holds each chain's part. What `spent` holds beyond the
// chains' parts (a 0.5.1 file, or one 0.5.1 wrote back without `chains`) can't be placed on a
// chain, so it counts toward every chain's total: migrating never under-counts today's spend.
import { closeSync, mkdirSync, openSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { formatFang } from "@blockula/sdk";

/** A tx's nonce slot: at most one tx with this account and nonce can execute (on one chain). */
export interface NonceSlot {
  account: string;
  nonce: number;
  /** The chain, as `chainKey` writes it: the slot and its spend count toward that chain's day. */
  chain?: string;
}

/** A chain as the ledger keys it: `<chain_id> <genesis_hash>`. */
export const chainKey = (chainId: string, genesisHash: string) => {
  if (!/^[A-Za-z0-9._-]{1,64}$/.test(chainId) || !/^[0-9a-f]{64}$/.test(genesisHash)) throw new Error("a chain is its id and its 64-hex genesis hash");
  return `${chainId} ${genesisHash}`;
};

/** A reserved part of one UTC day's budget. */
export interface Reservation {
  day: string;
  /** What this reservation added to the day's total (less than asked when its slot already held some). */
  drops: bigint;
  /** Its slot's key, when it has one. */
  slot?: string;
  /** The chain whose total it added to, when it was reserved for one. */
  chain?: string;
}

interface Day {
  day: string;
  /** Every chain's total, and what can't be placed on a chain (a 0.5.1 ledger's). */
  spent: bigint;
  /** Per (chain, account, nonce) slot: the most any tx signed for it today can cost. */
  slots: Record<string, bigint>;
  /** Each chain's part of `spent`. */
  chains: Record<string, bigint>;
}

/** A lock older than this is a crashed server's: it's taken over. */
const STALE_LOCK_MS = 10_000;
const sleep = new Int32Array(new SharedArrayBuffer(4));

const slotKey = (s: NonceSlot) => {
  if (!s.account || !Number.isSafeInteger(s.nonce) || s.nonce < 0) throw new Error("a nonce slot needs an account and a whole-number nonce");
  return s.chain ? `${s.chain} ${s.account}#${s.nonce}` : `${s.account}#${s.nonce}`;
};

/** What `spent` holds that no chain's part accounts for: it counts toward every chain. */
const unplaced = (d: Day) => {
  const placed = Object.values(d.chains).reduce((a, b) => a + b, 0n);
  return d.spent > placed ? d.spent - placed : 0n;
};

/** One chain's total today: its own part and everything that can't be placed. Without a chain, the whole day's. */
const totalFor = (d: Day, chain?: string) => (chain === undefined ? d.spent : (d.chains[chain] ?? 0n) + unplaced(d));

export class SpendLedger {
  /** `limitText` names the limit in refusals (e.g. "BLOCKULA_MAX_DAILY=100 FANG"). */
  constructor(readonly file: string, readonly maxDaily: bigint, readonly limitText: string, private readonly now: () => Date = () => new Date()) {}

  private today() {
    return this.now().toISOString().slice(0, 10);
  }

  /**
   * Today's total (UTC), reservations included: one chain's (with what can't be placed on any),
   * or without `chain`, every chain's. A ledger that can't be read refuses spending instead of
   * starting from 0.
   */
  spent(chain?: string): bigint {
    return totalFor(this.read(), chain);
  }

  /** What today's ledger holds for one nonce slot (0 if nothing). */
  held(slot: NonceSlot): bigint {
    return this.read().slots[slotKey(slot)] ?? 0n;
  }

  private read(): Day {
    const day = this.today();
    let raw: string;
    try {
      raw = readFileSync(this.file, "utf8");
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code === "ENOENT") return { day, spent: 0n, slots: {}, chains: {} };
      throw e;
    }
    let f: { day?: unknown; spent?: unknown; slots?: unknown; chains?: unknown };
    try {
      f = JSON.parse(raw);
    } catch {
      throw new Error(`the spending ledger ${this.file} is unreadable, so the daily limit can't be checked; fix or delete it`);
    }
    if (f.day !== day) return { day, spent: 0n, slots: {}, chains: {} };
    const malformed = () => new Error(`the spending ledger ${this.file} is malformed, so the daily limit can't be checked; fix or delete it`);
    if (typeof f.spent !== "string" || !/^\d+$/.test(f.spent)) throw malformed();
    const amounts = (v: unknown) => {
      const out: Record<string, bigint> = {};
      if (v === undefined) return out;
      if (typeof v !== "object" || v === null || Array.isArray(v)) throw malformed();
      for (const [k, x] of Object.entries(v)) {
        if (typeof x !== "string" || !/^\d+$/.test(x)) throw malformed();
        out[k] = BigInt(x);
      }
      return out;
    };
    return { day, spent: BigInt(f.spent), slots: amounts(f.slots), chains: amounts(f.chains) };
  }

  /** Written whole (temp file, then rename), so a crash never leaves half a ledger. */
  private write(d: Day) {
    mkdirSync(dirname(this.file), { recursive: true });
    const tmp = `${this.file}.${process.pid}.tmp`;
    const strings = (r: Record<string, bigint>) => Object.fromEntries(Object.entries(r).map(([k, v]) => [k, v.toString()]));
    writeFileSync(tmp, JSON.stringify({ day: d.day, spent: d.spent.toString(), slots: strings(d.slots), chains: strings(d.chains) }), { mode: 0o600 });
    renameSync(tmp, this.file);
  }

  /** Runs `f` holding the ledger's lock file, so two servers sharing BLOCKULA_HOME can't both spend the same budget. */
  private locked<T>(f: () => T): T {
    mkdirSync(dirname(this.file), { recursive: true });
    const lock = `${this.file}.lock`;
    for (let i = 0; ; i++) {
      try {
        closeSync(openSync(lock, "wx", 0o600));
        break;
      } catch (e) {
        if ((e as NodeJS.ErrnoException).code !== "EEXIST") throw e;
        try {
          if (Date.now() - statSync(lock).mtimeMs > STALE_LOCK_MS) rmSync(lock, { force: true });
        } catch {
          /* it went away */
        }
        if (i >= 500) throw new Error(`the spending ledger is locked (${lock}); if no other Blockula MCP is running, delete that file`);
        Atomics.wait(sleep, 0, 0, 10);
      }
    }
    try {
      return f();
    } finally {
      rmSync(lock, { force: true });
    }
  }

  /**
   * Reserves what a tx can cost against today's budget (its chain's: `slot.chain`), or throws (and
   * reserves nothing) if it doesn't fit. With its nonce `slot`, only what exceeds what the slot
   * already holds today is added: at most one tx per slot executes.
   */
  reserve(drops: bigint, slot?: NonceSlot): Reservation {
    if (drops < 0n) throw new Error("can't reserve a negative amount");
    const key = slot ? slotKey(slot) : undefined;
    const chain = slot?.chain;
    return this.locked(() => {
      const d = this.read();
      const held = key ? (d.slots[key] ?? 0n) : 0n;
      const extra = drops > held ? drops - held : 0n;
      const total = totalFor(d, chain);
      if (total + extra > this.maxDaily) {
        throw new Error(`this would exceed the agent's daily limit (${this.limitText}; ${formatFang(total)} already spent or in flight today, and this needs ${formatFang(extra)} more including fees)`);
      }
      d.spent += extra;
      if (chain !== undefined) d.chains[chain] = (d.chains[chain] ?? 0n) + extra;
      if (key) d.slots[key] = held > drops ? held : drops;
      this.write(d);
      return { day: d.day, drops: extra, slot: key, ...(chain !== undefined ? { chain } : {}) };
    });
  }

  /**
   * Gives back `drops` of a reservation (the unused part of a final tx's max fee), never more
   * than the reservation added. A reservation from an earlier day has nothing left to give back.
   */
  release(r: Reservation, drops: bigint) {
    const give = drops < r.drops ? drops : r.drops;
    if (give <= 0n) return;
    r.drops -= give;
    this.locked(() => {
      const d = this.read();
      if (d.day !== r.day) return;
      d.spent = d.spent > give ? d.spent - give : 0n;
      if (r.chain !== undefined && d.chains[r.chain] !== undefined) d.chains[r.chain] = d.chains[r.chain] > give ? d.chains[r.chain] - give : 0n;
      if (r.slot && d.slots[r.slot] !== undefined) d.slots[r.slot] = d.slots[r.slot] > give ? d.slots[r.slot] - give : 0n;
      this.write(d);
    });
  }
}
