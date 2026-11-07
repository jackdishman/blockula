/**
 * Networks this wallet knows about. The node is chosen in the header (never from a link): a
 * node controls what names resolve to and what balances look like, so switching it must be a
 * deliberate act, and the network is shown on every screen.
 *
 * Each known network pins the chain the node must report (`/api/status` chain_id and, when
 * configured, genesis_hash). The wallet refuses to use a node on another chain (lib/chain.ts),
 * so a wrong or fake node can't get a testnet signature replayed on mainnet or vice versa.
 */

import type { ListedChain } from "./chains";
import { demoNode } from "./site";

/** Mainnet's chain id and address prefix (DEC-7). */
export const MAINNET_CHAIN_ID = "blockula-1";
export const MAINNET_PREFIX = "boo";

/**
 * Mainnet's genesis hash and clock, built in by the release that launches mainnet
 * (`scripts/pin-network.sh blockula-1 … --mainnet`, after the genesis ceremony), so no deploy
 * setting can change them. Until then the hash comes from NEXT_PUBLIC_MAINNET_GENESIS_HASH alone
 * and the clock from the node on first use (lib/chain.ts). A mainnet build needs that variable
 * either way, and once the hash is built in the two must agree (next.config.ts).
 */
export const MAINNET_GENESIS_HASH: string | undefined = undefined;
export const MAINNET_GENESIS_CLOCK: GenesisClock | undefined = undefined;
/** NEXT_PUBLIC_MAINNET_GENESIS_HASH, as this deployment sets it. */
export const MAINNET_GENESIS_HASH_ENV = clean(process.env.NEXT_PUBLIC_MAINNET_GENESIS_HASH)?.toLowerCase();

/**
 * The testnet (deploy/launch.sh): its own chain id, never reused (DEC-7), and the testnet prefix.
 * The public testnet was retired in November 2026 and has no public node, so no RPC is built in:
 * a deployment lists the testnet only when NEXT_PUBLIC_BLOCKULA_RPC names a node of it.
 */
export const TESTNET_CHAIN_ID = "blockula-testnet-2";
export const TESTNET_PREFIX = "tboo";
/**
 * The testnet's genesis hash, once it has launched: the release after the launch pins it here
 * (scripts/pin-network.sh). Until then, and when it's set, NEXT_PUBLIC_TESTNET_GENESIS_HASH
 * pins it instead.
 */
export const TESTNET_GENESIS_HASH: string | undefined = "b004e2f91b63429fc009088b16ec02b6d5388a03642699ae3dc6e325939f19b2";

/** A chain's genesis clock: its genesis.json's `created_at_ms` and `block_time_ms`. */
export interface GenesisClock {
  createdAtMs: number;
  blockTimeMs: number;
}

/**
 * The testnet's genesis clock, pinned with TESTNET_GENESIS_HASH (scripts/pin-network.sh), for that
 * genesis only. With it the wallet refuses a node reporting a height the chain can't have reached
 * from the first payment on, instead of taking the clock from the node on first use
 * (lib/chain.ts, audit review-r3-clients-2).
 */
export const TESTNET_GENESIS_CLOCK: GenesisClock | undefined = { createdAtMs: 1792328803000, blockTimeMs: 1000 };

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
  /** That genesis's clock, when built in: it bounds the height a tx is signed against (lib/chain.ts). */
  genesisClock?: GenesisClock;
  /** The bech32m address prefix ("boo1…"), when known. */
  prefix?: string;
  /** Real money: no browser-only keys, no faucet copy, a plain badge. */
  mainnet?: boolean;
  /**
   * Independent nodes of the same chain that names are proven against before a send signs their
   * owner (audit gap-7-1). Without witnesses a name is the selected node's word, and the wallet
   * says so.
   */
  witnesses?: string[];
}

type KnownKey = "mainnet" | "testnet" | "local";
/** `served`: the chain of the node this wallet was served by (`SERVED`). */
export type NetworkKey = KnownKey | "custom" | "served";

