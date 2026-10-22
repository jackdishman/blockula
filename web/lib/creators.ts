/**
 * Creators' surfaces (product review, wallet-creators): memo hygiene, honest GitHub links, how a
 * multisig reads, a claim's timeline, and the copy those pages share. Pure (only type imports and
 * lib/explorer/ledger.ts), so `node --test` runs it directly (lib/creators.test.mjs).
 *
 * Copy that waits on a decision lives here, in one place each: CONTRIBUTION_NOTES (waiting on D7, the Hunts design).
 */
import { DROPS_PER_FANG, parseDrops } from "./explorer/ledger.ts";
import type { ClaimView, TxDetail } from "./explorer/types";

// ---------------------------------------------------------------------------------------------
// Memos (CRE-8). Anyone can put a permanent message on a creator's page for a few drops, so a
// memo on a tiny payment stays folded until someone asks for it, a memo that reads like a contact
// (a link, a renewal, "support", a refund) says Blockula never writes to anyone that way, and no
// memo is ever a link.

/** A payment under 0.01 FANG folds its memo. */
export const TINY_PAYMENT_DROPS = DROPS_PER_FANG / 100n;
/** So does one under this many times the fee it paid. */
export const TINY_FEE_MULTIPLE = 10n;

/** Links and web addresses, written any of the usual ways. `.boo` names aren't counted: memos name accounts all the time. */
const URLISH = /(?:\b(?:https?|ftp):\/\/|\bwww\.|\b[a-z0-9-]+\.(?:com|net|org|io|xyz|app|dev|me|co|info|link|site|online|live|gg|ly|to|ai|cc|top|click|page|cloud|support)\b)/i;
/** Words a fake "Blockula support" message leans on. */
const CONTACT_WORDS = /\b(?:renew|support|refund|expir|verif|recover|urgent|helpdesk)/i;

/**
 * Whether a payment is small enough that its memo folds: under TINY_PAYMENT_DROPS, or under
 * TINY_FEE_MULTIPLE × its fee. A transaction without an amount isn't a payment, so it isn't a tiny
 * one: a kind that names an account without paying it, like a name offer, takes more than dust
 * to send (a name to offer). A contact-looking memo on one still carries MEMO_WARNING.
 */
export function tinyPayment(t: Pick<TxDetail, "amount" | "fee" | "max_fee">): boolean {
  if (t.amount == null) return false;
  const amount = parseDrops(t.amount);
  const fee = parseDrops(t.fee ?? t.max_fee);
  return amount < TINY_PAYMENT_DROPS || amount < TINY_FEE_MULTIPLE * fee;
}

/** Whether a memo reads like someone getting in touch: a link, or renew/support/refund wording. */
export function memoLooksLikeContact(memo: string): boolean {
  return URLISH.test(memo) || CONTACT_WORDS.test(memo);
}

export interface MemoView {
  text: string;
  /** Folded until the reader asks: a tiny payment from someone else. */
  folded: boolean;
  /** Carries MEMO_WARNING. */
  warn: boolean;
}

/** What a ledger row shows of a transaction's memo. The viewer's own memos are theirs: never folded or flagged. */
export function memoView(t: Pick<TxDetail, "memo" | "amount" | "fee" | "max_fee" | "from">, viewer?: string | null): MemoView | null {
  if (!t.memo) return null;
  const own = !!viewer && t.from?.address === viewer;
  return { text: t.memo, folded: !own && tinyPayment(t), warn: !own && memoLooksLikeContact(t.memo) };
}

export const MEMO_FOLDED = "memo hidden (tiny payment)";
export const MEMO_WARNING = "Blockula never contacts you in a memo";

/** A memo as a "You received" notice shows it (components/explorer/Alerts.tsx): folded and flagged as the ledger does, or nothing. */
export function alertMemo(t: Pick<TxDetail, "memo" | "amount" | "fee" | "max_fee" | "from">, viewer?: string | null): string | undefined {
  const m = memoView(t, viewer);
  if (!m) return undefined;
  if (m.folded) return MEMO_FOLDED;
  return m.warn ? `“${m.text}” · ${MEMO_WARNING}` : `“${m.text}”`;
}
/** Receive (CRE-7). */
export const PUBLIC_MEMOS = "Payments and memos are public and permanent; anyone can send you one.";

// ---------------------------------------------------------------------------------------------
// GitHub links (CONTRIB-12). Linking a handle on chain checks nothing, so a handle is "unverified"
// until the node's off-chain check (`/api/github/{handle}`) finds a public gist by that user
// holding the account's address.

