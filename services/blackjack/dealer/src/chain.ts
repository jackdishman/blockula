/**
 * The dealer's sender on a real chain: the SDK's `Wallet`, which signs one transaction at a time
 * in nonce order, and its errors sorted into what the dealer acts on. The table reads are
 * `Table` from `@blockula/blackjack` as it is.
 */
import { eventsOf, type TableCall, type TableEvent } from "@blockula/blackjack";
import { ApiError, ProgramFailedError, TxFailedError, TxPendingError, checkReceipt, txDropped, type Client, type ProgramReceipt, type Wallet } from "@blockula/sdk";
import type { SendOptions, Sent, TxSender } from "./dealer.js";
import { errorText } from "./log.js";

export class WalletSender implements TxSender {
  constructor(
    private readonly wallet: Wallet,
    private readonly client: Client,
    private readonly program: string,
  ) {}

  get address(): string {
    return this.wallet.address;
  }

  /** The table's own events in a receipt; none if they don't decode (the chain's state is what's acted on). */
  private events(receipt: Pick<ProgramReceipt, "claims">): TableEvent[] {
    try {
      return eventsOf(receipt, this.program);
    } catch {
      return [];
    }
  }

  async send(c: TableCall, opts: SendOptions = {}): Promise<Sent> {
    // Set once the transaction is signed, just before it's submitted: from then on it may run.
    let signed: { id: string; validUntil: number } | undefined;
    try {
      const r = await this.wallet.call(this.program, c.method, c.input, {
        value: c.value ?? 0n,
        storageLimit: c.storageLimit ?? 0n,
        validForBlocks: opts.validForBlocks,
        fuelLimit: opts.fuelLimit,
        onSigned: (tx, id) => {
          signed = { id, validUntil: tx.valid_until };
        },
      });
      return { status: "final", id: String(r.tx.id), height: Number(r.tx.height), events: this.events(r.receipt), fuel: r.receipt.facts.fuel.used };
    } catch (e) {
      const s = signed as { id: string; validUntil: number } | undefined;
      if (e instanceof ProgramFailedError) return { status: "aborted", id: e.id, height: typeof e.tx?.height === "number" ? e.tx.height : undefined, code: e.receipt.outcome.abort_code, trap: e.receipt.outcome.trap?.name };
      if (e instanceof TxFailedError) {
        // Dropped by this node without running: another node may still include it.
        if (e.dropped) return { status: "unknown", id: e.id, held: false, validUntil: e.validUntil ?? s?.validUntil };
        return { status: "aborted", id: e.id, height: typeof e.tx?.height === "number" ? e.tx.height : undefined, code: undefined };
      }
      if (e instanceof TxPendingError) return { status: "unknown", id: e.id, held: true, validUntil: s?.validUntil };
      if (!s) return { status: "unsent", reason: errorText(e) };
      // Signed, and the node refused it outright: it was never admitted.
      if (e instanceof ApiError && e.status >= 400 && e.status < 500 && e.status !== 408) return { status: "unsent", reason: errorText(e) };
      // Signed and perhaps submitted (a timeout, a broken connection, an answer that didn't check).
      return { status: "unknown", id: s.id, held: false, validUntil: s.validUntil };
    }
  }

  async check(id: string): Promise<Sent> {
    let tx: any;
    try {
      tx = await this.client.tx(id);
    } catch (e) {
      // The node doesn't have it (yet, or ever).
      if (e instanceof ApiError && e.status === 404) return { status: "unknown", id, held: false };
      throw e;
    }
    const receipt = (): ProgramReceipt | null => {
      try {
        return checkReceipt(this.client.rpc, tx);
      } catch {
        return null;
      }
    };
    if (tx?.status === "finalized") {
      const r = receipt();
      return { status: "final", id, height: Number(tx.height), events: r ? this.events(r) : [], fuel: r?.facts.fuel.used };
    }
    if (tx?.status === "failed") {
      if (txDropped(tx)) return { status: "unknown", id, held: false };
      const outcome = receipt()?.outcome;
      return { status: "aborted", id, height: typeof tx.height === "number" ? tx.height : undefined, code: outcome?.abort_code, trap: outcome?.trap?.name };
    }
    return { status: "unknown", id, held: tx?.status === "pending" };
  }

  async balance(): Promise<bigint> {
    return BigInt((await this.client.account(this.wallet.address)).balance);
  }
}
