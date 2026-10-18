import { Client, HeightAheadError, heightLimit } from "@blockula/sdk";
import { KNOWN_GENESIS, NETWORK, type GenesisClock, type Network } from "./config";
import type { Status } from "./types";

/** What the node must report: the network's pins, then whatever it reported first (unpinned nodes). */
export interface ChainPin {
  chainId?: string;
  genesisHash?: string;
  prefix?: string;
}

type Reported = ChainPin & { chainId: string };

/** The node reported a different chain than the one this wallet is pinned to. */
export class ChainMismatchError extends Error {
  constructor(readonly expected: ChainPin, readonly reported: Reported) {
    super(describe(expected, reported));
    this.name = "ChainMismatchError";
  }
}

const short = (h?: string) => (h ? `${h.slice(0, 8)}…${h.slice(-4)}` : "none");

function describe(e: ChainPin, r: Reported) {
  if (e.chainId && e.chainId !== r.chainId) return `This node is on chain “${r.chainId}”, not “${e.chainId}”.`;
  if (e.genesisHash && e.genesisHash !== r.genesisHash) return `This node's “${r.chainId}” starts from genesis ${short(r.genesisHash)}, not ${short(e.genesisHash)}.`;
  return `This node uses “${r.prefix}1…” addresses, not “${e.prefix}1…”.`;
}

/** The address prefix a node reports, if it reports one (protocol v1 nodes; DEC-7). */
const prefixOf = (s: Status) => s.address_prefix ?? s.prefix;

/** Compare a node's status with a pin; null when it matches. */
export function chainMismatch(pin: ChainPin, s: Status): ChainMismatchError | null {
  const reported: Reported = { chainId: s.chain_id, genesisHash: s.genesis_hash?.toLowerCase(), prefix: prefixOf(s) };
  // A known chain id must come with that chain's genesis, whichever network the node claims to be.
  const want: ChainPin = { ...pin, genesisHash: pin.genesisHash ?? KNOWN_GENESIS[pin.chainId ?? reported.chainId] };
  const bad =
    (want.chainId && want.chainId !== reported.chainId) ||
    (want.genesisHash && want.genesisHash !== reported.genesisHash) ||
    (want.prefix && reported.prefix && want.prefix !== reported.prefix);
  return bad ? new ChainMismatchError(want, reported) : null;
}

const PINS = "blockula.customChain";

/** Custom nodes are trusted on first use: remember the chain they reported, per node URL. */
export function savedCustomPin(rpc: string): ChainPin | undefined {
  try {
    return (JSON.parse(localStorage.getItem(PINS) ?? "{}") as Record<string, ChainPin>)[rpc];
  } catch {
    return undefined;
  }
}

export function saveCustomPin(rpc: string, pin: ChainPin | null) {
  try {
    const all = JSON.parse(localStorage.getItem(PINS) ?? "{}") as Record<string, ChainPin>;
    if (pin) all[rpc] = pin;
    else delete all[rpc];
    localStorage.setItem(PINS, JSON.stringify(all));
  } catch {
    /* storage unavailable */
  }
}

export type { GenesisClock };

/** The genesis clock in a node's `/api/genesis` answer for `chainId`, if it is one. */
export function clockOf(genesis: unknown, chainId: string): GenesisClock | undefined {
  const g = genesis && typeof genesis === "object" ? (genesis as Record<string, unknown>) : {};
  const { created_at_ms: createdAtMs, block_time_ms: blockTimeMs } = g;
  if (g.chain_id !== chainId || !Number.isSafeInteger(createdAtMs) || (createdAtMs as number) < 0 || !Number.isSafeInteger(blockTimeMs) || (blockTimeMs as number) < 1) return undefined;
  return { createdAtMs: createdAtMs as number, blockTimeMs: blockTimeMs as number };
}

const CLOCKS = "blockula.genesisClock";

/** Genesis clocks are a property of the chain (its id and genesis hash), remembered on first use. */
function savedClock(chainId: string, genesisHash: string): GenesisClock | undefined {
  try {
    return clockOf({ ...(JSON.parse(localStorage.getItem(CLOCKS) ?? "{}") as Record<string, Record<string, unknown>>)[`${chainId} ${genesisHash}`], chain_id: chainId }, chainId);
  } catch {
    return undefined;
  }
}

