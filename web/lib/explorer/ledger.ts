/**
 * Reading transactions as money: who gained and lost what, from whose side, through which key.
 * Pure and dependency-free (only type imports), so `node --test` can run it directly.
 *
 * Effects are only what a transaction's own fields prove, read the way protocol v1 runs it
 * (node/src/api.rs `tx_json`, node/src/econ.rs): `fee` is what the transaction was charged, its
 * block's base fee for its size (burned) plus its `tip` (paid to the block's proposer); `amount`
 * is what it moved, locked or claimed, by kind; and a sealed transaction's `to` is who its action
 * named when it ran (`to_name`: the `.boo` name it was signed to). Anything else it moves (a
 * name's price, a claim bond, a bounty paid out of escrow, an account-creation fee) is reported
 * as `unseen` and never guessed.
 */
import type { Label } from "../types";
import type { TxDetail } from "./types";

export const DROPS_PER_FANG = 1_000_000_000n;

/** Not an account: where escrowed FANG sits while a program holds it. */
export interface Holding {
  address: string;
  name: string;
  short: string;
  /** Marks a program's holding rather than an account on the chain. */
  holding: true;
}

export interface Change {
  who: Label | Holding;
  /** Signed drops. */
  delta: bigint;
  why: string;
}

export interface Effects {
  changes: Change[];
  /** Drops destroyed: the base fee (the charge less the tip), and any account-creation fee known. */
  burned: bigint;
  /** Effects this transaction has that its fields don't show, in plain words; null if none. */
  unseen: string | null;
  /** Whose balance change those could make inexact: everyone's, or only the sender's (a creation fee). */
  unseenFor: "all" | "sender" | null;
  /** Worth knowing about what it did that moves no balance (a vote's lock, unbonding); null if none. */
  note: string | null;
  /** Pending: these are what will happen once it's sealed. */
  pending: boolean;
}

/** What the caller knows that a transaction's own fields don't say. */
export interface LedgerCtx {
  /** What it burns opening accounts (`account_creation_fee` per new recipient): known before signing. */
  creationFee?: bigint;
  /** The recipient already had an account before it ran, so paying it opened none. */
  recipientExisted?: boolean;
  /** The sender already had an account before it ran (a claim by a new account opens its own). */
  senderExisted?: boolean;
}

const holding = (key: string, name: string): Holding => ({ address: `holding:${key}`, name, short: name, holding: true });

const big = (s: string | null | undefined) => (s && /^\d+$/.test(s) ? BigInt(s) : 0n);

/** A bech32m address (any network's prefix), as opposed to a `.boo` name. */
const isAddress = (s: unknown): s is string => typeof s === "string" && /^t?boo1[02-9ac-hj-np-z]{40,}$/.test(s);

const labelText = (l: Label | null | undefined) => (l ? (l.name && !/\s/.test(l.name) ? l.name : l.short) : "?");

/** Where each value-moving kind puts its amount, when that isn't an account the node names. */
const ESCROW: Record<string, (tx: TxDetail) => Holding> = {
  create_hunt: () => holding("hunt", "Hunt escrow"),
  fund_hunt: (tx) => holding(`hunt:${String(tx.action?.hunt_id ?? "")}`, `Hunt #${String(tx.action?.hunt_id ?? "?")} escrow`),
  // Protocol v1 self-bonds a validator's bond into its own stake pool (node/src/staking.rs `register`).
  register_validator: () => holding("validator-bond", "Self-bond, staked in its pool"),
  vouch: () => holding("vouch-bond", "Vouch bond"),
  join_jury: () => holding("juror-bond", "Juror bond"),
  delegate: (tx) => holding(`stake:${tx.to?.address ?? String(tx.action?.validator ?? "")}`, `Stake with ${tx.to ? labelText(tx.to) : "a validator"}`),
  propose: () => holding("proposal-deposit", "Proposal deposit"),
  shield: () => holding("ghost-pool", "Ghost Mode pool"),
};