/**
 * The wallet a node serves at `/wallet/` (`npm run build:wallet`, node/src/wallet.rs): a static
 * export built into the `blockula` binary, so every node serves it. It talks to that node only
 * (this page's origin), its passkeys belong to this page's host, and it has no network menu:
 * the chain is whichever one the node that served it is on, pinned on first use (lib/chain.ts).
 * The code came from that same origin, so trusting it on first use trusts nothing the page
 * didn't already. Everything that differs comes from `servedConfig()`.
 */
export const SERVED = process.env.NEXT_PUBLIC_BLOCKULA_SERVED === "1";
/**
 * The project site (`NEXT_PUBLIC_SITE=project`): Blockula's own landing page at `/`
 * (components/site/), with no wallet of its own. Every other route redirects to `/` (proxy.ts),
 * and the page talks to one node only, the demo chain's (`DEMO_NODE`, lib/site.ts), to show its
 * stats. Visitors use a chain through that node's own wallet, `<node>/wallet/`.
 */
export const SITE = process.env.NEXT_PUBLIC_SITE === "project";
/** NEXT_PUBLIC_DEMO_NODE: the site's demo chain, a node's https URL (unset: the demo is resting). */
export const DEMO_NODE = SITE ? demoNode(process.env.NEXT_PUBLIC_DEMO_NODE) : undefined;
/**
 * The chains the project site lists (chains/*.json in the repo), read and checked when it's built
 * (next.config.ts, lib/chains.ts). Empty in every other build.
 */
export const SITE_CHAINS: ListedChain[] = SITE ? (JSON.parse(process.env.BLOCKULA_SITE_CHAINS || "[]") as ListedChain[]) : [];
/** Where the app's routes live under the origin: `/wallet` when served, else the root. */
export const BASE_PATH = SERVED ? "/wallet" : "";

/**
 * This page's path as the app's routes name it (`window.location.pathname` without the base
 * path, and the served export's trailing slash), for the router and comparisons with app paths.
 */
export function appPath(): string {
  const p = window.location.pathname;
  const app = BASE_PATH && (p === BASE_PATH || p.startsWith(`${BASE_PATH}/`)) ? p.slice(BASE_PATH.length) : p;
  return app.replace(/(.)\/$/, "$1") || "/";
}

/** The served wallet's settings, read from the page's own location (empty while prerendering). */
export function servedConfig(): { network: Network; rpId: string; walletUrl: string } {
  const loc = typeof window === "undefined" ? null : window.location;
  return {
    // The same label while prerendering and in the browser; the chain's own name is the node's.
    network: { key: "served", label: "this chain", rpc: loc?.origin ?? "" },
    rpId: loc?.hostname ?? "",
    walletUrl: loc ? `${loc.origin}${BASE_PATH}` : BASE_PATH,
  };
}

function clean(v: string | undefined) {
  return v?.trim() || undefined;
}

const mainnetRpc = clean(process.env.NEXT_PUBLIC_MAINNET_RPC);
const testnetRpc = clean(process.env.NEXT_PUBLIC_BLOCKULA_RPC);

/** Comma-separated https node URLs (NEXT_PUBLIC_*_WITNESSES): the witnesses names are proven against. */
export function witnessList(v: string | undefined, rpc: string): string[] {
  const out: string[] = [];
  for (const w of (v ?? "").split(",").map((x) => x.trim().replace(/\/+$/, "")).filter(Boolean)) {
    if (!isNodeUrl(w) || (!w.startsWith("https://") && !/^http:\/\/(localhost|127\.0\.0\.1)(:\d+)?$/.test(w))) throw new Error(`witness ${w} must be an https node URL`);
    if (w !== rpc.replace(/\/+$/, "") && !out.includes(w)) out.push(w);
  }
  return out;
}

