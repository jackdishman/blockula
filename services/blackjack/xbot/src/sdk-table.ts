/**
 * The table on a real chain, through the SDK: reads from the node's storage route, and the
 * relayed calls signed with the relayer's key. This build signs only on a local chain: the
 * node must be on this machine (`localRpc`), and a node on this machine that follows a shared
 * chain is refused too (`sharedChain`).
 */
import { ApiError, Client, NETWORKS, ProgramFailedError, Wallet, checkReceipt } from "@blockula/sdk";
import { Table, apiGet, bytesAddress, call, decodePlayer, eventsOf, hexBytes, send, type ApiGet, type Game, type House, type Limits, type PlayerRecord, type TableCall } from "@blockula/blackjack";
import { TableRefused, type Landed, type LinkClaim, type OnSigned, type TableDriver, type TxState } from "./types.js";

/** `rpc` if it is plain http to this machine; throws otherwise. This build talks to no other node. */
export function localRpc(rpc: string): string {
  let u: URL;
  try {
    u = new URL(rpc);
  } catch {
    throw new Error("the RPC isn't a URL");
  }
  const host = u.hostname.replace(/^\[|\]$/g, "");
  if (u.protocol !== "http:" || !(host === "127.0.0.1" || host === "localhost" || host === "::1") || u.username !== "" || u.password !== "") throw new Error("this build plays on a local chain only: the RPC must be http://127.0.0.1:<port> or http://localhost:<port>");
  return `${u.protocol}//${u.host}`;
}

/**
 * Whether a node's chain is one other people share: the chains the SDK knows by name, and
 * blockula-1. A node on this machine can still be a full node of one of them; this build
 * signs nothing there.
 */
export function sharedChain(chainId: string, genesisHash: string): boolean {
  const known = Object.values(NETWORKS).filter((n) => n.chainId !== undefined);
  return chainId === "blockula-1" || known.some((n) => n.chainId === chainId || n.genesisHash === genesisHash);
}

/** The chain and table a driver plays on: a store is kept for one of them only. */
export interface ChainPin {
  chainId: string;
  genesisHash: string;
  table: string;
}

/** The first byte of a player record's storage key: `b"p"`. */
const PLAYER_PREFIX = "70";

export interface SdkTableOptions {
  rpc: string;
  /** The table's address. */
  program: string;
  /** The relayer key's seed (hex), from its key file. Never logged. */
  seed: string;
  /** Blocks a relayed transaction stays valid: after them an unseen one is known never to have run. */
  validBlocks?: number;
}

export class SdkTable implements TableDriver {
  readonly relayer: string;

  private constructor(
    readonly rpc: string,
    readonly program: string,
    private readonly client: Client,
    private readonly wallet: Wallet,
    private readonly table: Table,
    private readonly api: ApiGet,
    readonly chain: ChainPin,
  ) {
    this.relayer = wallet.address;
  }

  static async open(o: SdkTableOptions): Promise<SdkTable> {
    const rpc = localRpc(o.rpc);
    const first = new Client(rpc, { network: "local" });
    const status = await first.status();
    const [chainId, genesisHash] = [String(status.chain_id), String(status.genesis_hash)];
    if (sharedChain(chainId, genesisHash)) throw new Error(`this build plays on a local chain only: the node at ${rpc} is on ${chainId}, a chain other people share`);
    // Pinned from here on: every status this client reads, and every signature, is for this chain.
    const client = new Client(rpc, { chainId, genesisHash });
    await client.addressPrefix();
    const wallet = Wallet.fromSeed(o.seed, client);
    wallet.validForBlocks = o.validBlocks ?? 30;
    const table = Table.at(rpc, o.program);
    // Fails here, before any post is read, if the address isn't a table that's set up.
    await table.house();
    return new SdkTable(rpc, o.program, client, wallet, table, apiGet(rpc), { chainId, genesisHash, table: o.program });
  }

  async height(): Promise<bigint> {
    return BigInt((await this.client.status()).height as number);
  }

  house(): Promise<House> {
    return this.table.house();
  }

  limits(): Promise<Limits> {
    return this.table.limits();
  }

  player(account: string): Promise<PlayerRecord> {
    return this.table.player(account);
  }

  game(id: bigint): Promise<Game | null> {
    return this.table.game(id);
  }

  /** Every player record of the table, a page at a time: the accounts whose grant names this relayer. */
  async granted(): Promise<string[]> {
    const out: string[] = [];
    let after = PLAYER_PREFIX;
    for (;;) {
      const page = (await this.api(`/program/${this.program}/storage?limit=100&after=${after}`)) as { entries?: { key?: unknown; value?: unknown }[]; next?: unknown };
      const entries = Array.isArray(page?.entries) ? page.entries : [];
      for (const e of entries) {
        if (typeof e.key !== "string" || typeof e.value !== "string" || !e.key.startsWith(PLAYER_PREFIX) || e.key.length !== 66) return out;
        if (decodePlayer(e.value, this.table.prefix).grant?.relayer === this.relayer) out.push(bytesAddress(hexBytes(e.key.slice(2), 32), this.table.prefix));
      }
      if (typeof page?.next !== "string" || entries.length === 0) return out;
      after = page.next;
    }
  }

  async link(account: string): Promise<LinkClaim | null> {
    const l = await this.client.xLink(account);
    return l ? { handle: l.handle, post_id: l.post_id, code: l.code } : null;
  }

  private async run(c: TableCall, onSigned: OnSigned): Promise<Landed> {
    try {
      const r = await send(this.wallet, this.program, c, { onSigned: (tx, id) => onSigned(id, BigInt(tx.valid_until)) });
      return { txId: String(r.tx.id), height: BigInt(r.tx.height), events: eventsOf(r.receipt, this.program) };
    } catch (e) {
      // The program's abort code is a number; its message is the program's text and is never read.
      if (e instanceof ProgramFailedError) throw new TableRefused(e.receipt.outcome.abort_code, e.id);
      throw e;
    }
  }

  relayBet(player: string, amount: bigint, postId: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.run(call.relayBet(player, amount, postId), onSigned);
  }

  relayHit(game: bigint, postId: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.run(call.relayHit(game, postId), onSigned);
  }

  relayStand(game: bigint, postId: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.run(call.relayStand(game, postId), onSigned);
  }

  relayWithdraw(player: string, postId: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.run(call.relayWithdraw(player, postId), onSigned);
  }

  timeout(game: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.run(call.timeout(game), onSigned);
  }

  async tx(id: string): Promise<TxState> {
    let t: { status?: unknown; dropped?: unknown; height?: unknown };
    try {
      t = await this.client.tx(id);
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return { status: "unknown" };
      throw e;
    }
    if (t.status === "pending") return { status: "pending" };
    // Dropped without running: nothing was charged, and a node that still holds it may run it yet.
    if (t.status === "failed" && t.dropped === true) return { status: "unknown" };
    let receipt = null;
    try {
      receipt = checkReceipt(this.rpc, t);
    } catch {
      receipt = null;
    }
    if (t.status === "failed") return { status: "refused", code: receipt?.outcome.abort_code, txId: id };
    if (t.status !== "finalized" || !receipt || typeof t.height !== "number") return { status: "unknown" };
    if (!receipt.outcome.ok) return { status: "refused", code: receipt.outcome.abort_code, txId: id };
    return { status: "landed", landed: { txId: id, height: BigInt(t.height), events: eventsOf(receipt, this.program) } };
  }
}
