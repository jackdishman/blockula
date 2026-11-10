/**
 * Deployed programs the wallet keeps on the Programs page after they leave the recent-block
 * window. The node has no list of every program (docs/PROGRAMS-V0.md §8.2); `programs.ts` describes
 * a program once its address is known. To add a pin, append it under that chain's id and genesis
 * hash (docs/EXPLORER.md). Pure, so `node --test` runs it directly (lib/featured-programs.test.mjs).
 *
 * A pin matches only when the node reports that chain id and that full genesis hash, so the same
 * chain id on another genesis can't show it. `title` and `blurb` are this wallet's words, not the
 * program's or its deployer's. The page hides a pin whose address the node doesn't have.
 */

/** A deployed program this wallet pins for one chain. */
export interface FeaturedProgram {
  /** Its address (`boo1…`). */
  address: string;
  /** This wallet's title for it. */
  title: string;
  /** This wallet's description of it. */
  blurb: string;
}

/** The pins for one chain. Shown only when the node reports this chain id and this genesis. */
export interface FeaturedChain {
  chainId: string;
  /** 64 lowercase hex (`/api/status`'s `genesis_hash`). */
  genesisHash: string;
  programs: readonly FeaturedProgram[];
}

/** Featured deployed programs, by chain. */
export const FEATURED_PROGRAMS: readonly FeaturedChain[] = [
  {
    chainId: "blockula-demo-2",
    genesisHash: "e712d9376917f0c9d10c164fa7c1a53fef7c8dc1c24100e7924037d0250f705e",
    programs: [
      {
        address: "boo1men67eckh4nt0xyqu553llnvf88aa6uzmvf9wcgp0wd0v3p0thds33f4yj",
        title: "Grok Bot registry",
        blurb: "A public directory where Grok Bots register a label, an optional X handle and an optional intro tweet.",
      },
    ],
  },
];

const GENESIS_HASH = /^[0-9a-f]{64}$/;

/**
 * The pins for the chain the node reports, or none. Both the chain id and the full genesis hash
 * have to match: a chain that reused the id, or a hash we only partly know, shows nothing.
 */
export function featuredPrograms(chainId: string | undefined, genesisHash: string | undefined, chains: readonly FeaturedChain[] = FEATURED_PROGRAMS): FeaturedProgram[] {
  if (!chainId || !genesisHash) return [];
  const genesis = genesisHash.trim().toLowerCase();
  if (!GENESIS_HASH.test(genesis)) return [];
  const chain = chains.find((c) => c.chainId === chainId && c.genesisHash === genesis);
  if (!chain) return [];
  const seen = new Set<string>();
  const out: FeaturedProgram[] = [];
  for (const p of chain.programs) {
    const address = p.address.trim().toLowerCase();
    if (!address || seen.has(address)) continue;
    seen.add(address);
    out.push(p.address === address ? p : { ...p, address });
  }
  return out;
}

/** How many recent creates the Programs page lists, after the pins. Pins themselves aren't capped. */
export const RECENT_DEPLOYED_LIMIT = 12;

/** Addresses of programs whose `create_program` is finalized in these transactions, newest first, once each. */
export function createdProgramAddresses(txs: readonly { status?: string; program?: { facts?: { created?: unknown } } | null }[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const t of txs) {
    const created = t.program?.facts?.created;
    if (t.status !== "finalized" || typeof created !== "string") continue;
    const key = created.toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    out.push(created);
  }
  return out;
}

export interface DeployedSlot {
  address: string;
  /** True when this address is one of this chain's pins (it may also be a recent create). */
  featured: boolean;
}

/**
 * Featured pins first, then recent creates, each address once. A pin that the recent window also
 * contains is listed only as featured. Recent creates stop at `recentLimit`; pins don't.
 */
export function listDeployed(recent: readonly string[], featured: readonly string[], recentLimit = RECENT_DEPLOYED_LIMIT): DeployedSlot[] {
  const out: DeployedSlot[] = [];
  const seen = new Set<string>();
  const push = (address: string, isFeatured: boolean) => {
    const key = address.trim().toLowerCase();
    if (!key || seen.has(key)) return false;
    seen.add(key);
    out.push({ address: address.trim(), featured: isFeatured });
    return true;
  };
  for (const a of featured) push(a, true);
  let n = 0;
  for (const a of recent) {
    if (n >= recentLimit) break;
    if (push(a, false)) n++;
  }
  return out;
}