/** A GitHub handle as the chain accepts one (node/src/types.rs `check_github_handle`). */
export const GITHUB_HANDLE = /^[A-Za-z0-9](?:[A-Za-z0-9]|-(?=[A-Za-z0-9])){0,38}$/;

export interface GithubCheck {
  verified: boolean;
  reason: string;
}

/**
 * The node's answer to `/api/github/{handle}` for `account`, read strictly: verified only when it
 * says `verified: true` for this handle and this account. Anything else is unverified, with why.
 */
export function readGithubCheck(body: unknown, handle: string, account: string): GithubCheck {
  const b = (body && typeof body === "object" ? body : {}) as { verified?: unknown; reason?: unknown; handle?: unknown; account?: { address?: unknown } | null };
  const reason = typeof b.reason === "string" && b.reason ? b.reason : "the node didn't say why";
  if (typeof b.handle === "string" && b.handle.toLowerCase() !== handle.toLowerCase()) return { verified: false, reason: "the node answered for another handle" };
  if (b.account?.address !== undefined && b.account.address !== account) return { verified: false, reason: "the node's check is for another account" };
  return { verified: b.verified === true, reason };
}

// ---------------------------------------------------------------------------------------------
// Multisig accounts (CRE-3): owner keys only propose and approve, and an action runs once
// `threshold` of them approved it. Session keys keep sending within their limits.

/** "2-of-3 multisig". */
export const multisigShape = (threshold: number, owners: number) => `${threshold}-of-${owners} multisig`;

/** Who must approve: "Any 2 of these 3 owners must approve every action". */
export function multisigRule(threshold: number, owners: number): string {
  if (threshold >= owners) return owners === 2 ? "Both owners must approve every action" : `All ${owners} owners must approve every action`;
  if (threshold === 1) return `Any one of these ${owners} owners can approve an action alone`;
  return `Any ${threshold} of these ${owners} owners must approve every action`;
}

/** The CLI command that approves exactly what was shown (`blockula multisig approve … --digest`). */
export const approveCommand = (id: number, account: string, digest: string) => `blockula multisig approve ${id} --from ${account} --digest ${digest}`;

/** programs.ts and the account page say it the same way (CRE-3). */
export const MULTISIG_APPROVALS =
  "Owners approve with the CLI (`blockula multisig approve`), which signs with a key file. Passkey owners can't approve in the wallet yet, so a multisig whose owners are passkeys can't approve anything for now.";

// ---------------------------------------------------------------------------------------------
// Contribution pages (CONTRIB-2, CONTRIB-4, CONTRIB-16).

/**
 * What Hunts and the contribution pool mean on each network.
 *
 * WAITING ON YOUR DECISION (D7): the Hunts design. The maintainer changed D7 on 2026-10-21:
 * contributors should be able to earn FANG on blockula-1 through Hunts, in a way that can't be
 * gamed and is legally clear. Its design isn't final, so until it is, `mainnet` says nothing about
 * where Hunts and the pool run or what they pay, and it's a draft for counsel review (D4). This is
 * the one place the text lives: the pages, programs.ts and the specs all read it from here, so
 * settling D7 changes this text and nothing else (lib/creators.test.mjs checks what any wording
 * must keep).
 */
export const CONTRIBUTION_NOTES = {
  testnet: "Testnet FANG, Hunts and Soul have no value.",
  mainnet:
    "How Hunts and the contribution pool work on mainnet is still being designed. Mainnet FANG is experimental and may be worth nothing, and nothing here promises income. You're responsible for any taxes. Provisional, under counsel review.",
} as const;

/** Both, for a page that can't know its network when it's rendered (programs.ts blurbs). */
export const CONTRIBUTION_NOTES_BOTH = `On the testnet: ${CONTRIBUTION_NOTES.testnet} On mainnet: ${CONTRIBUTION_NOTES.mainnet}`;

/** What happens to a claim's bond (CONTRIB-4): it isn't simply "refundable". */
export const BOND_TERMS = "returned unless validators refute the claim or the jury marks it spam";

/** The contribution pool's timings (`/api/contributions` `rules`), as far as a claim's timeline needs them. */
export interface PocRules {
  claim_maturity_blocks?: number;
  vesting_blocks?: number;
  seance_blocks?: number;
}

export interface TimelineStep {
  label: string;
  /** When, in words: a block and roughly when it is, or how long after the step before. */
  when: string;
  state: "done" | "now" | "later" | "skipped";
  /** The Séance it's in, for a link. */
  seance?: number;
}

