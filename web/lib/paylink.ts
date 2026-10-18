/**
 * Payment links: `<wallet>/send?to=<name or address>&amount=<FANG>&memo=<text>&chain=<chain id>&genesis=<hex>`.
 * Send reads them, fills the form and shows a "a link filled in this payment" warning; the payer
 * still reviews and signs it themselves. A link never chooses the node or the network (see
 * lib/config). `chain` and `genesis` (the first GENESIS_HEX hex digits of the genesis hash) are a
 * check only: a `.boo` name can belong to someone else on another network, so Send refuses a link
 * made on another chain than the wallet's and says which one it was made for (audit main-11-1).
 * Links made before the check (no `chain`) still fill the form, with a notice.
 * A link's `chain` and `genesis` are the sender's text: one that isn't a chain id (the genesis
 * builder's rule, CHAIN_ID) or GENESIS_HEX hex digits is refused as malformed, and the wallet
 * names only networks it knows, never the link's own words (audit review-r3-wallet-4).
 * No imports, so lib/qr.test.mjs can run it under plain Node.
 */

/** Hex digits of the genesis hash a link carries: enough to tell two instances of a chain id apart. */
export const GENESIS_HEX = 16;

/** The most drops an amount can be (a u64 on chain): about 18.4 billion FANG. */
export const MAX_DROPS = (1n << 64n) - 1n;

/** A chain id, as the genesis builder allows them (node/src/genesis_builder.rs): 1-64 of [A-Za-z0-9-]. */
export const CHAIN_ID = /^[A-Za-z0-9-]{1,64}$/;

/** The most bytes a memo can be (UTF-8), as the chain allows. */
export const MAX_MEMO_BYTES = 256;

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

/**
 * Builds a payment link; `chain` is the payee's chain (its pin). The amount goes in as its
 * canonical decimal (fangDecimal), not as typed, so no spelling of an amount ("0001.50") makes a
 * link longer than the largest amount does (audit review-r3-wallet-3). What a link can't carry is
 * left out rather than passed on: an empty or invalid amount (requestAmountError), an empty memo
 * or one over MAX_MEMO_BYTES, a chain id that isn't one (CHAIN_ID). So with the payee's own name
 * or address, every link this makes fits a QR code (lib/qr.test.mjs checks the longest).
 */
export function payLink(walletUrl: string, to: string, amount?: string, memo?: string, chain?: WalletChain): string {
  const q = new URLSearchParams({ to });
  const drops = amount === undefined ? null : requestDrops(amount);
  if (drops !== null) q.set("amount", fangDecimal(drops));
  if (memo && utf8Bytes(memo) <= MAX_MEMO_BYTES) q.set("memo", memo);
  setChain(q, chain);
  return `${walletUrl.replace(/\/$/, "")}/send?${q}`;
}

/**
 * Device links: `<wallet>/link?account=<address>&chain=<chain id>&genesis=<hex>`, the QR code
 * Devices & agents → Link a device shows. The new device opens it with the account filled in, and
 * the user still checks it against the old device before the new one signs its consent to joining.
 * Always the account's address, never a name: the old device knows its own address, and a name
 * would be a node's word on the new one. `chain` and `genesis` are the same check as a payment
 * link's (checkLinkChain).
 */
export function deviceLink(walletUrl: string, account: string, chain?: WalletChain): string {
  const q = new URLSearchParams({ account });
  setChain(q, chain);
  return `${walletUrl.replace(/\/$/, "")}/link?${q}`;
}

/** Adds the chain a link is for (a check for the wallet that opens it, never a switch). */
function setChain(q: URLSearchParams, chain?: WalletChain) {
  if (!chain?.chainId || !CHAIN_ID.test(chain.chainId)) return;
  q.set("chain", chain.chainId);
  if (chain.genesisHash && /^[0-9a-f]{64}$/i.test(chain.genesisHash)) q.set("genesis", chain.genesisHash.slice(0, GENESIS_HEX).toLowerCase());
}

/** UTF-8 bytes in `s`, as the chain counts a memo. */
const utf8Bytes = (s: string) => new TextEncoder().encode(s).length;

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
  /**
   * Refused. `chain`: made on another chain id; `genesis`: on another instance of this one;
   * `malformed`: its chain id isn't one (CHAIN_ID), or its genesis isn't GENESIS_HEX hex digits.
   * Only `chain` and `genesis` are well-formed enough to be named, and only `genesis` (hex) as is.
   */
  | { state: "mismatch"; why: "chain" | "genesis" | "malformed" };

/** Exactly what payLink writes: a shorter prefix would match too many chains. */
const GENESIS_PREFIX = new RegExp(`^[0-9a-f]{${GENESIS_HEX}}$`);

/** Whether a link made on `link` may fill a payment on the wallet's chain. */
export function checkLinkChain(link: LinkChain | null, wallet: WalletChain): LinkCheck {
  if (!link) return { state: "unchecked" };
  // The sender's text, checked before anything reads or shows it.
  if (!CHAIN_ID.test(link.chainId) || (link.genesis !== undefined && !GENESIS_PREFIX.test(link.genesis))) return { state: "mismatch", why: "malformed" };
  if (!wallet.chainId) return { state: "pending" };
  if (link.chainId !== wallet.chainId) return { state: "mismatch", why: "chain" };
  if (link.genesis === undefined) return { state: "match" };
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
  const drops = parseDrops(s);
  if (drops === null) return "Use a number like 1 or 0.25 (up to 9 decimals).";
  if (drops === 0n) return "Enter more than zero, or leave it empty.";
  if (drops > MAX_DROPS) return "That's more FANG than there can ever be.";
  return null;
}

/** The drops a payment request's amount asks for: null when it's empty or requestAmountError refuses it. */
export function requestDrops(amount: string): bigint | null {
  const s = amount.trim();
  if (!s || requestAmountError(s) !== null) return null;
  return parseDrops(s);
}

/** Drops as the shortest decimal FANG amount ("1.5", "0.000000001"): the SDK's formatFang without the unit. */
export function fangDecimal(drops: bigint): string {
  const frac = (drops % 1_000_000_000n).toString().padStart(9, "0").replace(/0+$/, "");
  return `${drops / 1_000_000_000n}${frac ? `.${frac}` : ""}`;
}

/** The SDK's parseFang rule (up to 9 decimals), or null. */
function parseDrops(s: string): bigint | null {
  if (!/^\d+(\.\d{1,9})?$/.test(s)) return null;
  const [whole, frac = ""] = s.split(".");
  return BigInt(whole) * 1_000_000_000n + BigInt(frac.padEnd(9, "0"));
}