/** What else a kind moves that its fields don't carry. */
const UNSEEN: Record<string, string> = {
  register_name: "The name's price went to the treasury. The transaction doesn't carry the amount.",
  renew_name: "The renewal price went to the treasury. The transaction doesn't carry the amount.",
  award_hunt: "The hunt's escrow paid the bounty to the winner (see the hunt for the amount); if that opened the winner's account, the account-creation fee burned from the sender too.",
  cancel_hunt: "The hunt's escrow refunded every funder what they put in.",
  submit_claim: "A refundable claim bond was held from the sender.",
  withdraw_vested: "Vested contribution rewards were paid to the sender.",
  withdraw_unbonded: "Every unbonding entry that had finished was paid to the sender. The transaction doesn't carry the amount.",
  leave_jury: "The juror bond was returned to the sender.",
  unvouch: "The vouch bond was returned to the voucher (or the vouch was declined).",
  unbond: "Before protocol v1 this returned a removed validator's bond to its owner; v1 refuses it (stake leaves by undelegating).",
  clawback: "Unvested rewards were clawed back into the pool.",
  attest_claim: "If this attestation settled the claim, its bond went back to the claimant or into the pool.",
  attest_revert: "If this attestation settled it, the reverted claim's unvested reward was clawed back into the pool.",
  reject_claim: "The claim's bond was forfeited to the contribution pool.",
  flag_sybil: "The flagged account's bonds and unvested rewards went to the contribution pool, and so did the bond of whoever vouched for it.",
  report_equivocation: "The reported validator's pool was slashed (and the validator banned for good), and the reporter may have been rewarded.",
  multisig_propose: "If the proposer's approval was enough for the account's threshold, the proposed action ran too, and moved what it moves.",
  multisig_approve: "If this approval reached the account's threshold, the proposed action ran too, and moved what it moves.",
  private_transfer: "A private Ghost Mode transfer: its amounts and parties are hidden by design.",
};

/** Kinds that move nothing but their fee (and say something worth knowing). */
const NOTES: Record<string, (tx: TxDetail) => string | null> = {
  vote: (tx) => `A vote moves nothing: it locks ${formatDrops(big(tx.amount))} (at its conviction) until a while after voting ends. Locked FANG can still be staked, not sent.`,
  unlock: () => "Released the account's expired vote locks: nothing moved.",
  undelegate: () => "Started unbonding: that stake stops earning now and can be withdrawn (withdraw_unbonded) once the unbonding period is over.",
  redelegate: () => "Moved stake from one pool to another without unbonding it.",
  transfer_name: () => "Names change hands by offer and acceptance; no FANG moves.",
  release_name: () => "Gave the name up: anyone can register it now.",
  propose: () => "The deposit is refunded once the proposal reaches quorum, and burned if it doesn't.",
};

/** Kinds whose fields show everything they move (besides those above). */
const COMPLETE = new Set([
  "transfer",
  "fund_pool",
  "add_key",
  "remove_key",
  "register_familiar",
  "link_github",
  "unlink_github",
  "approve_validator",
  "remove_validator",
  "set_repo",
  "grant_soul",
  "commit_votes",
  "reveal_votes",
  "claim_distribution",
  "unshield",
  "multisig_create",
  "update_validator",
  "unjail",
  "beacon_commit",
  "beacon_reveal",
  "validator_ready",
  "admit_validator",
  "set_ghost_address",
  "ghost_pause",
  ...Object.keys(ESCROW),
  ...Object.keys(NOTES),
]);

/** A kind newer than this explorer (a later protocol version) may move FANG in ways it can't read. */
const UNKNOWN = "This explorer doesn't know this kind of transaction yet, so only its fee is shown; it may have moved more.";
const RAN_UNSEEN = "It ran in a block and failed there, so its fee was charged, but this node no longer holds that block and can't say how much.";

/** Whether this explorer knows what a kind of transaction moves. */
export const knownKind = (kind: string) => COMPLETE.has(kind) || kind in UNSEEN;

/** Amounts these kinds report that don't leave the sender (a vote's lock) or come to it (a claim). */
const NOT_SPENT = new Set(["vote", "claim_distribution"]);

/** Whether a kind's reported `amount` is money leaving the sender (for a receipt's headline sign). */
export const amountLeaves = (kind: string) => knownKind(kind) && !NOT_SPENT.has(kind) && kind !== "unshield";

const PROPOSER = holding("proposer", "The block's proposer");

