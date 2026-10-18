/**
 * Preflight: what a transaction will do before you sign it, and whether the chain will take it.
 * Pure, so `node --test` runs it directly.
 *
 * Effects come from the same reading as receipts (`effectsOf`), applied to the transaction as
 * built, plus any cost the page knows that the fields don't carry (a name's price, the fee for
 * opening the recipient's account). The checks mirror protocol v1's admission (node/src/chain.rs
 * `admit` and the mempool's dry run of every transaction, node/src/econ.rs `check_locks` and
 * `charge_new_accounts`, node/src/governance.rs `check_multisig_admission`), which refuse a
 * transaction outright, charging nothing. They're read from the chain's state as of now, so
 * they're a forecast: the node's answer on submit is the final word.
 */
import type { KeyView, Label } from "../types";
import { DROPS_PER_FANG, effectsOf, formatDrops, type Change, type Effects, type Holding } from "./ledger.ts";
import type { AccountDetail, HuntView, MultisigView, NameDetail, TxDetail, ValidatorsDetail } from "./types";

/** node/src/types.rs: MIN_NEW_ACCOUNT, MIN_HUNT, MIN_HUNT_FUNDING; poc.rs: VOUCH_BOND, CLAIM_BOND. */
export const MIN_NEW_ACCOUNT = DROPS_PER_FANG / 100n;
export const MIN_HUNT = 10n * DROPS_PER_FANG;
export const MIN_HUNT_FUNDING = DROPS_PER_FANG;
export const VOUCH_BOND = 10n * DROPS_PER_FANG;
export const CLAIM_BOND = DROPS_PER_FANG;

/** node/src/types.rs `name_price` at genesis: shorter names cost more. (The node's `/api/name` price is the live one.) */
export function namePrice(name: string): bigint {
  const n = name.toLowerCase().replace(/\.boo$/, "");
  return n.length === 3 ? 100n * DROPS_PER_FANG : n.length === 4 ? 25n * DROPS_PER_FANG : 5n * DROPS_PER_FANG;
}

/**
 * What a pending transaction will take from the sender's balance when the block applies it before
 * this one (protocol v1 dry-runs the sender's queue in nonce order): its fee plus whatever its
 * action takes out. A renewal's price and a first payment's creation fee aren't in the fields.
 */
