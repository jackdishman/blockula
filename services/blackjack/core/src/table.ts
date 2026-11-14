/**
 * A blackjack table on a chain: its records read from the node, and the calls a wallet signs.
 *
 * Reads go through the node's storage route (`GET /api/program/{address}/storage`), which every
 * node serves, validators included; nothing here needs program views. What comes back is the
 * program's stored bytes, decoded here.
 */
import type { ProgramCallOptions, ProgramReceipt, ProgramTxResult, Wallet } from "@blockula/sdk";
import { DROPS_PER_FANG, EMPTY_PLAYER, MAX_OPEN_GAMES, decodeEvent, decodeGame, decodeHouse, decodeLimits, decodeOpenList, decodePlayer, input, prefixOf, storageKey, type Game, type House, type Limits, type PlayerRecord, type TableEvent } from "./codec.js";
import { hexBytes, toHex } from "./rules.js";

/** A GET under the node's `/api`, answering parsed JSON (and throwing on anything but 2xx). */
export type ApiGet = (path: string) => Promise<unknown>;

/** `ApiGet` over `fetch` for a node at `rpc`. */
export function apiGet(rpc: string, opts: { timeoutMs?: number; fetch?: typeof fetch } = {}): ApiGet {
  const base = rpc.replace(/\/$/, "") + "/api";
  const f = opts.fetch ?? fetch;
  return async (path) => {
    const res = await f(base + path, { signal: AbortSignal.timeout(opts.timeoutMs ?? 15_000) });
    const text = await res.text();
    if (!res.ok) throw new Error(`${base}${path} answered ${res.status}`);
    return JSON.parse(text);
  };
}

/** The largest key below `key` that a storage page can start after, so the page's first entry is `key` if it's there. */
function before(key: string): string {
  const b = hexBytes(key);
  if (b.length === 0) throw new Error("an empty storage key");
  const last = b[b.length - 1];
  if (last === 0) return toHex(b.slice(0, -1));
  const out = new Uint8Array(128).fill(0xff);
  out.set(b.slice(0, -1), 0);
  out[b.length - 1] = last - 1;
  return toHex(out);
}

export class Table {
  /** The chain's address prefix, from the program's own address. */
  readonly prefix: string;

  constructor(
    readonly program: string,
    private readonly get: ApiGet,
  ) {
    this.prefix = prefixOf(program);
  }

  /** A table read from the node at `rpc`. */
  static at(rpc: string, program: string, opts: { timeoutMs?: number; fetch?: typeof fetch } = {}): Table {
    return new Table(program, apiGet(rpc, opts));
  }

  /** One stored value by its key (hex), or `null`; and the height of the state it was read from. */
  async entry(key: string): Promise<{ value: string | null; height: number }> {
    const after = before(key);
    const page = (await this.get(`/program/${this.program}/storage?limit=1${after ? `&after=${after}` : ""}`)) as { entries?: { key?: unknown; value?: unknown }[]; height?: unknown };
    const first = Array.isArray(page?.entries) ? page.entries[0] : undefined;
    const height = typeof page?.height === "number" ? page.height : 0;
    if (!first || first.key !== key) return { value: null, height };
    if (typeof first.value !== "string") throw new Error("the node's storage entry has no value");
    return { value: first.value, height };
  }

  /** The house: who runs it and where every drop is. Throws if the table isn't set up. */
  async house(): Promise<House & { height: number }> {
    const { value, height } = await this.entry(storageKey.house);
    if (value === null) throw new Error(`${this.program} isn't a blackjack table that's set up`);
    return { ...decodeHouse(value, this.prefix), height };
  }

  async limits(): Promise<Limits> {
    const { value } = await this.entry(storageKey.limits);
    if (value === null) throw new Error(`${this.program} isn't a blackjack table that's set up`);
    return decodeLimits(value);
  }

  /** An open hand, or `null` once it's settled (or if it never was). */
  async game(id: bigint): Promise<Game | null> {
    const { value } = await this.entry(storageKey.game(id));
    return value === null ? null : decodeGame(id, value, this.prefix);
  }

  /** A player's record: table balance, the day's wagers, open hand, grant, last hand. */
  async player(account: string): Promise<PlayerRecord> {
    const { value } = await this.entry(storageKey.player(account));
    return value === null ? EMPTY_PLAYER : decodePlayer(value, this.prefix);
  }

  /** The open hands' ids. */
  async openGames(): Promise<bigint[]> {
    const [house, list] = await Promise.all([this.house(), this.entry(storageKey.open)]);
    if (list.value === null) return [];
    return decodeOpenList(list.value, Math.min(house.open, MAX_OPEN_GAMES));
  }

  /** Commitment `k`, while no bet has taken it. */
  async commitment(k: bigint): Promise<string | null> {
    return (await this.entry(storageKey.commitment(k))).value;
  }

  /** Whether the owner approved `account` as a relayer. */
  async relayerApproved(account: string): Promise<boolean> {
    return (await this.entry(storageKey.relayer(account))).value !== null;
  }
}

/** One call of the table: what a wallet signs. Amounts are drops. */
export interface TableCall {
  method: string;
  /** Lowercase hex. */
  input: string;
  /** FANG sent with it (a bet's stake, a deposit). */
  value?: bigint;
  /** The most its writes may bond: 0 for a call that stores nothing new. */
  storageLimit?: bigint;
}

