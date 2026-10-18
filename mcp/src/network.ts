// Which chain the MCP talks to, and how it makes sure it stays on it (WAL-4).
//
// The network is chosen once, at startup, by the user: `--network <name>` / BLOCKULA_NETWORK, or
// BLOCKULA_RPC. Nothing switches it later (no probing localhost, no tool that changes it). Each
// network pins a chain id and, where known, a genesis hash; BLOCKULA_CHAIN_ID and
// BLOCKULA_GENESIS_HASH pin them for any node. Whatever isn't pinned is pinned from the first
// status the node reports (trust on first use) and, except for throwaway local dev chains,
// remembered in BLOCKULA_HOME/known-chains.json, so a node that later reports another chain is
// refused, across restarts too.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

export type NetworkKey = "mainnet" | "testnet" | "local" | "custom";

/** What the node must report in `/api/status`. */
export interface Pin {
  chainId?: string;
  genesisHash?: string;
  prefix?: string;
}

export interface Network {
  key: NetworkKey;
  label: string;
  rpc: string;
  pin: Pin;
  /** Real money: the copy and the defaults get stricter. */
  mainnet: boolean;
  /** Remember the first chain this node reports (TOFU) across restarts. Off for local dev chains, which `--reset` replaces. */
  remember: boolean;
  /**
   * Independent nodes of the same chain (BLOCKULA_WITNESSES, comma-separated): names that end up
   * in a signature are proven against them (audit gap-7-1). Empty: a name is the node's word.
   */
  witnesses: string[];
}

export const LOCAL_RPC = "http://localhost:7070";
export const TESTNET_RPC = "https://blockula-testnet.fly.dev";
/** Public mainnet RPC endpoints, run by independent operators. Empty until launch: set BLOCKULA_RPC. */
export const MAINNET_RPCS: string[] = [];

/**
 * Chains whose genesis is known: any node claiming one of these chain ids must report this
 * genesis hash, whatever network it was configured as. `blockula-1`'s hash is added here once the
 * genesis ceremony has produced it (docs/GENESIS_CEREMONY.md).
 */
export const KNOWN_GENESIS: Record<string, string> = {};

/**
 * The genesis clock of chains whose genesis is known (`created_at_ms` and `block_time_ms` of the
 * genesis.json that hashes to `KNOWN_GENESIS[id]`), added with the hash. With it the client
 * refuses a node reporting a height the chain can't have reached by this machine's clock, before
 * anything is signed (audit review-r3-clients-2; `Client.checkSigningHeight`).
 */
export const KNOWN_CLOCKS: Record<string, GenesisClock> = {};

/** A chain's genesis clock: its genesis.json's `created_at_ms` and `block_time_ms`. */
export interface GenesisClock {
  createdAtMs: number;
  blockTimeMs: number;
}

/**
 * The genesis clock in a node's `/api/genesis` answer for chain `chainId`, or `undefined` when it
 * isn't one (another chain's genesis, missing or malformed fields). The node's word: the MCP takes
 * it on first use, as it does the genesis hash, and remembers it with that hash.
 */
export function clockFrom(genesis: unknown, chainId: string): GenesisClock | undefined {
  const g = genesis && typeof genesis === "object" ? (genesis as Record<string, unknown>) : {};
  const { created_at_ms: createdAtMs, block_time_ms: blockTimeMs } = g;
  if (g.chain_id !== chainId || !Number.isSafeInteger(createdAtMs) || (createdAtMs as number) < 0 || !Number.isSafeInteger(blockTimeMs) || (blockTimeMs as number) < 1) return undefined;
  return { createdAtMs: createdAtMs as number, blockTimeMs: blockTimeMs as number };
}

/** The networks the MCP knows by name. */
export const KNOWN_NETWORKS: Record<Exclude<NetworkKey, "custom">, Omit<Network, "rpc" | "witnesses"> & { rpc?: string }> = {
  mainnet: { key: "mainnet", label: "Blockula mainnet", rpc: MAINNET_RPCS[0], pin: { chainId: "blockula-1", prefix: "boo" }, mainnet: true, remember: true },
  // The protocol v1 public testnet (deploy/launch.sh): its own chain id (never reused) and the
  // testnet prefix (DEC-7). Its genesis hash exists only once it launches: until a release adds it
  // to KNOWN_GENESIS, the first status the node reports pins it (trust on first use, remembered).
  testnet: { key: "testnet", label: "public testnet", rpc: TESTNET_RPC, pin: { chainId: "blockula-testnet-2", prefix: "tboo" }, mainnet: false, remember: true },
  local: { key: "local", label: "local dev node", rpc: LOCAL_RPC, pin: {}, mainnet: false, remember: false },
};

const isLocalUrl = (u: string) => {
  try {
    return ["localhost", "127.0.0.1", "[::1]"].includes(new URL(u).hostname);
  } catch {
    return false;
  }
};

