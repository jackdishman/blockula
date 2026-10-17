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
// What does come back is double counting. Each reservation is filed under its tx's (account,
// nonce) slot, and at most one tx per slot can ever execute. So when this MCP signs another tx with
// a nonce it already used today (which is what happens after a real refusal: the node's next nonce
// didn't move), the slot counts the larger of the two, not their sum.
import { closeSync, mkdirSync, openSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { formatFang } from "@blockula/sdk";

/** A tx's nonce slot: at most one tx with this account and nonce can execute. */
export interface NonceSlot {
  account: string;
  nonce: number;
}

/** A reserved part of one UTC day's budget. */
export interface Reservation {
  day: string;
  /** What this reservation added to the day's total (less than asked when its slot already held some). */
  drops: bigint;
  /** Its slot's key, when it has one. */
  slot?: string;
}

interface Day {
  day: string;
  spent: bigint;
  /** Per (account, nonce) slot: the most any tx signed for it today can cost. */
  slots: Record<string, bigint>;
}

/** A lock older than this is a crashed server's: it's taken over. */
const STALE_LOCK_MS = 10_000;
const sleep = new Int32Array(new SharedArrayBuffer(4));

const slotKey = (s: NonceSlot) => {
  if (!s.account || !Number.isSafeInteger(s.nonce) || s.nonce < 0) throw new Error("a nonce slot needs an account and a whole-number nonce");
  return `${s.account}#${s.nonce}`;
};

export class SpendLedger {
  /** `limitText` names the limit in refusals (e.g. "BLOCKULA_MAX_DAILY=100 FANG"). */
  constructor(readonly file: string, readonly maxDaily: bigint, readonly limitText: string, private readonly now: () => Date = () => new Date()) {}

  private today() {
    return this.now().toISOString().slice(0, 10);
  }

  /** Today's total (UTC), reservations included. A ledger that can't be read refuses spending instead of starting from 0. */
  spent(): bigint {
    return this.read().spent;
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
      if ((e as NodeJS.ErrnoException).code === "ENOENT") return { day, spent: 0n, slots: {} };
      throw e;
    }
    let f: { day?: unknown; spent?: unknown; slots?: unknown };
    try {
      f = JSON.parse(raw);
    } catch {
      throw new Error(`the spending ledger ${this.file} is unreadable, so the daily limit can't be checked; fix or delete it`);
    }
    if (f.day !== day) return { day, spent: 0n, slots: {} };
    const malformed = () => new Error(`the spending ledger ${this.file} is malformed, so the daily limit can't be checked; fix or delete it`);
    if (typeof f.spent !== "string" || !/^\d+$/.test(f.spent)) throw malformed();
    const slots: Record<string, bigint> = {};
    if (f.slots !== undefined) {
      if (typeof f.slots !== "object" || f.slots === null || Array.isArray(f.slots)) throw malformed();
      for (const [k, v] of Object.entries(f.slots)) {
        if (typeof v !== "string" || !/^\d+$/.test(v)) throw malformed();
        slots[k] = BigInt(v);
      }
    }
    return { day, spent: BigInt(f.spent), slots };
  }

  /** Written whole (temp file, then rename), so a crash never leaves half a ledger. */
  private write(d: Day) {
    mkdirSync(dirname(this.file), { recursive: true });
    const tmp = `${this.file}.${process.pid}.tmp`;
    const slots = Object.fromEntries(Object.entries(d.slots).map(([k, v]) => [k, v.toString()]));
    writeFileSync(tmp, JSON.stringify({ day: d.day, spent: d.spent.toString(), slots }), { mode: 0o600 });
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
   * Reserves what a tx can cost against today's budget, or throws (and reserves nothing) if it
   * doesn't fit. With its nonce `slot`, only what exceeds what the slot already holds today is
   * added: at most one tx per slot executes.
   */
  reserve(drops: bigint, slot?: NonceSlot): Reservation {
    if (drops < 0n) throw new Error("can't reserve a negative amount");
    const key = slot ? slotKey(slot) : undefined;
    return this.locked(() => {
      const d = this.read();
      const held = key ? (d.slots[key] ?? 0n) : 0n;
      const extra = drops > held ? drops - held : 0n;
      if (d.spent + extra > this.maxDaily) {
        throw new Error(`this would exceed the agent's daily limit (${this.limitText}; ${formatFang(d.spent)} already spent or in flight today, and this needs ${formatFang(extra)} more including fees)`);
      }
      d.spent += extra;
      if (key) d.slots[key] = held > drops ? held : drops;
      this.write(d);
      return { day: d.day, drops: extra, slot: key };
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
      if (r.slot && d.slots[r.slot] !== undefined) d.slots[r.slot] = d.slots[r.slot] > give ? d.slots[r.slot] - give : 0n;
      this.write(d);
    });
  }
}
