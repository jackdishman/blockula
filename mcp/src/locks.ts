// One tx at a time per account (AGENT-10). A tx's nonce is the node's `next_nonce` when it's built,
// so two built before either reaches the node get the same nonce, and the node refuses one ("bad
// nonce"). That happened with parallel tool calls in one session and with two sessions at once.
//
// - AccountLocks serializes building, signing and submitting per account: in this process with a
//   promise chain, and across processes sharing BLOCKULA_HOME with a lock file (spend.ts's
//   pattern, made asynchronous: a request to the node can take seconds). Waiting for finality
//   happens outside the lock, so parallel sends still overlap there.
// - NonceTracker remembers the last nonce this MCP signed per (chain, account) while its tx is
//   pending (submitted, not yet final). The next tx takes max(node next_nonce, last + 1), so a node
//   whose view lags the mempool that took the last one can't hand out its nonce again. It only
//   informs: an unreadable file means the node's next_nonce, as before.
import { createHash } from "node:crypto";
import { mkdirSync, openSync, closeSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Whether a process with this pid is running (on this machine). */
function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return (e as NodeJS.ErrnoException).code === "EPERM";
  }
}

export interface LockOptions {
  /** A lock file older than this is a stuck holder's, and is taken over. */
  staleMs: number;
  /** How long to wait for the lock before giving up. */
  waitMs: number;
  pollMs: number;
}

export class AccountLocks {
  private readonly queues = new Map<string, Promise<void>>();

  constructor(readonly dir: string, readonly opts: LockOptions = { staleMs: 120_000, waitMs: 120_000, pollMs: 25 }) {}

  /** The lock file for a key (`<chain_id> <genesis_hash> <address>`). */
  file(key: string) {
    return join(this.dir, `agent-nonce-${keyHash(key)}.lock`);
  }

  /** Runs `f` holding `key`'s lock: no other call in this process, or other process sharing the directory, holds it meanwhile. */
  async run<T>(key: string, f: () => Promise<T>): Promise<T> {
    const before = this.queues.get(key) ?? Promise.resolve();
    let done!: () => void;
    const mine = new Promise<void>((r) => (done = r));
    const tail = before.then(() => mine);
    this.queues.set(key, tail);
    await before;
    try {
      return await this.holdingFile(key, f);
    } finally {
      done();
      if (this.queues.get(key) === tail) this.queues.delete(key);
    }
  }

  private async holdingFile<T>(key: string, f: () => Promise<T>): Promise<T> {
    const lock = this.file(key);
    mkdirSync(dirname(lock), { recursive: true });
    const token = `${process.pid} ${Date.now()} ${Math.random().toString(36).slice(2)}`;
    const start = Date.now();
    for (;;) {
      try {
        const fd = openSync(lock, "wx", 0o600);
        try {
          writeFileSync(fd, token);
        } finally {
          closeSync(fd);
        }
        break;
      } catch (e) {
        if ((e as NodeJS.ErrnoException).code !== "EEXIST") throw e;
      }
      // Held: by a process that's gone, or for too long, it's taken over.
      try {
        const holder = Number(readFileSync(lock, "utf8").split(" ")[0]);
        const stale = Date.now() - statSync(lock).mtimeMs > this.opts.staleMs;
        if (stale || (Number.isSafeInteger(holder) && holder > 0 && holder !== process.pid && !alive(holder))) rmSync(lock, { force: true });
      } catch {
        /* it went away, or is being written: try again */
      }
      if (Date.now() - start > this.opts.waitMs) throw new Error(`another Blockula MCP has been sending from this account for over ${Math.round(this.opts.waitMs / 1000)} s (${lock}); if none is running, delete that file`);
      await sleep(this.opts.pollMs);
    }
    try {
      return await f();
    } finally {
      try {
        if (readFileSync(lock, "utf8") === token) rmSync(lock, { force: true });
      } catch {
        /* taken over as stale, or removed */
      }
    }
  }
}