/**
 * The part of the charge that tipped the block's proposer instead of burning, or null if it can't
 * be told. Protocol v1 charges exactly the including block's base fee for the size plus the tip
 * (node/src/econ.rs `check_fee` refuses a `max_fee` that doesn't cover both), so of the reported
 * `fee`, `tip` went to the proposer and the rest burned. Older nodes (no `max_fee`) burned
 * `min_fee(size)` and paid the rest to the proposer.
 */
function tipOf(tx: TxDetail, fee: bigint): bigint | null {
  if (tx.max_fee != null) {
    const tip = big(tx.tip);
    return tip < fee ? tip : fee;
  }
  if (tx.size_bytes == null) return 0n;
  const min = 10_000n + BigInt(tx.size_bytes) * 10n;
  return fee > min ? fee - min : 0n;
}

/** A node that predates `to_name` resolved a name at read time, so its `to` is the name's owner now. */
const readTimeOwner = (tx: TxDetail) => tx.status !== "pending" && tx.to_name === undefined && typeof tx.action?.to === "string" && !isAddress(tx.action.to);

export function effectsOf(tx: TxDetail, ctx: LedgerCtx = {}): Effects {
  const pending = tx.status === "pending";
  const fee = big(tx.fee);
  const kind = tx.kind ?? tx.action?.type ?? "";
  const none = (unseen: string | null = null, note: string | null = null): Effects => ({ changes: [], burned: 0n, unseen, unseenFor: unseen ? "all" : null, note, pending });
  if (!tx.from) {
    // Unsigned Ghost Mode transactions have no sender: an unshield pays out of the pool.
    if (tx.status === "finalized" && kind === "unshield" && tx.to) {
      const amount = big(tx.amount);
      return { changes: [{ who: holding("ghost-pool", "Ghost Mode pool"), delta: -(amount + fee), why: "amount + fee, from the pool" }, { who: tx.to, delta: amount, why: "received" }], burned: fee, unseen: null, unseenFor: null, note: null, pending };
    }
    if (tx.status === "finalized" && kind === "private_transfer") return none(UNSEEN.private_transfer);
    // A failure the node remembers without its transaction: one that ran in a block (`dropped:
    // false`) was charged its fee; one dropped from the mempool unrun changed nothing.
    return none(tx.status === "failed" && tx.dropped === false ? RAN_UNSEEN : null);
  }
  const tip = tipOf(tx, fee);
  const burnedFee = fee - (tip ?? 0n);
  const tipped: Change[] = tip ? [{ who: PROPOSER, delta: tip, why: "tip, on top of the base fee that burns" }] : [];
  const split = tip == null ? "Part of the fee may have tipped the block's proposer rather than burning; the node doesn't say how much." : null;
  // Included but failed: only the fee was charged.
  if (tx.status === "failed") return { changes: [{ who: tx.from, delta: -fee, why: "fee (the action failed)" }, ...tipped], burned: burnedFee, unseen: split, unseenFor: split ? "all" : null, note: null, pending };
  // A kind newer than this explorer: the fee is all that's certain (its amount may even be
  // coming in, not going out).
  if (!knownKind(kind)) return { changes: [{ who: tx.from, delta: -fee, why: "fee" }, ...tipped], burned: burnedFee, unseen: UNKNOWN, unseenFor: "all", note: null, pending };

  const amount = big(tx.amount);
  const changes: Change[] = [];
  const unseen: (string | null)[] = [UNSEEN[kind] ?? null, split];
  // What only the sender's outflow might be missing (the recipient's gain is exact either way).
  const senderOnly: string[] = [];
  let creation = 0n;
  if (kind === "claim_distribution" && amount > 0n) {
    // The community account pays the claim to the sender (it may even pay the claim's own fee).
    const id = String(tx.action?.distribution_id ?? "?");
    changes.push({ who: tx.from, delta: amount - fee, why: "claimed, less the fee" });
    changes.push({ who: holding(`distribution:${id}`, `Community distribution #${id}`), delta: -amount, why: "paid the claim" });
    // A claim by an account that doesn't exist yet opens it, and burns the account-creation fee
    // out of the claim (node/src/chain.rs, the prepaid path). Only an account's first transaction
    // can (nonce 0), and the transaction doesn't record whether it did (audit review-main13-7).
    if (!pending && !ctx.senderExisted && (tx.nonce == null || tx.nonce === 0)) senderOnly.push(`If this claim opened ${labelText(tx.from)}'s account, the chain also burned the account-creation fee (0.1 FANG by default) out of the claim; the transaction doesn't record whether it did.`);
  } else if (amount > 0n && !NOT_SPENT.has(kind)) {
    // An escrowing kind's amount goes to its holding even when the node names a `to` (a vouch
    // names the newcomer, but the bond is held by the program, not paid to them).
    const dest = ESCROW[kind]?.(tx) ?? tx.to ?? null;
    if (dest && dest.address === tx.from.address) {
      changes.push({ who: tx.from, delta: -fee, why: "fee (paid itself)" });
    } else {
      if (kind === "transfer" && dest && !("holding" in dest)) {
        // Opening the recipient's account burns `account_creation_fee` from the sender (v1,
        // node/src/econ.rs `charge_new_accounts`). The transaction doesn't record whether it did:
        // a name's owner always has an account, and the caller may know (a preflight does).
        if (ctx.creationFee != null) creation = ctx.creationFee;
        else if (!ctx.recipientExisted && isAddress(tx.action?.to) && !pending) senderOnly.push(`If this payment opened ${labelText(dest)}'s account, the chain also burned the account-creation fee (0.1 FANG by default) from the sender; the transaction doesn't record whether it did.`);
        if (readTimeOwner(tx)) unseen.push(`It was signed to ${String(tx.action?.to)}, which the chain paid to whoever owned it when this ran; this node reports who owns it now.`);
      }
      changes.push({ who: tx.from, delta: -(amount + fee + creation), why: creation ? "amount + fee + account-creation fee" : "amount + fee" });
      if (dest) changes.push({ who: dest, delta: amount, why: kind === "transfer" || kind === "fund_pool" ? "received" : "held" });
    }
  } else {
    changes.push({ who: tx.from, delta: -fee, why: "fee" });
  }
  changes.push(...tipped);
  const all = unseen.filter(Boolean).join(" ");
  const text = [all, ...senderOnly].filter(Boolean).join(" ");
  return { changes, burned: burnedFee + creation, unseen: text || null, unseenFor: all ? "all" : senderOnly.length ? "sender" : null, note: NOTES[kind]?.(tx) ?? null, pending };
}

