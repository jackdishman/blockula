// The networks this SDK knows by name, and what a client for each pins (DEV-1).
//
// `new Client({ network: "testnet" })` (or `Client.testnet()`) talks to the public testnet and
// pins its chain id, genesis hash, address prefix and genesis clock up front, so the first address
// it renders is already `tboo1…` and a node on any other chain is refused before anything is
// signed. `new Client("http://localhost:7171", { network: "testnet" })` does the same through your
// own node. For a chain of your own, pin it yourself: `new Client(rpc, { chainId, genesisHash })`.
//
// The testnet values match mcp/src/network.ts (`TESTNET_RPC`, `KNOWN_GENESIS`, `KNOWN_CLOCKS`) and
// web/lib/config.ts; test/integrators.ts checks the MCP's.

/** A network this SDK knows by name. */
export type NetworkName = "testnet" | "local";

/** A chain's genesis clock: its genesis.json's `created_at_ms` and `block_time_ms`. */
export interface NetworkClock {
  createdAtMs: number;
  blockTimeMs: number;
}

/** What a client for a known network uses and pins. */
export interface NetworkPreset {
  name: NetworkName;
  /** A public RPC node, when one is built in. Without one, pass your node's URL. */
  rpc?: string;
  /** The chain id every status must report. */
  chainId?: string;
  /** The genesis hash every status must report. */
  genesisHash?: string;
  /** The address prefix (`boo` or `tboo`), known before the first request. */
  prefix?: string;
  /** The genesis clock of `genesisHash`'s genesis: it bounds the height a tx is signed against. */
  genesisClock?: NetworkClock;
}

const TESTNET: NetworkPreset = {
  name: "testnet",
  rpc: "https://blockula-testnet.fly.dev",
  chainId: "blockula-testnet-2",
  genesisHash: "b004e2f91b63429fc009088b16ec02b6d5388a03642699ae3dc6e325939f19b2",
  prefix: "tboo",
  genesisClock: { createdAtMs: 1792328803000, blockTimeMs: 1000 },
};

/** A `blockula dev` chain on this machine. Nothing is pinned: `--reset` makes a new chain each time. */
const LOCAL: NetworkPreset = { name: "local", rpc: "http://localhost:7070" };

const frozen = (p: NetworkPreset): Readonly<NetworkPreset> => Object.freeze({ ...p, genesisClock: p.genesisClock ? Object.freeze({ ...p.genesisClock }) : undefined });

/** The networks this SDK knows: `new Client({ network })` pins what they list. */
export const NETWORKS: Readonly<Record<NetworkName, Readonly<NetworkPreset>>> = Object.freeze({ testnet: frozen(TESTNET), local: frozen(LOCAL) });

/** Whether `s` names a network this SDK knows. */
export function isNetworkName(s: unknown): s is NetworkName {
  return s === "testnet" || s === "local";
}
