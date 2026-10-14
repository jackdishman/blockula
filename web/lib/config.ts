/**
 * Networks this wallet knows about. The node is chosen in the header (never from a link): a
 * node controls what names resolve to and what balances look like, so switching it must be a
 * deliberate act, and the network is shown on every screen.
 *
 * Each known network pins the chain the node must report (`/api/status` chain_id and, when
 * configured, genesis_hash). The wallet refuses to use a node on another chain (lib/chain.ts),
 * so a wrong or fake node can't get a testnet signature replayed on mainnet or vice versa.
 */

/** Mainnet's chain id and address prefix (DEC-7). */
export const MAINNET_CHAIN_ID = "blockula-1";
export const MAINNET_PREFIX = "boo";

/**
 * Mainnet's genesis hash. Pinned from the environment until launch; once the ceremony has
 * produced it, hard-code it here so no deploy setting can change it.
 */
export const MAINNET_GENESIS_HASH = clean(process.env.NEXT_PUBLIC_MAINNET_GENESIS_HASH)?.toLowerCase();

/** A mainnet deployment (`NEXT_PUBLIC_NETWORK=mainnet`): stricter rules, see next.config.ts. */
export const IS_MAINNET_BUILD = process.env.NEXT_PUBLIC_NETWORK === "mainnet";

export interface Network {
  key: NetworkKey;
  label: string;
  rpc: string;
  /** The chain id the node must report. Unset for local and custom nodes (pinned on first contact). */
  chainId?: string;
  /** The genesis hash the node must report, when known. */
  genesisHash?: string;
  /** The bech32m address prefix ("boo1…"), when known. */
  prefix?: string;
  /** Real money: no browser-only keys, no faucet copy, a plain badge. */
  mainnet?: boolean;
}

type KnownKey = "mainnet" | "testnet" | "local";
export type NetworkKey = KnownKey | "custom";

function clean(v: string | undefined) {
  return v?.trim() || undefined;
}

const mainnetRpc = clean(process.env.NEXT_PUBLIC_MAINNET_RPC);
const testnetRpc = clean(process.env.NEXT_PUBLIC_BLOCKULA_RPC);

const mainnet: Network | undefined = mainnetRpc
  ? { key: "mainnet", label: "Mainnet", rpc: mainnetRpc, chainId: MAINNET_CHAIN_ID, genesisHash: MAINNET_GENESIS_HASH, prefix: MAINNET_PREFIX, mainnet: true }
  : undefined;

// A mainnet deployment lists the testnet only when it's configured explicitly.
const testnet: Network | undefined =
  !IS_MAINNET_BUILD || testnetRpc
    ? {
        key: "testnet",
        label: "Testnet",
        rpc: testnetRpc || "https://blockula-testnet.fly.dev",
        chainId: clean(process.env.NEXT_PUBLIC_TESTNET_CHAIN_ID) || "blockula-testnet",
        genesisHash: clean(process.env.NEXT_PUBLIC_TESTNET_GENESIS_HASH)?.toLowerCase(),
        // Today's public testnet predates DEC-7 and still uses "boo"; protocol v1 testnets use "tboo".
        prefix: clean(process.env.NEXT_PUBLIC_TESTNET_PREFIX) || "boo",
      }
    : undefined;

/** Known networks, in menu order. Mainnet appears only once its RPC is configured. */
export const NETWORKS: Partial<Record<KnownKey, Network>> = {
  ...(mainnet ? { mainnet } : {}),
  ...(testnet ? { testnet } : {}),
  local: { key: "local", label: "Local node", rpc: "http://localhost:7070" },
};

/** Chains with a pinned genesis: any node claiming one of these chain ids must match it. */
export const KNOWN_GENESIS: Record<string, string> = Object.fromEntries(
  Object.values(NETWORKS).flatMap((n) => (n?.chainId && n.genesisHash ? [[n.chainId, n.genesisHash]] : [])),
);

const NETWORK_KEY = "blockula.network";
const CUSTOM_KEY = "blockula.customRpc";
/** Mirrors the custom node's origin so the server can allow it in the CSP's connect-src. */
export const RPC_COOKIE = "blockula_rpc";

/** Blocks per day at Blockula's 1-second block time. Fixed here, not read from the node. */
export const BLOCKS_PER_DAY = 86_400;

/** Passkeys are bound to this hostname; wallets created elsewhere can't be unlocked here. */
export const RP_ID = clean(process.env.NEXT_PUBLIC_RP_ID) || "blockula-wallet.vercel.app";