export type Direction = "in" | "out" | "self" | "none";

/** Which way the money went for `me`, by this transaction's visible effects. */
export function deltaFor(tx: TxDetail, me: string, ctx?: LedgerCtx): { delta: bigint; direction: Direction; exact: boolean } {
  const fx = effectsOf(tx, ctx);
  const delta = fx.changes.reduce((s, c) => (c.who.address === me ? s + c.delta : s), 0n);
  const mine = fx.changes.some((c) => c.who.address === me);
  const self = !!tx.from && (tx.kind ?? tx.action?.type) === "transfer" && tx.to?.address === tx.from.address;
  const direction: Direction = !mine ? "none" : self ? "self" : delta > 0n ? "in" : delta < 0n ? "out" : "none";
  return { delta, direction, exact: !uncertainFor(fx, tx, me) };
}

/** Whether what `effectsOf` couldn't see may change `me`'s side of it. */
export const uncertainFor = (fx: Effects, tx: TxDetail, me: string) => fx.unseenFor === "all" || (fx.unseenFor === "sender" && tx.from?.address === me);

/** The other side of a transaction, from `me`'s point of view. */
export function counterparty(tx: TxDetail, me: string | null): Label | Holding | null {
  if (!tx.from) return tx.to ?? null;
  if (me && tx.from.address !== me) return tx.from;
  const other = effectsOf(tx).changes.find((c) => c.who.address !== tx.from?.address && c.who !== PROPOSER);
  return other?.who ?? tx.to ?? null;
}

/**
 * Whether sealed transaction `t` shows that `address` had an account once it ran: it sent it (a
 * sender pays a fee, from an account), or it's a finalized payment, award or unshield to it.
 * Being named doesn't: a vouch, a name offer, Soul or a failed payment opens no account (audit
 * main13-render-2, review-main13-3).
 */
export function showsAccount(t: TxDetail, address: string): boolean {
  if (t.status === "pending" || t.height == null) return false;
  if (t.from?.address === address) return true;
  const kind = t.kind ?? t.action?.type;
  return t.status === "finalized" && t.to?.address === address && (kind === "transfer" || kind === "award_hunt" || kind === "unshield");
}