/** What this MCP knows about one (chain, account)'s nonces. */
interface Nonces {
  key: string;
  /** The highest nonce of a tx this MCP signed that ran in a block (finalized or failed): spent for good. */
  ran?: number;
  /** The last tx this MCP signed that's still pending (submitted, not final). */
  pending?: { nonce: number; valid_until: number };
}

const keyHash = (key: string) => createHash("sha256").update(key).digest("hex").slice(0, 24);

/**
 * One small file per (chain, account), beside its lock: every write to it happens while holding
 * that account's AccountLocks lock, so two processes never write it at once.
 *
 * A node's next_nonce can lag what this MCP knows: its mempool may not show a tx another node
 * took, and right after a block runs its account read can still be the one before (observed on a
 * dev chain: "bad nonce: expected 5, got 4" for the tx after a finalized one). Nonces only go up,
 * so a tx that ran keeps its nonce spent for good, and a pending one holds its nonce until its
 * valid_until. One that was dropped or refused holds nothing: its nonce may be free again.
 */
export class NonceTracker {
  constructor(readonly dir: string) {}

  file(key: string) {
    return join(this.dir, `agent-nonce-${keyHash(key)}.json`);
  }

  private read(key: string): Nonces {
    try {
      const n = JSON.parse(readFileSync(this.file(key), "utf8")) as Nonces;
      if (!n || n.key !== key) return { key };
      const ran = Number.isSafeInteger(n.ran) && (n.ran as number) >= 0 ? n.ran : undefined;
      const p = n.pending;
      const pending = p && Number.isSafeInteger(p.nonce) && p.nonce >= 0 && Number.isSafeInteger(p.valid_until) ? { nonce: p.nonce, valid_until: p.valid_until } : undefined;
      return { key, ...(ran !== undefined ? { ran } : {}), ...(pending ? { pending } : {}) };
    } catch {
      return { key };
    }
  }

  private write(n: Nonces) {
    try {
      mkdirSync(this.dir, { recursive: true });
      const tmp = `${this.file(n.key)}.${process.pid}.tmp`;
      writeFileSync(tmp, JSON.stringify(n), { mode: 0o600 });
      renameSync(tmp, this.file(n.key));
    } catch {
      /* informational only */
    }
  }

  /** The lowest nonce the next tx may take: past the last one that ran, and past a pending one while it can still run (the chain at or before its valid_until). */
  floor(key: string, height: number): number | undefined {
    const n = this.read(key);
    const after = [n.ran !== undefined ? n.ran + 1 : undefined, n.pending && height <= n.pending.valid_until ? n.pending.nonce + 1 : undefined].filter((x): x is number => x !== undefined);
    return after.length ? Math.max(...after) : undefined;
  }

  /** A tx was submitted and isn't final yet. Call holding the account's lock. */
  pending(key: string, nonce: number, validUntil: number) {
    const n = this.read(key);
    this.write({ ...n, pending: { nonce, valid_until: validUntil } });
  }

  /** The tx with this nonce ran in a block (finalized, or failed: its fee was charged). Call holding the account's lock. */
  ran(key: string, nonce: number) {
    const n = this.read(key);
    const pending = n.pending && n.pending.nonce > nonce ? n.pending : undefined;
    this.write({ key, ran: Math.max(n.ran ?? -1, nonce), ...(pending ? { pending } : {}) });
  }

  /**
   * Forgets what's known: after the node refused a tx for a nonce past its own, so a wrong record
   * (a node that said a tx ran when it didn't) can't block every later tx. Call holding the lock.
   */
  forget(key: string) {
    rmSync(this.file(key), { force: true });
  }

  /** The tx with this nonce was dropped or refused: it holds its nonce no longer. Call holding the account's lock. */
  settled(key: string, nonce: number) {
    const n = this.read(key);
    if (n.pending?.nonce !== nonce) return;
    this.write({ key, ...(n.ran !== undefined ? { ran: n.ran } : {}) });
  }
}
