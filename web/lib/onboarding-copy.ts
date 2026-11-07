/**
 * The wallet's onboarding copy in one place: where a new wallet's FANG comes from, how the wallet
 * describes what it signs with, and the safety lines at moments that can't be undone. Change the
 * words here and every screen that shows them follows.
 */

/**
 * Where FANG comes from. Each chain's FANG is its own and has no value: the chain's faucet hands it
 * out, and on a chain without one, someone sends it.
 */
export const FIRST_FANG = {
  title: "How coins reach you",
  /** Every chain without a faucet. */
  receive: "Someone sends it to you. Share your address or its QR code from Receive.",
  receiveLink: "Open Receive",
  noFaucet: "This chain has no faucet.",
  /** Next to the faucet, and in the card on a chain without one. */
  noValue: "These coins have no value: nobody sells them, buys them or gets paid in them.",
  /** The node refused a faucet request (lib/errors.ts). */
  noFaucetError: "This chain has no faucet. Share your address from Receive so someone can send you coins.",
};

/**
 * What the wallet signs with, said precisely: post-quantum signatures (ML-DSA-65) on-chain, and a
 * passkey that only unlocks the key. FANG is the chain's unit, not a currency.
 */
export const BRAND = {
  chapterOne:
    "Your coins live on a Blockula chain, a shared ledger kept by its validators. A wallet holds the secret key that proves which coins are yours and lets you move them.",
  chapterThreeTitle: "Post-quantum signatures (ML-DSA-65)",
  chapterThreeBody:
    "A large quantum computer could forge the signatures Bitcoin and Ethereum use. Blockula signs on-chain with ML-DSA-65, a NIST post-quantum standard. Your passkey only unlocks that key; it never signs anything on-chain. The passkey itself, and the account that syncs it, still use today's cryptography.",
  /** "That's FANG, …" */
  unit: "this chain's native unit",
  accountSigns: "Your account signs with ML-DSA-65, a post-quantum signature standard.",
  signingKey: "post-quantum signing key (ML-DSA-65)",
};

/**
 * The safety lines at moments that can't be undone: claiming a name, labels written on-chain, an
 * agent's budget, the recovery kit, and a wallet only one key opens.
 */
export const SAFETY = {
  /** NameClaim, under the node's price for the name typed. It's paid to the treasury (node/src/econ.rs), not the community account. */
  namePrice: (price: string, term: string, renewal: string) => `${price} for ${term}, then ${renewal} a year. The price goes to the treasury and isn't refunded.`,
  /** `grace`: the node's grace period, in words ("30 days"), when it says. */
  nameTerms: (grace: string | null) =>
    `Names are public and go to whoever asks first. They aren't internet (ICANN) domains, give you no trademark or other rights, and mustn't impersonate anyone. Renew it in time: ${grace ? `${grace} after it expires` : "after a grace period"}, anyone can register it.`,
  /** Devices & agents: labels go on-chain for good. */
  publicLabel: "(public and permanent: it's written on the chain)",
  publicLabelShort: "(public and permanent)",
  agentResponsibility:
    "You're responsible for what your agent sends. Within this limit its payments are final, like yours. AI agents make mistakes and can be tricked by text they read (memos, issues, web pages). Start small, and revoke the key here when you're done.",
  /** The recovery kit. */
  kitNeverAsk: "Blockula will never ask you for these words.",
  kitWarning: "Anyone holding these words controls this account. Keep the kit offline, and only ever type the words into the Blockula wallet's restore screen. Blockula will never ask you for them.",
  /** Home, while one key is the only way in: a passkey, or a browser-only key (test chains). */
  onePasskey: "Only this passkey opens your wallet.",
  oneBrowserKey: "Only this browser's key opens your wallet.",
  onePasskeyLost: "If you lose it, nothing can get you back in.",
};

/**
 * The home checklist (components/Tutorial): its stages, and one short line on why each task is
 * worth doing. `u` is the chain's unit (its ticker). The tasks themselves are built in Tutorial.
 */
export const TUTORIAL = {
  heading: "Your path",
  stages: {
    basics: { title: "Basics", blurb: "Coins, a name, and your first payments." },
    further: { title: "Go further", blurb: "Protect your account and take part in the chain." },
    grow: { title: "Grow the chain", blurb: "Bring in friends, validators and agents." },
    yours: { title: "Make it yours", blurb: "Change the app and put it online." },
  },
  why: {
    fang: (u: string) => `${u} is this chain's native unit.`,
    name: "A .boo name stands in for your long boo1… address, so friends can pay vlad.boo.",
    send: "A payment is signed with your post-quantum key and final within seconds.",
    receive: "Anyone can pay you from a link or QR code, with nothing to copy by hand.",
    device: "A second passkey or a recovery kit means losing one device doesn't lose your account.",
    stake: (u: string) => `Staking backs a validator and earns ${u}, but it stays locked until the unbonding period ends.`,
    vote: "Holders decide the chain's rules: vote on a proposal, or make one.",
    program: "This chain runs programs: WebAssembly anyone can upload and call.",
    invite: "This link lets a friend make their first payment, to you, right away.",
    validator: "Each validator on another machine makes the chain harder to stop.",
    agent: "A session key lets an AI agent spend from your account, up to a limit you set.",
    coding: "Give your coding agent its own wallet on this chain and a Hunt to work on.",
    layout: "The wallet and the explorer are yours to change: ask Claude Code to do it.",
    deploy: "Put the wallet on a website so friends can use it from anywhere.",
  },
  finale: "Every task done. The chain is yours.",
};