/**
 * What each transaction tells about the accounts it pays, from the list it came from: an account
 * an earlier transaction of the list shows existed ([`showsAccount`]) had one then, so paying it
 * opened none (and burned no creation fee), and its own claim opened none either.
 */
export function contextsOf(txs: TxDetail[]): (tx: TxDetail) => LedgerCtx {
  const first = new Map<string, number>();
  for (const t of txs) {
    if (t.height == null) continue;
    for (const w of [t.from, t.to]) if (w && showsAccount(t, w.address) && (first.get(w.address) ?? Infinity) > t.height) first.set(w.address, t.height);
  }
  const before = (w: Label | null | undefined, t: TxDetail) => !!w && t.height != null && (first.get(w.address) ?? Infinity) < t.height;
  return (t) => ({ recipientExisted: before(t.to, t), senderExisted: before(t.from, t) });
}

export interface Flow {
  who: Label | Holding;
  /** Drops received from them. */
  in: bigint;
  /** Drops sent to them. */
  out: bigint;
  count: number;
}

export interface Flows {
  rows: Flow[];
  /** Fees this account paid. */
  burned: bigint;
  totalIn: bigint;
  totalOut: bigint;
  /** Transactions counted, and how many also moved FANG we can't see (see `effectsOf`). */
  counted: number;
  unseen: number;
  /** Heights the window covers. */
  from: number | null;
  to: number | null;
}

/** Money in and out of `me` over `txs`, by counterparty, biggest first. Sealed transactions only. */
export function flowsOf(txs: TxDetail[], me: string): Flows {
  const by = new Map<string, Flow>();
  const ctxOf = contextsOf(txs);
  let burned = 0n, totalIn = 0n, totalOut = 0n, counted = 0, unseen = 0;
  let from: number | null = null, to: number | null = null;
  const add = (who: Label | Holding, inn: bigint, out: bigint) => {
    const row = by.get(who.address) ?? { who, in: 0n, out: 0n, count: 0 };
    row.in += inn;
    row.out += out;
    totalIn += inn;
    totalOut += out;
    by.set(who.address, row);
    return row;
  };
  for (const tx of txs) {
    if (tx.status === "pending" || tx.height == null) continue;
    const fx = effectsOf(tx, ctxOf(tx));
    const mine = fx.changes.find((c) => c.who.address === me);
    if (!mine) continue;
    counted++;
    if (uncertainFor(fx, tx, me)) unseen++;
    from = from == null ? tx.height : Math.min(from, tx.height);
    to = to == null ? tx.height : Math.max(to, tx.height);
    const touched = new Set<string>();
    if (tx.from?.address === me) {
      // Sent by me: whoever gained, I paid; whatever paid me (a distribution) came in.
      burned += fx.burned;
      for (const c of fx.changes) {
        if (c.who.address === me) continue;
        add(c.who, c.delta < 0n ? -c.delta : 0n, c.delta > 0n ? c.delta : 0n);
        touched.add(c.who.address);
      }
    } else if (tx.from) {
      // Sent by someone else: what I gained came from them (what I lost went to them).
      const d = fx.changes.filter((c) => c.who.address === me).reduce((s, c) => s + c.delta, 0n);
      add(tx.from, d > 0n ? d : 0n, d < 0n ? -d : 0n);
      touched.add(tx.from.address);
    }
    for (const a of touched) (by.get(a) as Flow).count++;
  }
  const rows = [...by.values()].sort((a, b) => cmp(b.in + b.out, a.in + a.out));
  return { rows, burned, totalIn, totalOut, counted, unseen, from, to };
}

const cmp = (a: bigint, b: bigint) => (a > b ? 1 : a < b ? -1 : 0);