/** This machine, or a private network (where plain http can't be intercepted from the internet). */
const isPrivateUrl = (u: string) => {
  if (isLocalUrl(u)) return true;
  const h = new URL(u).hostname;
  return /^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(h) || /\.(internal|local|lan)$/.test(h);
};

/** A node URL: http(s) without credentials; plain http only on this machine or a private network. */
export function checkRpcUrl(u: string, what = "BLOCKULA_RPC"): string {
  let url: URL;
  try {
    url = new URL(u);
  } catch {
    throw new Error(`${what} isn't a URL: ${u}`);
  }
  if (!/^https?:$/.test(url.protocol) || url.username || url.password) throw new Error(`${what} must be an http(s) URL without credentials: ${u}`);
  if (url.protocol === "http:" && !isPrivateUrl(u)) throw new Error(`${what} must use https unless the node is on this machine or a private network: ${u}`);
  return u.replace(/\/+$/, "");
}

/** `--network <name>` or `--network=<name>` from the command line. */
export function networkArg(argv: string[]): string | undefined {
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--network") return argv[i + 1];
    if (argv[i].startsWith("--network=")) return argv[i].slice("--network=".length);
  }
  return undefined;
}

/**
 * The network from the configuration: `--network` (or BLOCKULA_NETWORK) names one, BLOCKULA_RPC
 * gives the node, BLOCKULA_CHAIN_ID / BLOCKULA_GENESIS_HASH add pins. With none of them, the
 * public testnet. Contradictions are errors, never guesses.
 */
export function resolveNetwork(env: Record<string, string | undefined>, argv: string[] = []): Network {
  const named = (networkArg(argv) ?? env.BLOCKULA_NETWORK)?.trim().toLowerCase() || undefined;
  if (named !== undefined && !["mainnet", "testnet", "local", "custom"].includes(named)) throw new Error(`unknown network "${named}" (mainnet, testnet, local or custom)`);
  const rpc = env.BLOCKULA_RPC?.trim() ? checkRpcUrl(env.BLOCKULA_RPC.trim()) : undefined;
  let key: NetworkKey;
  if (named) key = named as NetworkKey;
  else if (rpc) key = (Object.values(KNOWN_NETWORKS).find((n) => n.rpc && n.rpc === rpc)?.key as NetworkKey | undefined) ?? (isLocalUrl(rpc) ? "local" : "custom");
  else key = "testnet";

  const base = key === "custom" ? { key, label: "custom node", pin: {}, mainnet: false, remember: true } : KNOWN_NETWORKS[key];
  const url = rpc ?? base.rpc;
  if (!url) {
    throw new Error(key === "mainnet" ? "no public mainnet RPC is built in yet: set BLOCKULA_RPC to a mainnet node you trust" : "the custom network needs BLOCKULA_RPC");
  }
  if (key === "local" && !isLocalUrl(url)) throw new Error(`the local network is a node on this machine, not ${url}`);
  const pin: Pin = { ...base.pin };
  const chainId = env.BLOCKULA_CHAIN_ID?.trim() || undefined;
  const genesisHash = env.BLOCKULA_GENESIS_HASH?.trim().toLowerCase() || undefined;
  if (chainId) {
    if (pin.chainId && pin.chainId !== chainId) throw new Error(`BLOCKULA_CHAIN_ID=${chainId} contradicts the ${base.label} (${pin.chainId})`);
    pin.chainId = chainId;
  }
  if (genesisHash) {
    if (!/^[0-9a-f]{64}$/.test(genesisHash)) throw new Error("BLOCKULA_GENESIS_HASH must be 64 hex characters");
    pin.genesisHash = genesisHash;
  }
  const known = pin.chainId ? KNOWN_GENESIS[pin.chainId] : undefined;
  if (known && pin.genesisHash && known !== pin.genesisHash) throw new Error(`BLOCKULA_GENESIS_HASH contradicts ${pin.chainId}'s known genesis ${known}`);
  pin.genesisHash ??= known;
  // A real-money chain is never the local or a custom network by accident.
  const mainnet = base.mainnet || pin.chainId === KNOWN_NETWORKS.mainnet.pin.chainId;
  const witnesses = (env.BLOCKULA_WITNESSES ?? "").split(",").map((w) => w.trim()).filter(Boolean).map((w) => checkRpcUrl(w, "BLOCKULA_WITNESSES"));
  if (witnesses.includes(url)) throw new Error("BLOCKULA_WITNESSES must be other nodes than BLOCKULA_RPC's");
  if (new Set(witnesses).size !== witnesses.length) throw new Error("BLOCKULA_WITNESSES lists a node twice");
  return { key, label: base.label, rpc: url, pin, mainnet, remember: base.remember, witnesses };
}