function saveClock(chainId: string, genesisHash: string, c: GenesisClock) {
  try {
    const all = JSON.parse(localStorage.getItem(CLOCKS) ?? "{}") as Record<string, unknown>;
    all[`${chainId} ${genesisHash}`] = { created_at_ms: c.createdAtMs, block_time_ms: c.blockTimeMs };
    localStorage.setItem(CLOCKS, JSON.stringify(all));
  } catch {
    /* storage unavailable: this session only */
  }
}

/** The pin to start from for a network: its configured chain, or a custom node's remembered one. */
export function initialPin(net: Network): ChainPin {
  if (net.key === "custom") return (typeof window !== "undefined" && savedCustomPin(net.rpc)) || {};
  return { chainId: net.chainId, genesisHash: net.genesisHash, prefix: net.prefix };
}

/**
 * The SDK client, except every status read must match the pinned chain. `Wallet.build` signs
 * the chain id and genesis hash from `status()`, so no tx can be signed for a chain other than
 * the one the wallet shows. Unpinned fields are pinned from the first status seen, so a node can't
 * switch chains mid-session either.
 */
export class PinnedClient extends Client {
  constructor(rpc: string, public pin: ChainPin, private onFirstPin?: (pin: ChainPin) => void, witnesses: string[] = [], genesisClock?: GenesisClock) {
    // Witnesses only for networks whose chain is pinned by configuration: they're held to it too.
    super(rpc, { genesisClock, witnesses: pin.chainId ? witnesses.map((w) => new Client(w, { chainId: pin.chainId, genesisHash: pin.genesisHash, prefix: pin.prefix })) : [] });
    // `Wallet.address` renders with the client's prefix: use the pinned one before the first
    // status arrives (which still checks it, through `status()` below).
    if (pin.prefix) this.prefix = pin.prefix;
  }

  private clock?: Promise<GenesisClock | undefined>;

  /**
   * The pinned chain's genesis clock: remembered in this browser for the chain's id and genesis
   * hash, else read from the node's `/api/genesis` once and remembered (trust on first use, like
   * a custom node's pin). `undefined` when the node serves none.
   */
  private chainClock(): Promise<GenesisClock | undefined> {
    // A release pin is authoritative even on first use and if browser storage was poisoned.
    if (this.genesisClock) return Promise.resolve(this.genesisClock);
    const { chainId, genesisHash } = this.pin;
    if (!chainId || !genesisHash) return Promise.resolve(undefined);
    const saved = typeof window === "undefined" ? undefined : savedClock(chainId, genesisHash);
    if (saved) return Promise.resolve(saved);
    this.clock ??= this.genesis().then(
      (g) => {
        const c = clockOf(g, chainId);
        if (c && typeof window !== "undefined") saveClock(chainId, genesisHash, c);
        return c;
      },
      () => undefined,
    );
    return this.clock;
  }

  /**
   * Before anything is signed, the height a tx's validity window counts from must be one the chain
   * can have reached by this machine's clock and the chain's genesis clock: a node reporting a
   * future height could hold the signed tx and release it later, when a name signed in it may
   * have changed hands (audit review-r3-clients-2). Then the SDK's own checks (witnesses).
   */
  async checkSigningHeight(height: number): Promise<void> {
    const c = await this.chainClock();
    if (c) {
      const limit = heightLimit(c.createdAtMs, c.blockTimeMs, Date.now());
      if (height > limit) throw new HeightAheadError(this.rpc, height, limit, "this machine's clock and the chain's genesis");
    }
    return super.checkSigningHeight(height);
  }

  async status(): Promise<Status> {
    const s = (await super.status()) as Status;
    const err = chainMismatch(this.pin, s);
    if (err) throw err;
    const filled: ChainPin = { chainId: this.pin.chainId ?? s.chain_id, genesisHash: this.pin.genesisHash ?? s.genesis_hash?.toLowerCase(), prefix: this.pin.prefix ?? prefixOf(s) };
    if (filled.chainId !== this.pin.chainId || filled.genesisHash !== this.pin.genesisHash || filled.prefix !== this.pin.prefix) {
      this.pin = filled;
      this.onFirstPin?.(filled);
    }
    return s;
  }
}

/** The client for the selected network. Custom nodes remember the chain they first reported. */
export function networkClient() {
  return new PinnedClient(
    NETWORK.rpc.replace(/\/$/, ""),
    initialPin(NETWORK),
    (pin) => {
      if (NETWORK.key === "custom") saveCustomPin(NETWORK.rpc, pin);
    },
    NETWORK.witnesses ?? [],
    NETWORK.genesisClock,
  );
}