/** What a call that stores a record may bond at most: the deposit comes back when the record is removed. */
const BOND = DROPS_PER_FANG / 4n;

/** Every call, built. `storageLimit` is set where the call stores something new. */
export const call = {
  fund: (amount: bigint): TableCall => ({ method: "fund", input: input.fund(), value: amount }),
  defund: (amount: bigint): TableCall => ({ method: "defund", input: input.defund(amount) }),
  setLimits: (l: Limits): TableCall => ({ method: "set_limits", input: input.setLimits(l) }),
  pause: (on: boolean): TableCall => ({ method: "pause", input: input.pause(on) }),
  setDealer: (dealer: string): TableCall => ({ method: "set_dealer", input: input.setDealer(dealer) }),
  approveRelayer: (relayer: string, on: boolean): TableCall => ({ method: "approve_relayer", input: input.approveRelayer(relayer, on), storageLimit: BOND }),
  commitBatch: (first: bigint, commitments: readonly string[]): TableCall => ({ method: "commit_batch", input: input.commitBatch(first, commitments), storageLimit: BOND + (BigInt(commitments.length) * DROPS_PER_FANG) / 20n }),
  skipCommits: (count: number): TableCall => ({ method: "skip_commits", input: input.skipCommits(count) }),
  deposit: (amount: bigint): TableCall => ({ method: "deposit", input: input.deposit(), value: amount, storageLimit: BOND }),
  withdraw: (amount: bigint): TableCall => ({ method: "withdraw", input: input.withdraw(amount) }),
  setRelayer: (relayer: string, maxBet: bigint, dailyCap: bigint, expiresAt: bigint): TableCall => ({ method: "set_relayer", input: input.setRelayer(relayer, maxBet, dailyCap, expiresAt), storageLimit: BOND }),
  revoke: (): TableCall => ({ method: "revoke", input: input.revoke() }),
  /** A bet's first transaction: the stake and the seed's hash (`seedHash(account, clientSeed)`). Keep the seed: `revealSeed` needs it. */
  bet: (stake: bigint, seedHash: string): TableCall => ({ method: "bet", input: input.bet(seedHash), value: stake, storageLimit: BOND }),
  /** A bet's second transaction, sent once the first is final: the seed itself. Only then does the dealer deal. */
  revealSeed: (game: bigint, clientSeed: string): TableCall => ({ method: "reveal_seed", input: input.revealSeed(game, clientSeed) }),
  relayBet: (player: string, amount: bigint, postId: bigint): TableCall => ({ method: "relay_bet", input: input.relayBet(player, amount, postId), storageLimit: BOND }),
  hit: (game: bigint): TableCall => ({ method: "hit", input: input.hit(game) }),
  stand: (game: bigint): TableCall => ({ method: "stand", input: input.stand(game) }),
  relayHit: (game: bigint, postId: bigint): TableCall => ({ method: "relay_hit", input: input.relayHit(game, postId) }),
  relayStand: (game: bigint, postId: bigint): TableCall => ({ method: "relay_stand", input: input.relayStand(game, postId) }),
  relayWithdraw: (player: string, postId: bigint): TableCall => ({ method: "relay_withdraw", input: input.relayWithdraw(player, postId) }),
  reveal: (game: bigint, links: readonly string[]): TableCall => ({ method: "reveal", input: input.reveal(game, links) }),
  timeout: (game: bigint): TableCall => ({ method: "timeout", input: input.timeout(game) }),
} as const;

/**
 * Signs and submits one call with `wallet` (`Wallet.call`: it binds the program's version digest,
 * and `versionDigest` when you pass the one you showed), and waits for it to be final. Throws the
 * SDK's errors: `ProgramFailedError` carries the receipt of a call the program refused.
 */
export function send(wallet: Wallet, program: string, c: TableCall, opts: ProgramCallOptions & { versionDigest?: string } = {}): Promise<ProgramTxResult> {
  return wallet.call(program, c.method, c.input, { ...opts, value: c.value ?? 0n, storageLimit: c.storageLimit ?? 0n });
}

/** The table's own events in a receipt, decoded, in order. Events of other programs, and names the table doesn't emit, are left out. */
export function eventsOf(receipt: Pick<ProgramReceipt, "claims">, program: string): TableEvent[] {
  const prefix = prefixOf(program);
  const out: TableEvent[] = [];
  for (const e of receipt.claims.events) {
    if (e.program !== program) continue;
    const decoded = decodeEvent(e.untrusted_name, e.data, prefix);
    if (decoded) out.push(decoded);
  }
  return out;
}

/** The first event called `name` among `events`. */
export function eventNamed<N extends TableEvent["name"]>(events: readonly TableEvent[], name: N): Extract<TableEvent, { name: N }> | undefined {
  return events.find((e) => e.name === name) as Extract<TableEvent, { name: N }> | undefined;
}

/**
 * Uploads the program and creates a table owned by `owner`, with `dealer` as its dealer and the
 * default limits. The program takes no plain transfers (so its balance is exactly what its
 * methods account for). Returns the table's address once the create is final.
 */
export async function createTable(owner: Wallet, wasm: Uint8Array, iface: Uint8Array, dealer: string): Promise<string> {
  const up = await owner.uploadCode(wasm, iface);
  const created = await owner.createProgram({ wasmHash: up.wasmHash, interfaceHash: up.interfaceHash, init: { method: "init", input: input.init(dealer) }, storageLimit: "1" });
  return created.address;
}