/** Why a node's status doesn't match a pin, or null if it does. Known chain ids must come with their known genesis. */
export function mismatch(pin: Pin, status: { chain_id?: unknown; genesis_hash?: unknown; address_prefix?: unknown }): string | null {
  const chainId = typeof status.chain_id === "string" ? status.chain_id : undefined;
  const genesis = typeof status.genesis_hash === "string" ? status.genesis_hash.toLowerCase() : undefined;
  const prefix = typeof status.address_prefix === "string" ? status.address_prefix : undefined;
  if (!chainId) return "the node doesn't report a chain id";
  if (pin.chainId && chainId !== pin.chainId) return `the node is on chain "${chainId}", not "${pin.chainId}"`;
  const wantGenesis = pin.genesisHash ?? KNOWN_GENESIS[chainId];
  if (wantGenesis && genesis !== wantGenesis) return `the node's "${chainId}" starts from genesis ${genesis ?? "(none reported)"}, not ${wantGenesis}`;
  if (pin.prefix && prefix && prefix !== pin.prefix) return `the node uses "${prefix}1…" addresses, not "${pin.prefix}1…"`;
  return null;
}

/** The pin after a first status: every field the node reported that wasn't pinned yet. */
export function completePin(pin: Pin, status: { chain_id?: unknown; genesis_hash?: unknown; address_prefix?: unknown }): Pin {
  return {
    chainId: pin.chainId ?? (typeof status.chain_id === "string" ? status.chain_id : undefined),
    genesisHash: pin.genesisHash ?? (typeof status.genesis_hash === "string" ? status.genesis_hash.toLowerCase() : undefined),
    prefix: pin.prefix ?? (typeof status.address_prefix === "string" ? status.address_prefix : undefined),
  };
}

/** Chains remembered on first use, by network and node URL (BLOCKULA_HOME/known-chains.json). */
export class KnownChains {
  constructor(readonly file: string) {}

  private all(): Record<string, unknown> {
    try {
      return existsSync(this.file) ? (JSON.parse(readFileSync(this.file, "utf8")) as Record<string, unknown>) : {};
    } catch {
      throw new Error(`${this.file} is corrupt; fix or delete it (it pins which chain each node must be on)`);
    }
  }

  static id(net: Network) {
    return `${net.key} ${net.rpc}`;
  }

  get(net: Network): Pin | undefined {
    return net.remember ? (this.all()[KnownChains.id(net)] as Pin | undefined) : undefined;
  }

  save(net: Network, pin: Pin) {
    if (!net.remember) return;
    this.write(KnownChains.id(net), pin);
  }

  /** Genesis clocks are a property of the chain (its id and genesis hash), whichever node reported them. */
  static clockId(chainId: string, genesisHash: string) {
    return `clock ${chainId} ${genesisHash}`;
  }

  /** The genesis clock remembered for this chain (`clockFrom` of the first `/api/genesis` read for it). */
  clock(net: Network, chainId: string, genesisHash: string): GenesisClock | undefined {
    if (!net.remember) return undefined;
    const c = this.all()[KnownChains.clockId(chainId, genesisHash)];
    return clockFrom(c && typeof c === "object" ? { ...(c as Record<string, unknown>), chain_id: chainId } : undefined, chainId);
  }

  saveClock(net: Network, chainId: string, genesisHash: string, clock: GenesisClock) {
    if (!net.remember) return;
    this.write(KnownChains.clockId(chainId, genesisHash), { created_at_ms: clock.createdAtMs, block_time_ms: clock.blockTimeMs });
  }

  private write(key: string, value: unknown) {
    const all = this.all();
    all[key] = value;
    mkdirSync(dirname(this.file), { recursive: true });
    writeFileSync(this.file, JSON.stringify(all, null, 2), { mode: 0o600 });
  }
}

/**
 * The pin to enforce for a network: its configured pin, filled from what was remembered for it.
 * A remembered field never overrides a configured one; a contradiction means the configuration
 * changed, and the configuration wins. What was remembered for another chain id (the network was
 * relaunched under a new one, which this release pins) isn't used at all: the new chain is
 * pinned on first use instead of refused for the old one's genesis.
 */
export function effectivePin(net: Network, remembered?: Pin): Pin {
  const r = remembered && (!net.pin.chainId || !remembered.chainId || remembered.chainId === net.pin.chainId) ? remembered : undefined;
  return { chainId: net.pin.chainId ?? r?.chainId, genesisHash: net.pin.genesisHash ?? r?.genesisHash, prefix: net.pin.prefix ?? r?.prefix };
}

/** The error an agent sees when a node is on the wrong chain: what happened and what the user can do. */
export function mismatchError(net: Network, why: string, knownChainsFile: string) {
  const fix = net.remember
    ? ` If this network was relaunched on purpose, the user can remove its entry ("${KnownChains.id(net)}") from ${knownChainsFile}, or set BLOCKULA_GENESIS_HASH.`
    : net.key === "local"
      ? " A reset dev chain starts from a new genesis: call start_dev_node to use the one running now."
      : "";
  return new Error(`Refusing ${net.rpc} (${net.label}): ${why}. Nothing was signed or sent.${fix}`);
}
