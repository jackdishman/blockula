// The agent's daily spending cap (BLOCKULA_MAX_DAILY), as a ledger in BLOCKULA_HOME that holds
// across calls, restarts and concurrent servers.
//
// A spend is reserved *before* its tx is sent: the amount, anything else it costs (a name's
// price, a claim bond, an account-opening fee) and the tx's max fee. It is only given back when
// the node refused the tx (so it never entered a mempool), and the unused part of the max fee once
// the tx is final. A tx that is pending, timed out, unknown or failed stays counted: it may still
// execute (or did, and paid its fee), and a retry must not find a fresh budget (audit F68, F72).
import { closeSync, mkdirSync, openSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { formatFang } from "@blockula/sdk";

/** A reserved part of one UTC day's budget. */
export interface Reservation {
  day: string;
  drops: bigint;
}

/** A lock older than this is a crashed server's: it's taken over. */
const STALE_LOCK_MS = 10_000;
const sleep = new Int32Array(new SharedArrayBuffer(4));

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

  private read(): { day: string; spent: bigint } {
    const day = this.today();
    let raw: string;
    try {
      raw = readFileSync(this.file, "utf8");
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code === "ENOENT") return { day, spent: 0n };
      throw e;
    }
    let f: { day?: unknown; spent?: unknown };
    try {
      f = JSON.parse(raw);
    } catch {
      throw new Error(`the spending ledger ${this.file} is unreadable, so the daily limit can't be checked; fix or delete it`);
    }
    if (f.day !== day) return { day, spent: 0n };
    if (typeof f.spent !== "string" || !/^\d+$/.test(f.spent)) throw new Error(`the spending ledger ${this.file} is malformed, so the daily limit can't be checked; fix or delete it`);
    return { day, spent: BigInt(f.spent) };
  }

  /** Written whole (temp file, then rename), so a crash never leaves half a ledger. */
  private write(day: string, spent: bigint) {
    mkdirSync(dirname(this.file), { recursive: true });
    const tmp = `${this.file}.${process.pid}.tmp`;
    writeFileSync(tmp, JSON.stringify({ day, spent: spent.toString() }), { mode: 0o600 });
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

  /** Reserves `drops` of today's budget, or throws (and reserves nothing) if it doesn't fit. */
  reserve(drops: bigint): Reservation {
    if (drops < 0n) throw new Error("can't reserve a negative amount");
    return this.locked(() => {
      const { day, spent } = this.read();
      if (spent + drops > this.maxDaily) {
        throw new Error(`this would exceed the agent's daily limit (${this.limitText}; ${formatFang(spent)} already spent or in flight today, and this needs ${formatFang(drops)} including fees)`);
      }
      this.write(day, spent + drops);
      return { day, drops };
    });
  }

  /** Gives back `drops` of a reservation (a refused tx, the unused part of a fee). A reservation from an earlier day has nothing left to give back. */
  release(r: Reservation, drops: bigint) {
    const give = drops < r.drops ? drops : r.drops;
    if (give <= 0n) return;
    r.drops -= give;
    this.locked(() => {
      const { day, spent } = this.read();
      if (day !== r.day) return;
      this.write(day, spent > give ? spent - give : 0n);
    });
  }
}