/** A rough calendar day `blocks` from now, at `blockMs` a block: "Oct 21, 2027". */
export function approxDate(blocks: number, blockMs = 1000, nowMs = Date.now()): string {
  return new Date(nowMs + blocks * blockMs).toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric", timeZone: "UTC" });
}

/** "4 days", "2 h", "day": a rough duration of `blocks`. (lib/format's, kept here so this module stays pure.) */
export function duration(blocks: number, blockMs = 1000): string {
  const s = (blocks * blockMs) / 1000;
  if (s < 90) return `${Math.round(s)}s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  if (s < 86400) return `${Math.round(s / 3600)} h`;
  const d = Math.round(s / 86400);
  return d === 1 ? "a day" : `${d} days`;
}

/** "block #1,234 (in about 3 days, around Oct 21, 2027)", or just the block once it's past. */
export function atBlock(height: number, head: number | undefined, blockMs = 1000, nowMs = Date.now()): string {
  const b = `block #${height.toLocaleString("en-US")}`;
  if (head == null || height <= head) return b;
  return `${b} (in about ${duration(height - head, blockMs)}, around ${approxDate(height - head, blockMs, nowMs)})`;
}

type ClaimLike = Pick<ClaimView, "status" | "submitted_at" | "verified_at" | "eligible_at" | "expires_at" | "seance" | "payout" | "closed_at">;

/**
 * A claim's way to a reward (CONTRIB-16): submitted, its merge checked by validators, eligible
 * once the revert window is over, judged in a Séance, and its reward fully vested. Times come from
 * the claim and the pool's rules; steps that can't happen any more say so.
 */
export function claimTimeline(c: ClaimLike, rules: PocRules | null | undefined, head: number | undefined, blockMs = 1000, nowMs = Date.now()): TimelineStep[] {
  const at = (h: number) => atBlock(h, head, blockMs, nowMs);
  const later = (blocks: number | undefined, after: string) => (blocks != null ? `${blocks ? `about ${duration(blocks, blockMs)}` : "right"} after ${after}` : `after ${after}`);
  const closed = c.status === "rejected" || c.status === "spam";
  const steps: TimelineStep[] = [{ label: "Submitted", when: at(c.submitted_at), state: "done" }];

  const verified = c.verified_at != null;
  steps.push(
    verified
      ? { label: "Merge checked by validators", when: at(c.verified_at as number), state: "done" }
      : c.status === "rejected"
        ? { label: "Merge checked by validators", when: "not confirmed: see why above", state: "skipped" }
        : { label: "Merge checked by validators", when: c.expires_at != null ? `waiting, until ${at(c.expires_at)}; if they don't settle it by then, the bond comes back` : "waiting", state: "now" },
  );

  const eligible = c.eligible_at ?? (verified && rules?.claim_maturity_blocks != null ? (c.verified_at as number) + rules.claim_maturity_blocks : null);
  steps.push(
    closed && !c.seance
      ? { label: "Eligible for a Séance", when: "no longer", state: "skipped" }
      : eligible != null
        ? { label: "Eligible for a Séance", when: `after the revert window, ${at(eligible)}`, state: head != null && head >= eligible ? "done" : "later" }
        : { label: "Eligible for a Séance", when: later(rules?.claim_maturity_blocks, "the merge is checked (the revert window)"), state: "later" },
  );

  const judged = c.status === "scored" || c.status === "spam";
  steps.push(
    c.seance != null
      ? { label: "Séance", when: judged ? (c.closed_at != null ? `judged, closed at ${at(c.closed_at)}` : "judged") : "being judged now", state: judged ? "done" : "now", seance: c.seance }
      : closed
        ? { label: "Séance", when: "not judged", state: "skipped" }
        : { label: "Séance", when: rules?.seance_blocks ? `the first one after it's eligible (one opens about every ${duration(rules.seance_blocks, blockMs)})` : "the first one after it's eligible", state: "later" },
  );

  const paid = parseDrops(c.payout) > 0n;
  if (closed) steps.push({ label: "Reward fully vested", when: "no reward", state: "skipped" });
  else if (c.status === "scored" && !paid) steps.push({ label: "Reward fully vested", when: "no reward: it scored below the threshold", state: "skipped" });
  else if (c.status === "scored" && c.closed_at != null && rules?.vesting_blocks != null) {
    const end = c.closed_at + rules.vesting_blocks;
    steps.push({ label: "Reward fully vested", when: at(end), state: head != null && head >= end ? "done" : "later" });
  } else steps.push({ label: "Reward fully vested", when: later(rules?.vesting_blocks, "its Séance pays it (it vests gradually until then, and can be clawed back)"), state: "later" });
  return steps;
}
