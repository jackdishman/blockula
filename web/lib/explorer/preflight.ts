/**
 * Preflight: what a transaction will do before you sign it, and whether the chain will take it.
 * Pure, so `node --test` runs it directly.
 *
 * Effects come from the same reading as receipts (`effectsOf`), applied to the transaction as
 * built, plus any cost the page knows that the fields don't carry (a name's price). The checks
 * mirror the node's admission rules (node/src/chain.rs `admit` and the mempool dry run), which
 * refuse a transaction outright, charging nothing. They're read from the chain's state as of
 * now, so they're a forecast: the node's answer on submit is the final word.
 */
import type { KeyView, Label } from "../types";
import { DROPS_PER_FANG, effectsOf, formatDrops, type Change, type Effects, type Holding } from "./ledger.ts";
import type { TxDetail } from "./types";

/** node/src/types.rs: MIN_NEW_ACCOUNT, MIN_HUNT, MIN_HUNT_FUNDING; poc.rs: VOUCH_BOND, CLAIM_BOND. */
export const MIN_NEW_ACCOUNT = DROPS_PER_FANG / 100n;
export const MIN_HUNT = 10n * DROPS_PER_FANG;
export const MIN_HUNT_FUNDING = DROPS_PER_FANG;
export const VOUCH_BOND = 10n * DROPS_PER_FANG;
export const CLAIM_BOND = DROPS_PER_FANG;

/** node/src/types.rs `name_price`: shorter names cost more. */
export function namePrice(name: string): bigint {
  const n = name.toLowerCase().replace(/\.boo$/, "");
  return n.length === 3 ? 100n * DROPS_PER_FANG : n.length === 4 ? 25n * DROPS_PER_FANG : 5n * DROPS_PER_FANG;
}

/**
 * What the node reserves from the sender's balance while a transaction waits in the mempool
 * (node/src/chain.rs `tx_debit`): its fee plus whatever its action takes out.
 */
export function txDebit(t: Pick<TxDetail, "fee" | "action">): bigint {
  const a = t.action ?? { type: "" };
  const taken = (() => {
    switch (a.type) {
      case "transfer":
      case "create_hunt":
      case "fund_hunt":
      case "fund_pool":
        return big(a.amount);
      case "register_validator":
      case "join_jury":
        return big(a.bond);
      case "register_name":
        return typeof a.name === "string" ? namePrice(a.name) : 0n;
      case "vouch":
        return VOUCH_BOND;
      case "submit_claim":
        return CLAIM_BOND;
      default:
        return 0n;
    }
  })();
  return big(t.fee) + taken;
}

export interface Built {
  from: string;
  fee: string | number | bigint;
  nonce: number;
  action: { type: string; [k: string]: unknown };
}

export interface Sender {
  label: Label;
  balance: string;
  keys: KeyView[];
  /** The account's latest transactions (its pending ones come first). */
  recent?: TxDetail[];
  /** Key ids registered as Familiars (agent keys that may also claim PRs). */
  familiars?: string[];
}

export interface PreflightInput {
  tx: Built;
  sender: Sender;
  /** The key that signed it (this device's). */
  signer?: string;
  /** The chain's height now; the transaction lands in a later block. */
  head: number;
  /** Who a transfer pays, and whether the chain has seen them before. */
  recipient?: { label: Label; isNew: boolean } | null;
  /** A cost the transaction's fields don't carry, known to the page (a name's price, a claim bond). */
  extra?: { drops: bigint; to: Label | Holding; why: string } | null;
  /** The hunt it funds, for fund_hunt (`missing` when the node has no such hunt). */
  hunt?: { id: number; status: string; expires_at: number } | { id: number; missing: true } | null;
}

export interface Budget {
  label: string;
  spentAfter: bigint;
  limit: bigint;
  periodBlocks: number;
}

export interface Preflight {
  effects: Effects;
  /** What leaves the sender in total (fee included). */
  cost: bigint;
  /** What the sender can spend: balance minus what its pending transactions will take. */
  available: bigint;
  balanceAfter: bigint;
  /** Drops reserved by the sender's pending transactions. */
  pending: bigint;
  /** An agent key's spending this period, after this. */
  budget: Budget | null;
  /** Reasons the chain will refuse it. */
  refusals: string[];
  /** Worth knowing, not blocking. */
  notes: string[];
}

const big = (v: unknown) => (typeof v === "bigint" ? v : typeof v === "number" ? BigInt(v) : typeof v === "string" && /^\d+$/.test(v) ? BigInt(v) : 0n);

/** The amount the node reports for each kind (`tx_json` in node/src/api.rs). */
function amountOf(a: Built["action"]): bigint {
  switch (a.type) {
    case "transfer":
    case "create_hunt":
    case "fund_hunt":
    case "fund_pool":
      return big(a.amount);
    case "register_validator":
    case "join_jury":
      return big(a.bond);
    case "vouch":
      return VOUCH_BOND;
    default:
      return 0n;
  }
}

const POOL: Holding = { address: "holding:pool", name: "Contribution pool", short: "Contribution pool", holding: true };

/** What a session key has spent in the window the transaction lands in (the window resets on its own). */
function spentIn(k: KeyView, height: number) {
  if (k.role.type !== "session") return 0n;
  return height >= k.window_start + k.role.period_blocks ? 0n : big(k.spent_this_period);
}

