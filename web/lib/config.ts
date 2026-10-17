/**
 * Networks this wallet knows about. The node is chosen in the header (never from a link): a
 * node controls what names resolve to and what balances look like, so switching it must be a
 * deliberate act, and a custom node is flagged on every screen.
 */
export const NETWORKS = {
  testnet: { label: "Testnet", rpc: process.env.NEXT_PUBLIC_BLOCKULA_RPC || "https://blockula-testnet.fly.dev" },
  local: { label: "Local node", rpc: "http://localhost:7070" },
} as const;

export type NetworkKey = keyof typeof NETWORKS | "custom";

const NETWORK_KEY = "blockula.network";
const CUSTOM_KEY = "blockula.customRpc";

/** Blocks per day at Blockula's 1-second block time. Fixed here, not read from the node. */
export const BLOCKS_PER_DAY = 86_400;

/** Passkeys are bound to this hostname; wallets created elsewhere can't be unlocked here. */
export const RP_ID = process.env.NEXT_PUBLIC_RP_ID || "blockula-wallet.vercel.app";

/**
 * Where payment links point. Passkeys are bound to RP_ID, so links go to the wallet on that host
 * (https://blockula-wallet.vercel.app by default), even when made on a preview deploy. When this
 * page is itself served from RP_ID (production, or `localhost` in development), its own origin is used.
 */
export function walletUrl(): string {
  if (typeof window !== "undefined" && window.location.hostname === RP_ID) return window.location.origin;
  return `https://${RP_ID}`;
}

function resolve(): { key: NetworkKey; label: string; rpc: string } {
  const fallback = { key: "testnet" as const, ...NETWORKS.testnet };
  if (typeof window === "undefined") return fallback;
  try {
    const key = localStorage.getItem(NETWORK_KEY) as NetworkKey | null;
    if (key === "local") return { key, ...NETWORKS.local };
    if (key === "custom") {
      const rpc = localStorage.getItem(CUSTOM_KEY);
      if (rpc && /^https?:\/\/[^\s]+$/.test(rpc)) return { key, label: "Custom node", rpc };
    }
    // Default to the local node during local development.
    if (!key && ["localhost", "127.0.0.1"].includes(window.location.hostname) && !process.env.NEXT_PUBLIC_BLOCKULA_RPC) {
      return { key: "local", ...NETWORKS.local };
    }
  } catch {
    /* storage unavailable */
  }
  return fallback;
}

export const NETWORK = resolve();
export const RPC = NETWORK.rpc.replace(/\/$/, "");

/** Switch networks (from the header menu only) and reload so nothing mixes state across chains. */
export function setNetwork(key: NetworkKey, customRpc?: string) {
  localStorage.setItem(NETWORK_KEY, key);
  if (key === "custom" && customRpc) localStorage.setItem(CUSTOM_KEY, customRpc);
  window.location.assign("/");
}

/** The explorer is part of this app (docs/EXPLORER.md); same paths as lib/explorer/routes. */
export const explorer = {
  home: () => "/explore",
  tx: (id: string) => `/tx/${encodeURIComponent(id)}`,
  account: (nameOrAddress: string) => `/account/${encodeURIComponent(nameOrAddress)}`,
  block: (height: number) => `/block/${height}`,
};

/** The node's own built-in explorer, for operators and as a second opinion. */
export const nodeExplorer = {
  home: () => `${RPC}/#/`,
  tx: (id: string) => `${RPC}/#/tx/${id}`,
  account: (nameOrAddress: string) => `${RPC}/#/account/${encodeURIComponent(nameOrAddress)}`,
  block: (height: number) => `${RPC}/#/block/${height}`,
};