export function txDebit(t: Pick<TxDetail, "fee" | "action">): bigint {
  const a = t.action ?? { type: "" };
  const taken = (() => {
    switch (a.type) {
      case "transfer":
      case "create_hunt":
      case "fund_hunt":
      case "fund_pool":
      case "delegate":
      case "shield":
        return big(a.amount);
      case "propose":
        return big(a.deposit);
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
  /** Of `fee`, the tip to the proposer (protocol v1: it must come from transferable FANG). */
  tip?: string | number | bigint;
  nonce: number;
  action: { type: string; [k: string]: unknown };
}

export interface Sender {
  label: Label;
  balance: string;
  /** Protocol v1 nodes: what it can move out now (vesting and vote locks hold the rest, net of stake). */
  transferable?: string;
  /** Protocol v1 nodes: what its locks hold, and why (for the refusal's words). */
  locked?: string;
  lockReason?: string;
  keys: KeyView[];
  /** The account's latest transactions (its pending ones come first). */
  recent?: TxDetail[];
  /** Key ids registered as Familiars (agent keys that may also claim PRs). */
  familiars?: string[];
  /** Set when the account is a multisig (`/api/multisig`): its owner keys can only propose and approve. */
  multisig?: { threshold: number; owners: number } | null;
}

export interface PreflightInput {
  tx: Built;
  sender: Sender;
  /** The key that signed it (this device's). */
  signer?: string;
  /** The chain's height now; the transaction lands in a later block. */
  head: number;
  /**
   * Who a transfer (or a hunt's award) pays, whether the chain has an account for it yet, and
   * what opening one burns from the sender (`/api/account` `creation_fee`, else `/api/fees`
   * `account_creation_fee` for a new one).
   */
  recipient?: { label: Label; isNew: boolean; creationFee?: bigint } | null;
  /** A cost the transaction's fields don't carry, known to the page (a name's price, a claim bond). */
  extra?: { drops: bigint; to: Label | Holding; why: string } | null;
  /** The hunt it funds, awards or cancels (`missing` when the node has no such hunt). */
  hunt?: { id: number; status: string; expires_at: number; creator?: string } | { id: number; missing: true } | null;
  /** Dev and test chains' admins (`/api/validators` `admins`): they may cancel any hunt. */
  admins?: string[];
  /** The name a `transfer_name` moves: who owns it and who it's offered to, now. */
  name?: { name: string; owner?: string; offeredTo?: string; status?: string } | null;
  /** Which of a kind's forms the user opened (an ActionSpec id: `accept_name`, `withdraw_name_offer`). */
  intent?: string;
}

export interface Budget {
  label: string;
  spentAfter: bigint;
  limit: bigint;
  periodBlocks: number;
}

export interface Preflight {
  effects: Effects;
  /** What leaves the sender in total (fee and any creation fee included). */
  cost: bigint;
  /** What the sender can spend: what it can move (its balance, less locks) minus what its pending transactions will take. */
  available: bigint;
  balanceAfter: bigint;
  /** Drops reserved by the sender's pending transactions. */
  pending: bigint;
  /** Drops its locks hold out of reach (vesting, votes), when the node reports them. */
  locked: bigint;
  /** An agent key's spending this period, after this. */
  budget: Budget | null;
  /** Reasons the chain will refuse it. */
  refusals: string[];
  /** Worth knowing, not blocking. */
  notes: string[];
  /** What it will do, when that depends on the chain's state rather than the form (a name sent to oneself). */
  headline?: string;
}

const big = (v: unknown) => (typeof v === "bigint" ? v : typeof v === "number" ? BigInt(v) : typeof v === "string" && /^\d+$/.test(v) ? BigInt(v) : 0n);

/** The amount the node reports for each kind (`tx_json` in node/src/api.rs). */
function amountOf(a: Built["action"]): bigint {
  switch (a.type) {
    case "transfer":
    case "create_hunt":
    case "fund_hunt":
    case "fund_pool":
    case "delegate":
    case "vote":
    case "claim_distribution":
    case "shield":
      return big(a.amount);
    case "propose":
      return big(a.deposit);
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
  const tip = big(tx.tip);
  const amount = amountOf(tx.action);
  const at = i.head + 1;
  const opening = (kind === "transfer" || kind === "award_hunt") && i.recipient?.isNew ? (i.recipient.creationFee ?? 0n) : 0n;
  // The transaction as the node will report it once it's in a block.
  const shaped: TxDetail = {
    id: "preflight",
    summary: "",
    kind,
    status: "pending",
    height: null,
    from: sender.label,
    to: kind === "transfer" || kind === "delegate" ? (i.recipient?.label ?? null) : kind === "fund_pool" ? (POOL as unknown as Label) : null,
    amount: amount > 0n ? amount.toString() : null,
    fee: fee.toString(),
    max_fee: fee.toString(),
    tip: tip.toString(),
    nonce: tx.nonce,
    action: tx.action,
  };
  const fx = effectsOf(shaped, { creationFee: kind === "transfer" ? opening : undefined });
  let changes: Change[] = fx.changes;
  let unseen = fx.unseen;
  let burned = fx.burned;
  if (i.extra && i.extra.drops > 0n) {
    const extra = i.extra;
    changes = changes.map((c) => (c.who.address === sender.label.address ? { ...c, delta: c.delta - extra.drops, why: `${c.why} + ${extra.why}` } : c));
    changes.push({ who: extra.to, delta: extra.drops, why: extra.why });
    unseen = null;
  }
  // A hunt's award to a new account: the creator burns its creation fee (the bounty comes from escrow).
  if (kind === "award_hunt" && opening > 0n) {
    changes = changes.map((c) => (c.who.address === sender.label.address ? { ...c, delta: c.delta - opening, why: `${c.why} + account-creation fee` } : c));
    burned += opening;
  }
  const effects: Effects = { ...fx, changes, unseen, burned };
  const mine = changes.find((c) => c.who.address === sender.label.address);
  const cost = mine && mine.delta < 0n ? -mine.delta : fee;

  // Pending transactions from this account reserve what they'll take.
  const queued = (sender.recent ?? []).filter((t) => t.status === "pending" && t.from?.address === sender.label.address);
  const pending = queued.reduce((sum, t) => sum + txDebit(t), 0n);
  const balance = big(sender.balance);
  // What it can move: v1 nodes report it (locks hold the rest); older ones, the balance.
  const movable = sender.transferable != null ? big(sender.transferable) : balance;
  const locked = sender.transferable != null && balance > movable ? balance - movable : 0n;
  const available = movable > pending ? movable - pending : 0n;
  const spendable = balance > pending ? balance - pending : 0n;

  const refusals: string[] = [];
  const notes: string[] = [];
  const key = i.signer ? sender.keys.find((k) => k.key_id === i.signer) : undefined;
  if (i.signer && sender.keys.length && !key) refusals.push("This device's key isn't on the account any more.");

  // A multisig account's owner keys act only through proposals and approvals (SF-11).
  if (sender.multisig && key?.role.type === "owner" && kind !== "multisig_propose" && kind !== "multisig_approve")
    refusals.push(`This account is a ${sender.multisig.threshold}-of-${sender.multisig.owners} multisig: its owner keys can only propose an action (multisig_propose) and approve one.`);

  // Locked tokens may pay the burned base fee, never the tip, and nothing they hold may leave:
  // the fee is paid first, then whatever the action takes out must fit in what's left to move
  // (node/src/econ.rs `check_locks`). Staking locked tokens is fine (delegate keeps them held).
  const locks = sender.lockReason ? ` (${sender.lockReason})` : "";
  const moves = cost > fee ? cost - fee : 0n;
  if (spendable < fee) refusals.push(`Not enough FANG for the fee: it needs ${formatDrops(fee)}${pending ? " after your pending transactions" : ""}, and ${formatDrops(spendable)} is free.`);
  else if (spendable < cost) refusals.push(`Not enough FANG: this needs ${formatDrops(cost)}, and ${pending ? `${formatDrops(spendable)} is free after your pending transactions` : `the balance is ${formatDrops(spendable)}`}.`);
  else if (locked > 0n && kind !== "delegate" && moves > 0n && available < cost)
    refusals.push(`${formatDrops(locked)} of this account is locked${locks}: locked FANG can be staked, not moved out, and this takes ${formatDrops(cost)} while ${formatDrops(available)} can move now.`);
  else if (locked > 0n && tip > available) refusals.push(`The tip must come from FANG that can move: ${formatDrops(available)} can, and ${formatDrops(locked)} is locked${locks}.`);

  let budget: Budget | null = null;
  if (key?.role.type === "session") {
    const r = key.role;
    const limit = big(r.limit);
    const familiar = !!i.signer && (sender.familiars ?? []).includes(i.signer);
    // The chain counts the amount moved, the fee for opening the recipient's account, and the
    // charge against the limit (a Familiar's claim counts its bond), and blocks apply this key's
    // pending payments first (node/src/chain.rs `admit`, `charge_new_accounts`).
    const costOf = (t: Pick<TxDetail, "fee" | "action" | "kind">, bond: bigint, opens: bigint) => big(t.fee) + ((t.kind ?? t.action?.type) === "transfer" ? big(t.action?.amount) + opens : (t.kind ?? t.action?.type) === "submit_claim" ? bond : 0n);
    const queuedHere = queued.filter((t) => t.signer_key === i.signer).reduce((sum, t) => sum + costOf(t, CLAIM_BOND, 0n), 0n);
    const spent = spentIn(key, at) + queuedHere;
    const after = spent + costOf({ fee: fee.toString(), action: tx.action, kind }, i.extra?.drops ?? CLAIM_BOND, opening);
    budget = { label: key.label, spentAfter: after, limit, periodBlocks: r.period_blocks };
    if (kind !== "transfer" && !(kind === "submit_claim" && familiar))
      refusals.push(kind === "submit_claim" ? `“${key.label}” is an agent key but not a registered Familiar, so it can't claim PRs for this account.` : `“${key.label}” is an agent key, and agent keys can only send transfers.`);
    if (at > r.expires_at) refusals.push(`The agent key “${key.label}” expired at block #${r.expires_at.toLocaleString("en-US")}.`);
    else if (after > limit)
      refusals.push(`Over the agent key's budget: “${key.label}” has used ${formatDrops(spent)} of ${formatDrops(limit)} this period${queuedHere ? " (counting its pending payments)" : ""}, and this needs ${formatDrops(after - spent)} more${opening ? `, opening the recipient's account (${formatDrops(opening)}) included` : ""}.`);
  }

  if (kind === "transfer") {
    if (i.recipient?.isNew && amount < MIN_NEW_ACCOUNT) refusals.push(`${who(i.recipient.label)} is new to the chain, and a first payment must be at least ${formatDrops(MIN_NEW_ACCOUNT)}.`);
    else if (i.recipient?.isNew) notes.push(`${who(i.recipient.label)} hasn't been seen on the chain before. Check it's the right address.${opening ? ` Opening its account burns ${formatDrops(opening)} from yours, on top of the payment.` : ""}`);
  }
  if (kind === "award_hunt" && opening > 0n) notes.push(`${who((i.recipient as { label: Label }).label)} has no account yet: opening one burns ${formatDrops(opening)} from yours.`);
  if (kind === "create_hunt" && amount < MIN_HUNT) refusals.push(`A hunt needs at least ${formatDrops(MIN_HUNT)}.`);
  const h = i.hunt;
  if (kind === "fund_hunt" || kind === "award_hunt" || kind === "cancel_hunt") {
    if (h && "missing" in h) refusals.push(`There's no hunt #${h.id}.`);
    else if (h && h.status !== "open" && h.status !== "expired") refusals.push(`Hunt #${h.id} is ${h.status} already.`);
  }
  if (kind === "fund_hunt") {
    if (amount < MIN_HUNT_FUNDING) refusals.push(`Fund a hunt with at least ${formatDrops(MIN_HUNT_FUNDING)}.`);
    if (h && !("missing" in h) && (h.status === "open" || h.status === "expired") && at > h.expires_at) refusals.push(`Hunt #${h.id} expired at block #${h.expires_at.toLocaleString("en-US")}.`);
  }
  if (h && !("missing" in h) && (h.status === "open" || h.status === "expired") && h.creator) {
    const admin = (i.admins ?? []).includes(sender.label.address);
    // GOV-2: only the creator awards. The creator cancels only after the expiry (dev and test
    // chains' admins any time); before that only a governance RefundHunt refunds it.
    if (kind === "award_hunt" && h.creator !== sender.label.address) refusals.push(`Only hunt #${h.id}'s creator can award it.`);
    if (kind === "cancel_hunt" && !admin) {
      if (h.creator !== sender.label.address) refusals.push(`Only hunt #${h.id}'s creator can cancel it (after it expires), or a governance refund.`);
      else if (at <= h.expires_at) refusals.push(`Hunt #${h.id} can be cancelled by its creator after block #${h.expires_at.toLocaleString("en-US")}; until then, award it or ask governance for a refund (RefundHunt).`);
    }
  }

  if (sender.transferable == null && moves > 0n && kind !== "delegate") notes.push("This node doesn't report locked FANG, so vote and vesting locks weren't checked.");

  // transfer_name to oneself accepts a name offered to this account, or withdraws an offer of one
  // it owns: the chain tells them apart by the name's state when it runs, not by which form was
  // opened (audit review-main13-2: a "withdraw" link could make someone accept an offered name).
  let headline: string | undefined;
  const nm = i.name;
  if (kind === "transfer_name" && nm) {
    const me = sender.label.address;
    // Names stay signed as names without witnesses. Their current resolved address is only
    // forecast evidence: sending to one's own name accepts or withdraws just like one's address.
    const toSelf = tx.action.to === me || i.recipient?.label.address === me;
    const expired = nm.status === "grace" || nm.status === "available";
    if (toSelf) {
      const outcome = nm.owner !== me && nm.offeredTo === me ? "accept" : nm.owner === me ? "withdraw" : null;
      if (!outcome) refusals.push(`${nm.name} isn't this account's, and it isn't offered to it.`);
      else {
        headline = outcome === "accept" ? `Accept ${nm.name}: it becomes this account's name` : nm.offeredTo ? `Withdraw your offer of ${nm.name}` : `No open offer of ${nm.name} to withdraw`;
        if (outcome === "accept" && expired) refusals.push(`${nm.name} has expired: only its owner can renew it, so it can't be accepted.`);
        if (outcome === "withdraw" && expired) refusals.push(`${nm.name} has expired: renew it first.`);
        if (outcome === "withdraw" && !nm.offeredTo) refusals.push(`${nm.name} has no open offer to withdraw.`);
        if (i.intent === "withdraw_name_offer" && outcome === "accept") refusals.push(`${nm.name} is offered to this account, so this would accept it, not withdraw an offer. To take it, use “Accept a name”.`);
        if (i.intent === "accept_name" && outcome === "withdraw") refusals.push(`This account owns ${nm.name}, so this would withdraw its offer, not accept one.`);
      }
    } else if (nm.owner !== me) refusals.push(`${nm.name} isn't this account's to offer.`);
    else if (expired) refusals.push(`${nm.name} has expired: renew it first.`);
  }

  const balanceAfter = spendable > cost ? spendable - cost : 0n;
  return { effects, cost, available, balanceAfter, pending, locked, budget, refusals, notes, ...(headline ? { headline } : {}) };
}

const who = (l: Label) => (l.name && !/\s/.test(l.name) ? l.name : l.short);

/** `/api/fees`, for the account-creation fee on nodes whose `/api/account` doesn't report it. */
export interface Fees {
  account_creation_fee?: string;
}

/** What `checkTx` reads from the node for one transaction, as the node answered (node/src/api.rs). */
export interface Reads {
  status: { height: number; block_time_ms?: number };
  /** The sender's `/api/account`. */
  account: AccountDetail;
  /** The sender's `/api/multisig` (null: not a multisig, a 404). */
  multisig?: MultisigView | null;
  /** A transfer's or a hunt award's recipient: its `/api/account`, by what the action names. */
  recipient?: AccountDetail | null;
  fees?: Fees | null;
  /** The hunt it funds, awards or cancels (`missing`: a 404). */
  hunt?: HuntView | { id: number; missing: true } | null;
  /** A name it registers, renews or transfers. */
  name?: NameDetail | null;
  /** `/api/validators`, for a hunt's cancel (dev and test chains' admins). */
  validators?: ValidatorsDetail | null;
}

const treasury: Holding = { address: "holding:treasury", name: "Treasury", short: "Treasury", holding: true };
const claimBond: Holding = { address: "holding:claim-bond", name: "Claim bond", short: "Claim bond", holding: true };

/**
 * The preflight's input from the node's answers. A recipient's account says whether the chain has
 * it yet and what opening it burns from the payer (protocol v1: `exists`, `creation_fee`; older
 * nodes answer "balance 0, nonce 0" for a missing account, and an existing account almost never
 * looks like that). The sender's account says what it can move (`transferable`) and why the rest
 * is locked (its biggest lock), and costs the fields don't carry come from the reads: a name's
 * price or renewal, the bond the account's next claim holds.
 */
export function preflightInput(tx: Built, signer: string, r: Reads, intent?: string): PreflightInput {
  const action = tx.action;
  const mine = r.account;
  const recipient = r.recipient
    ? (() => {
        const a = r.recipient;
        const isNew = a.exists != null ? !a.exists : a.balance === "0" && a.nonce === 0;
        const creationFee = a.creation_fee != null ? big(a.creation_fee) : isNew ? big(r.fees?.account_creation_fee ?? "0") : 0n;
        return { label: a.account, isNew, creationFee };
      })()
    : null;
  const name = r.name;
  const bond = action.type === "submit_claim" ? mine.contribution?.next_claim_bond : undefined;
  const years = BigInt(Number.isSafeInteger(action.years) ? (action.years as number) : 1);
  const extra =
    name && action.type === "register_name"
      ? { drops: big(name.price), to: treasury, why: `${name.name}'s price` }
      : name && action.type === "renew_name" && name.renewal_price
        ? { drops: big(name.renewal_price) * years, to: treasury, why: `renewing ${name.name} for ${years} year${years === 1n ? "" : "s"}` }
        : bond
          ? { drops: big(bond), to: claimBond, why: "refundable claim bond" }
          : null;
  const lock = [...(mine.locks ?? [])].sort((a, b) => (big(b.amount) > big(a.amount) ? 1 : big(b.amount) < big(a.amount) ? -1 : 0))[0];
  const h = r.hunt;
  return {
    tx,
    sender: {
      label: mine.account,
      balance: mine.balance,
      transferable: mine.transferable,
      locked: mine.locked,
      lockReason: lock ? `${lock.reason}${lock.vesting ? "" : ` until block #${lock.until.toLocaleString("en-US")}`}` : undefined,
      keys: mine.keys,
      recent: mine.recent_txs,
      familiars: (mine.familiars ?? []).map((f) => f.key_id),
      multisig: r.multisig ? { threshold: r.multisig.threshold, owners: r.multisig.owners.length } : null,
    },
    signer,
    head: r.status.height,
    recipient,
    extra,
    hunt: h && "missing" in h ? h : h ? { id: h.id, status: h.status, expires_at: h.expires_at, creator: h.creator.address } : null,
    admins: r.validators?.admins?.map((a) => a.address),
    name: name && action.type === "transfer_name" ? { name: name.name, owner: name.owner?.address, offeredTo: name.offered_to?.address, status: name.status } : null,
    intent,
  };
}