const mainnetGenesis = MAINNET_GENESIS_HASH ?? MAINNET_GENESIS_HASH_ENV;

const mainnet: Network | undefined = mainnetRpc
  ? {
      key: "mainnet",
      label: "Mainnet",
      rpc: mainnetRpc,
      chainId: MAINNET_CHAIN_ID,
      genesisHash: mainnetGenesis,
      // The built-in clock is that of the built-in genesis: never another one's.
      genesisClock: mainnetGenesis && mainnetGenesis === MAINNET_GENESIS_HASH ? MAINNET_GENESIS_CLOCK : undefined,
      prefix: MAINNET_PREFIX,
      mainnet: true,
      witnesses: witnessList(process.env.NEXT_PUBLIC_MAINNET_WITNESSES, mainnetRpc),
    }
  : undefined;

const testnetGenesis = clean(process.env.NEXT_PUBLIC_TESTNET_GENESIS_HASH)?.toLowerCase() || TESTNET_GENESIS_HASH;

// The testnet is listed only when a deployment names a node of it: it has no public RPC.
const testnet: Network | undefined = testnetRpc
  ? {
      key: "testnet",
      label: "Testnet",
      rpc: testnetRpc,
      chainId: clean(process.env.NEXT_PUBLIC_TESTNET_CHAIN_ID) || TESTNET_CHAIN_ID,
      genesisHash: testnetGenesis,
      // The built-in clock is that of the built-in genesis: never another one's.
      genesisClock: testnetGenesis && testnetGenesis === TESTNET_GENESIS_HASH ? TESTNET_GENESIS_CLOCK : undefined,
      prefix: clean(process.env.NEXT_PUBLIC_TESTNET_PREFIX) || TESTNET_PREFIX,
      witnesses: witnessList(process.env.NEXT_PUBLIC_TESTNET_WITNESSES, testnetRpc),
    }
  : undefined;

/**
 * Known networks, in menu order. Mainnet and the testnet appear only once their RPC is
 * configured. The served wallet knows none: it has only the node that served it.
 */
export const NETWORKS: Partial<Record<KnownKey, Network>> = SERVED
  ? {}
  : {
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

/**
 * Passkeys are bound to this hostname; wallets created elsewhere can't be unlocked here. The
 * served wallet's belong to the host that serves it.
 */
export const RP_ID = SERVED ? servedConfig().rpId : clean(process.env.NEXT_PUBLIC_RP_ID) || "testnet.blockula.app";

/**
 * Where payment links point. Passkeys are bound to RP_ID, so links go to the wallet on that host
 * (https://testnet.blockula.app by default), even when made on a preview deploy. When this
 * page is itself served from RP_ID (production, or `localhost` in development), its own origin is used.
 */
export function walletUrl(): string {
  if (SERVED) return servedConfig().walletUrl;
  if (typeof window !== "undefined" && window.location.hostname === RP_ID) return window.location.origin;
  return `https://${RP_ID}`;
}

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

/**
 * Where a fresh browser starts: mainnet on a mainnet deployment, else the testnet when this
 * deployment configures one (NEXT_PUBLIC_BLOCKULA_RPC), else a `blockula dev` node on this machine.
 */
export const DEFAULT_NETWORK = (SERVED ? servedConfig().network : (IS_MAINNET_BUILD && NETWORKS.mainnet) || NETWORKS.testnet || NETWORKS.local) as Network;

function resolve(): Network {
  const fallback = DEFAULT_NETWORK;
  if (SERVED) return fallback;
  if (typeof window === "undefined") return fallback;
  try {
    const key = localStorage.getItem(NETWORK_KEY) as NetworkKey | null;
    if (key && key !== "custom" && key !== "served" && NETWORKS[key]) return NETWORKS[key] as Network;
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
  window.location.assign(`${BASE_PATH}/`);
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
  program: (address: string) => `${RPC}/#/program/${encodeURIComponent(address)}`,
};
