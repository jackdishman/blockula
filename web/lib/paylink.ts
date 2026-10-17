/**
 * Payment links: `<wallet>/send?to=<name or address>&amount=<FANG>&memo=<text>&chain=<chain id>&genesis=<hex>`.
 * Send reads them, fills the form and shows a "a link filled in this payment" warning; the payer
 * still reviews and signs it themselves. A link never chooses the node or the network (see
 * lib/config). `chain` and `genesis` (the first GENESIS_HEX hex digits of the genesis hash) are a
 * check only: a `.boo` name can belong to someone else on another network, so Send refuses a link
 * made on another chain than the wallet's and says which one it was made for (audit main-11-1).
 * Links made before the check (no `chain`) still fill the form, with a notice.
 * No imports, so lib/qr.test.mjs can run it under plain Node.
 */

/** Hex digits of the genesis hash a link carries: enough to tell two instances of a chain id apart. */
export const GENESIS_HEX = 16;

/** The most drops an amount can be (a u64 on chain): about 18.4 billion FANG. */
export const MAX_DROPS = (1n << 64n) - 1n;

/** The chain a link was made on, as it says. */
export interface LinkChain {
  chainId: string;
  /** The start of its genesis hash (lowercase hex), if the link says. */
  genesis?: string;
}

/** The wallet's chain: its pin (lib/chain.ts `ChainPin`). Unknown fields aren't known yet. */
export interface WalletChain {
  chainId?: string;
  genesisHash?: string;
}

/** Builds a payment link. Empty amount or memo are left out; `chain` is the payee's chain (its pin). */
export function payLink(walletUrl: string, to: string, amount?: string, memo?: string, chain?: WalletChain): string {
  const q = new URLSearchParams({ to });
  if (amount?.trim()) q.set("amount", amount.trim());
  if (memo) q.set("memo", memo);
  if (chain?.chainId) {
    q.set("chain", chain.chainId);
    if (chain.genesisHash && /^[0-9a-f]{64}$/i.test(chain.genesisHash)) q.set("genesis", chain.genesisHash.slice(0, GENESIS_HEX).toLowerCase());
  }
  return `${walletUrl.replace(/\/$/, "")}/send?${q}`;
}

/** What a link's query says about its chain: `null` for a link without one (made before the check). */
export function linkChain(params: { get(key: string): string | null }): LinkChain | null {
  const chainId = params.get("chain")?.trim();
  if (!chainId) return null;
  const genesis = params.get("genesis")?.trim().toLowerCase();
  return genesis ? { chainId, genesis } : { chainId };
}

export type LinkCheck =
  /** The link doesn't say which chain it was made on. */
  | { state: "unchecked" }
  /** The wallet doesn't know its own chain yet (its node hasn't answered). */
  | { state: "pending" }
  | { state: "match" }
  /** Made on another chain id, or on another instance of this one (or with a malformed genesis). */
  | { state: "mismatch"; why: "chain" | "genesis" };

/** Whether a link made on `link` may fill a payment on the wallet's chain. */
export function checkLinkChain(link: LinkChain | null, wallet: WalletChain): LinkCheck {
  if (!link) return { state: "unchecked" };
  if (!wallet.chainId) return { state: "pending" };
  if (link.chainId !== wallet.chainId) return { state: "mismatch", why: "chain" };
  if (link.genesis === undefined) return { state: "match" };
  // Exactly what payLink writes: a shorter prefix would match too many chains.
  if (!new RegExp(`^[0-9a-f]{${GENESIS_HEX}}$`).test(link.genesis)) return { state: "mismatch", why: "genesis" };
  if (!wallet.genesisHash) return { state: "pending" };
  return wallet.genesisHash.toLowerCase().startsWith(link.genesis) ? { state: "match" } : { state: "mismatch", why: "genesis" };
}

/**
 * Why an amount typed into a payment request can't go in a link, or null if it can (empty is
 * fine: the payer chooses). The same rule as the SDK's `parseFang` (up to 9 decimals), and at
 * most a u64 of drops: anything longer would be refused by Send anyway, and could make the link
 * too long for a QR code (audit main-11-2).
 */
export function requestAmountError(amount: string): string | null {
  const s = amount.trim();
  if (!s) return null;
  if (!/^\d+(\.\d{1,9})?$/.test(s)) return "Use a number like 1 or 0.25 (up to 9 decimals).";
  const [whole, frac = ""] = s.split(".");
  const drops = BigInt(whole) * 1_000_000_000n + BigInt(frac.padEnd(9, "0"));
  if (drops === 0n) return "Enter more than zero, or leave it empty.";
  if (drops > MAX_DROPS) return "That's more FANG than there can ever be.";
  return null;
}