/** Per signing key: how many of these transactions it signed and what it spent (what left the account). */
export function bySigner(txs: TxDetail[], account: string): Map<string, { count: number; spent: bigint; last: number | null }> {
  const out = new Map<string, { count: number; spent: bigint; last: number | null }>();
  const ctxOf = contextsOf(txs);
  for (const tx of txs) {
    if (!tx.signer_key || tx.from?.address !== account) continue;
    const s = out.get(tx.signer_key) ?? { count: 0, spent: 0n, last: null };
    s.count++;
    const d = effectsOf(tx, ctxOf(tx)).changes.filter((c) => c.who.address === account).reduce((t, c) => t + c.delta, 0n);
    if (d < 0n) s.spent += -d;
    if (tx.height != null) s.last = Math.max(s.last ?? 0, tx.height);
    out.set(tx.signer_key, s);
  }
  return out;
}

export interface Me {
  address: string;
  names: string[];
  short?: string;
}

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/**
 * Words after which a name is the thing acted on (a name being registered, renewed, offered,
 * accepted or released), not an account: "You offered castle.boo to mina.boo", never "You
 * offered you to mina.boo" (audit main13-render-6).
 */
const NAME_OBJECT = ["the name ", "renewed ", "offered ", "offer of ", "accepted ", "released ", "claimed "];

/**
 * The node's summary, from `me`'s side: "vlad.boo sent 1 FANG to mina.boo" reads
 * "You sent 1 FANG to mina.boo" for vlad and "vlad.boo sent 1 FANG to you" for mina.
 */
export function fromMySide(summary: string, me: Me | null): string {
  if (!me) return summary;
  const ids = [...me.names, me.address, me.short].filter((s): s is string => !!s).sort((a, b) => b.length - a.length);
  if (!ids.length) return summary;
  // A whole name or address, not part of a longer one, and not a name being acted on.
  const re = new RegExp(`(?<![\\w.…-])${NAME_OBJECT.map((w) => `(?<!${escapeRe(w)})`).join("")}(?:${ids.map(escapeRe).join("|")})(?![\\w…-])`, "g");
  // Leave quoted text (labels, hunt titles) as written.
  let offset = 0;
  return summary
    .split(/(“[^”]*”)/)
    .map((part, i) => {
      const start = offset;
      offset += part.length;
      return i % 2 ? part : part.replace(re, (_m, at: number) => (start + at === 0 ? "You" : "you"));
    })
    .join("");
}

// ---- Formatting drops, kept here so tests and pages agree ----

/** Exact: "1.5 FANG", "-0.00006469 FANG". */
export function formatDrops(drops: bigint, unit = "FANG"): string {
  const neg = drops < 0n;
  const d = neg ? -drops : drops;
  const whole = d / DROPS_PER_FANG;
  const frac = (d % DROPS_PER_FANG).toString().padStart(9, "0").replace(/0+$/, "");
  return `${neg ? "-" : ""}${whole.toLocaleString("en-US")}${frac ? `.${frac}` : ""}${unit ? ` ${unit}` : ""}`;
}

/**
 * For display: grouped whole part and only as many decimals as the size deserves
 * (1,099,999,999 · 12,345.67 · 0.00006469). Never rounds a nonzero amount to zero.
 */
export function compactDrops(drops: bigint): string {
  const neg = drops < 0n;
  const d = neg ? -drops : drops;
  const whole = d / DROPS_PER_FANG;
  const frac9 = (d % DROPS_PER_FANG).toString().padStart(9, "0");
  const digits = whole >= 1_000_000n ? 0 : whole >= 1000n ? 2 : whole >= 1n ? 4 : 9;
  let frac = frac9.slice(0, digits).replace(/0+$/, "");
  if (whole === 0n && !frac && d > 0n) frac = frac9.replace(/0+$/, "");
  if (whole === 0n && frac.length > 4) {
    // Small amounts: keep 4 significant digits.
    const lead = frac.match(/^0*/)?.[0].length ?? 0;
    frac = frac.slice(0, lead + 4).replace(/0+$/, "");
  }
  return `${neg ? "-" : ""}${whole.toLocaleString("en-US")}${frac ? `.${frac}` : ""}`;
}

/** For forms and links: "1000.5", never grouped ("1,000.5" wouldn't parse back). */
export function plainDrops(drops: bigint): string {
  const neg = drops < 0n;
  const d = neg ? -drops : drops;
  const frac = (d % DROPS_PER_FANG).toString().padStart(9, "0").replace(/0+$/, "");
  return `${neg ? "-" : ""}${d / DROPS_PER_FANG}${frac ? `.${frac}` : ""}`;
}

export const parseDrops = big;