export function preflight(i: PreflightInput): Preflight {
  const { tx, sender } = i;
  const kind = tx.action.type;
  const fee = big(tx.fee);
  const amount = amountOf(tx.action);
  const at = i.head + 1;
  // The transaction as the node will report it once it's in a block.
  const shaped: TxDetail = {
    id: "preflight",
    summary: "",
    kind,
    status: "pending",
    height: null,
    from: sender.label,
    to: kind === "transfer" ? (i.recipient?.label ?? null) : kind === "fund_pool" ? (POOL as unknown as Label) : null,
    amount: amount > 0n ? amount.toString() : null,
    fee: fee.toString(),
    nonce: tx.nonce,
    action: tx.action,
  };
  const fx = effectsOf(shaped);
  let changes: Change[] = fx.changes;
  let unseen = fx.unseen;
  if (i.extra && i.extra.drops > 0n) {
    const extra = i.extra;
    changes = changes.map((c) => (c.who.address === sender.label.address ? { ...c, delta: c.delta - extra.drops, why: `${c.why} + ${extra.why}` } : c));
    changes.push({ who: extra.to, delta: extra.drops, why: extra.why });
    unseen = null;
  }
  const effects: Effects = { ...fx, changes, unseen };
  const mine = changes.find((c) => c.who.address === sender.label.address);
  const cost = mine && mine.delta < 0n ? -mine.delta : fee;

  // Pending transactions from this account reserve what they'll take.
  const queued = (sender.recent ?? []).filter((t) => t.status === "pending" && t.from?.address === sender.label.address);
  const pending = queued.reduce((sum, t) => sum + txDebit(t), 0n);
  const balance = big(sender.balance);
  const available = balance > pending ? balance - pending : 0n;

  const refusals: string[] = [];
  const notes: string[] = [];
  const key = i.signer ? sender.keys.find((k) => k.key_id === i.signer) : undefined;
  if (i.signer && sender.keys.length && !key) refusals.push("This device's key isn't on the account any more.");

  if (available < fee) refusals.push(`Not enough FANG for the fee: it needs ${formatDrops(fee)}${pending ? " after your pending transactions" : ""}, and ${formatDrops(available)} is free.`);
  else if (available < cost) refusals.push(`Not enough FANG: this needs ${formatDrops(cost)}, and ${pending ? `${formatDrops(available)} is free after your pending transactions` : `the balance is ${formatDrops(available)}`}.`);

  let budget: Budget | null = null;
  if (key?.role.type === "session") {
    const r = key.role;
    const limit = big(r.limit);
    const familiar = !!i.signer && (sender.familiars ?? []).includes(i.signer);
    // The chain counts the amount moved plus the fee against the limit (a Familiar's claim
    // counts its bond), and blocks apply this key's pending payments first.
    const cost = (t: Pick<TxDetail, "fee" | "action" | "kind">, bond: bigint) => big(t.fee) + ((t.kind ?? t.action?.type) === "transfer" ? big(t.action?.amount) : (t.kind ?? t.action?.type) === "submit_claim" ? bond : 0n);
    const queuedHere = queued.filter((t) => t.signer_key === i.signer).reduce((sum, t) => sum + cost(t, CLAIM_BOND), 0n);
    const spent = spentIn(key, at) + queuedHere;
    const after = spent + cost({ fee: fee.toString(), action: tx.action, kind }, i.extra?.drops ?? CLAIM_BOND);
    budget = { label: key.label, spentAfter: after, limit, periodBlocks: r.period_blocks };
    if (kind !== "transfer" && !(kind === "submit_claim" && familiar))
      refusals.push(kind === "submit_claim" ? `“${key.label}” is an agent key but not a registered Familiar, so it can't claim PRs for this account.` : `“${key.label}” is an agent key, and agent keys can only send transfers.`);
    if (at > r.expires_at) refusals.push(`The agent key “${key.label}” expired at block #${r.expires_at.toLocaleString("en-US")}.`);
    else if (after > limit)
      refusals.push(`Over the agent key's budget: “${key.label}” has used ${formatDrops(spent)} of ${formatDrops(limit)} this period${queuedHere ? " (counting its pending payments)" : ""}, and this needs ${formatDrops(after - spent)} more.`);
  }

  if (kind === "transfer") {
    if (i.recipient?.isNew && amount < MIN_NEW_ACCOUNT) refusals.push(`${who(i.recipient.label)} is new to the chain, and a first payment must be at least ${formatDrops(MIN_NEW_ACCOUNT)}.`);
    else if (i.recipient?.isNew) notes.push(`${who(i.recipient.label)} hasn't been seen on the chain before. Check it's the right address.`);
  }
  if (kind === "create_hunt" && amount < MIN_HUNT) refusals.push(`A hunt needs at least ${formatDrops(MIN_HUNT)}.`);
  if (kind === "fund_hunt") {
    if (amount < MIN_HUNT_FUNDING) refusals.push(`Fund a hunt with at least ${formatDrops(MIN_HUNT_FUNDING)}.`);
    const h = i.hunt;
    if (h && "missing" in h) refusals.push(`There's no hunt #${h.id}.`);
    else if (h && h.status !== "open") refusals.push(`Hunt #${h.id} is ${h.status}, so it can't take more funding.`);
    else if (h && at > h.expires_at) refusals.push(`Hunt #${h.id} expired at block #${h.expires_at.toLocaleString("en-US")}.`);
  }

  const balanceAfter = available > cost ? available - cost : 0n;
  return { effects, cost, available, balanceAfter, pending, budget, refusals, notes };
}

const who = (l: Label) => (l.name && !/\s/.test(l.name) ? l.name : l.short);