/** Hosting domains we don't own: a wallet whose passkeys are bound to one could be taken over. */
const SHARED_HOSTS = /(^|\.)(vercel\.app|netlify\.app|pages\.dev|github\.io|fly\.dev|onrender\.com|herokuapp\.com)$/i;

/**
 * Why this deployment must not create mainnet passkeys, or null if it may. Mainnet passkeys need
 * an rpId on a domain the project owns (DEC-8): whoever controls that domain controls the keys.
 */
export function rpIdProblem(rpId = process.env.NEXT_PUBLIC_RP_ID): string | null {
  if (!IS_MAINNET_BUILD) return null;
  const id = clean(rpId);
  if (!id) return "NEXT_PUBLIC_RP_ID is unset";
  if (SHARED_HOSTS.test(id)) return `NEXT_PUBLIC_RP_ID (${id}) is on a shared hosting domain`;
  return null;
}

/** A custom node URL the user may pick: http(s), no spaces or credentials. */
export function isNodeUrl(s: string) {
  try {
    const u = new URL(s);
    return /^https?:$/.test(u.protocol) && !u.username && !u.password && /^https?:\/\/[^\s]+$/.test(s);
  } catch {
    return false;
  }
}

/** Where a fresh browser starts: mainnet on a mainnet deployment, else the testnet. */
export const DEFAULT_NETWORK = ((IS_MAINNET_BUILD && NETWORKS.mainnet) || NETWORKS.testnet || NETWORKS.mainnet || NETWORKS.local) as Network;

function resolve(): Network {
  const fallback = DEFAULT_NETWORK;
  if (typeof window === "undefined") return fallback;
  try {
    const key = localStorage.getItem(NETWORK_KEY) as NetworkKey | null;
    if (key && key !== "custom" && NETWORKS[key]) return NETWORKS[key] as Network;
    if (key === "custom") {
      const rpc = localStorage.getItem(CUSTOM_KEY);
      if (rpc && isNodeUrl(rpc)) return { key, label: "Custom node", rpc };
    }
    // Default to the local node during local development.
    if (!key && !IS_MAINNET_BUILD && ["localhost", "127.0.0.1"].includes(window.location.hostname) && !testnetRpc) {
      return NETWORKS.local as Network;
    }
  } catch {
    /* storage unavailable */
  }
  return fallback;
}

export const NETWORK = resolve();
export const RPC = NETWORK.rpc.replace(/\/$/, "");

/** Browser-only keys keep their seed in localStorage: never with real money. */
export const localKeysAllowed = (chainId?: string) => !IS_MAINNET_BUILD && !NETWORK.mainnet && chainId !== MAINNET_CHAIN_ID;

function setRpcCookie(rpc: string | null) {
  const secure = window.location.protocol === "https:" ? "; Secure" : "";
  document.cookie = rpc
    ? `${RPC_COOKIE}=${encodeURIComponent(new URL(rpc).origin)}; Path=/; Max-Age=31536000; SameSite=Strict${secure}`
    : `${RPC_COOKIE}=; Path=/; Max-Age=0; SameSite=Strict${secure}`;
}

/**
 * The CSP only lets the page talk to known nodes, plus the custom node named by a cookie. Set
 * it if it's missing (e.g. a custom node picked before the cookie existed) and reload once.
 */
export function ensureRpcCookie() {
  if (NETWORK.key !== "custom") return;
  const want = `${RPC_COOKIE}=${encodeURIComponent(new URL(NETWORK.rpc).origin)}`;
  if (document.cookie.split("; ").includes(want)) return;
  setRpcCookie(NETWORK.rpc);
  if (!sessionStorage.getItem("blockula.cspReload")) {
    sessionStorage.setItem("blockula.cspReload", "1");
    window.location.reload();
  }
}

/** Switch networks (from the header menu only) and reload so nothing mixes state across chains. */
export function setNetwork(key: NetworkKey, customRpc?: string) {
  localStorage.setItem(NETWORK_KEY, key);
  if (key === "custom" && customRpc) localStorage.setItem(CUSTOM_KEY, customRpc);
  setRpcCookie(key === "custom" ? (customRpc ?? localStorage.getItem(CUSTOM_KEY)) : null);
  window.location.assign("/");
}

export const explorer = {
  home: () => `${RPC}/#/`,
  tx: (id: string) => `${RPC}/#/tx/${id}`,
  account: (nameOrAddress: string) => `${RPC}/#/account/${encodeURIComponent(nameOrAddress)}`,
  block: (height: number) => `${RPC}/#/block/${height}`,
};
