/**
 * Reading transactions as money: who gained and lost what, from whose side, through which key.
 * Pure and dependency-free (only type imports), so `node --test` can run it directly.
 *
 * Effects are only what a transaction's own fields prove: the amount and fee it signed for, and
 * where the node says the amount went. Anything else it moves (a name's price, a claim bond, a
 * bounty paid out of escrow, refunds) is reported as `unseen` and never guessed.
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
  /** Drops destroyed (the fee). */
  burned: bigint;
  /** Effects this transaction has that its fields don't show, in plain words; null if none. */
  unseen: string | null;
  /** Pending: these are what will happen once it's sealed. */
  pending: boolean;
}

const holding = (key: string, name: string): Holding => ({ address: `holding:${key}`, name, short: name, holding: true });

/** Where each value-moving kind puts the amount, when the node doesn't name a recipient. */
const ESCROW: Record<string, (tx: TxDetail) => Holding> = {
  create_hunt: () => holding("hunt", "Hunt escrow"),
  fund_hunt: (tx) => holding(`hunt:${String(tx.action?.hunt_id ?? "")}`, `Hunt #${String(tx.action?.hunt_id ?? "?")} escrow`),
  register_validator: () => holding("validator-bond", "Validator bond"),
  vouch: () => holding("vouch-bond", "Vouch bond"),
  join_jury: () => holding("juror-bond", "Juror bond"),
};

/** What else a kind moves that its fields don't carry. */
const UNSEEN: Record<string, string> = {
  register_name: "The name's price went to the treasury. The transaction doesn't carry the amount.",
  award_hunt: "The hunt's escrow paid the bounty to the winner. See the hunt for the amount.",
  cancel_hunt: "The hunt's escrow refunded every funder what they put in.",
  submit_claim: "A refundable claim bond was held from the sender.",
  withdraw_vested: "Vested contribution rewards were paid to the sender.",
  leave_jury: "The juror bond was returned to the sender.",
  unvouch: "The vouch bond was returned to the voucher (or the vouch was declined).",
  unbond: "The validator bond was returned to its owner.",
  clawback: "Unvested rewards were clawed back into the pool.",
  attest_claim: "If this attestation settled the claim, its bond went back to the claimant or into the pool.",
  reject_claim: "The claim's bond was forfeited to the contribution pool.",
  flag_sybil: "The flagged account's bonds and unvested rewards went to the contribution pool, and so did the bond of whoever vouched for it.",
};

/** Kinds whose fields show everything they move (besides those above). */
const COMPLETE = new Set(["transfer", "fund_pool", "add_key", "remove_key", "register_familiar", "link_github", "unlink_github", "approve_validator", "remove_validator", "set_repo", "grant_soul", "commit_votes", "reveal_votes", ...Object.keys(ESCROW)]);

/** A kind newer than this explorer (a later protocol version) may move FANG in ways it can't read. */
const UNKNOWN = "This explorer doesn't know this kind of transaction yet, so only its fee is shown; it may have moved more.";

/** Whether this explorer knows what a kind of transaction moves. */
export const knownKind = (kind: string) => COMPLETE.has(kind) || kind in UNSEEN;

const big = (s: string | null | undefined) => (s && /^\d+$/.test(s) ? BigInt(s) : 0n);

const PROPOSER = holding("proposer", "The block's proposer");

/**
 * The part of a fee that tipped the block's proposer instead of burning, or null if it can't be
 * told. Today's chain burns `min_fee(size)` (node/src/types.rs) and pays the rest to the
 * proposer; later protocol versions report the `tip` and a `max_fee`, and charge
 * min(base fee + tip, max_fee), so a capped charge hides the split.
 */
function tipOf(tx: TxDetail, fee: bigint): bigint | null {
  if (tx.max_fee != null) {
    const tip = big(tx.tip);
    if (tip === 0n) return 0n;
    return fee < big(tx.max_fee) ? (tip < fee ? tip : fee) : null;
  }
  if (tx.size_bytes == null) return 0n;
  const min = 10_000n + BigInt(tx.size_bytes) * 10n;
  return fee > min ? fee - min : 0n;
}

export function effectsOf(tx: TxDetail): Effects {
  const pending = tx.status === "pending";
  const fee = big(tx.fee);
  if (!tx.from) return { changes: [], burned: 0n, unseen: null, pending };
  const tip = tipOf(tx, fee);
  const burned = fee - (tip ?? 0n);
  const tipped: Change[] = tip ? [{ who: PROPOSER, delta: tip, why: "tip, above the fee that burns" }] : [];
  const split = tip == null ? "Part of the fee may have tipped the block's proposer rather than burning; the node doesn't say how much." : null;
  // Included but failed: only the fee was charged.
  if (tx.status === "failed") return { changes: [{ who: tx.from, delta: -fee, why: "fee (the action failed)" }, ...tipped], burned, unseen: split, pending };
  const kind = tx.kind ?? tx.action?.type ?? "";
  // A kind newer than this explorer: the fee is all that's certain (its amount may even be
  // coming in, not going out).
  if (!knownKind(kind)) return { changes: [{ who: tx.from, delta: -fee, why: "fee" }, ...tipped], burned, unseen: UNKNOWN, pending };
  const amount = big(tx.amount);
  const changes: Change[] = [];
  if (amount > 0n) {
    // An escrowing kind's amount goes to its holding even when the node names a `to` (a vouch
    // names the newcomer, but the bond is held by the program, not paid to them).
    const dest = ESCROW[kind]?.(tx) ?? tx.to ?? null;
    if (dest && dest.address === tx.from.address) {
      changes.push({ who: tx.from, delta: -fee, why: "fee (paid itself)" });
    } else {
      changes.push({ who: tx.from, delta: -(amount + fee), why: "amount + fee" });
      if (dest) changes.push({ who: dest, delta: amount, why: kind === "transfer" ? "received" : "held" });
    }
  } else {
    changes.push({ who: tx.from, delta: -fee, why: "fee" });
  }
  changes.push(...tipped);
  const unseen = [UNSEEN[kind], split].filter(Boolean).join(" ");
  return { changes, burned, unseen: unseen || null, pending };
}

export type Direction = "in" | "out" | "self" | "none";

/** Which way the money went for `me`, by this transaction's visible effects. */
export function deltaFor(tx: TxDetail, me: string): { delta: bigint; direction: Direction; exact: boolean } {
  const fx = effectsOf(tx);
  const delta = fx.changes.reduce((s, c) => (c.who.address === me ? s + c.delta : s), 0n);
  const mine = fx.changes.some((c) => c.who.address === me);
  const self = !!tx.from && tx.to?.address === tx.from.address;
  const direction: Direction = !mine ? "none" : self ? "self" : delta > 0n ? "in" : delta < 0n ? "out" : "none";
  return { delta, direction, exact: !fx.unseen };
}

/** The other side of a transaction, from `me`'s point of view. */
export function counterparty(tx: TxDetail, me: string | null): Label | Holding | null {
  const fx = effectsOf(tx);
  if (!tx.from) return null;
  if (me && tx.from.address !== me) return tx.from;
  const other = fx.changes.find((c) => c.who.address !== tx.from?.address);
  return other?.who ?? tx.to ?? null;
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
  let burned = 0n, totalIn = 0n, totalOut = 0n, counted = 0, unseen = 0;
  let from: number | null = null, to: number | null = null;
  for (const tx of txs) {
    if (tx.status === "pending" || tx.height == null) continue;
    const fx = effectsOf(tx);
    const mine = fx.changes.find((c) => c.who.address === me);
    if (!mine) continue;
    counted++;
    if (fx.unseen) unseen++;
    from = from == null ? tx.height : Math.min(from, tx.height);
    to = to == null ? tx.height : Math.max(to, tx.height);
    if (tx.from?.address === me) burned += fx.burned;
    for (const c of fx.changes) {
      if (c.who.address === me) continue;
      const row = by.get(c.who.address) ?? { who: c.who, in: 0n, out: 0n, count: 0 };
      row.count++;
      // The other side gained what we lost (minus the fee, which burned), or the reverse.
      if (c.delta > 0n) {
        row.out += c.delta;
        totalOut += c.delta;
      } else {
        // What they lost beyond their fee (burned or tipped) came to us.
        const got = -c.delta - (tx.from?.address === c.who.address ? big(tx.fee) : 0n);
        row.in += got;
        totalIn += got;
      }
      by.set(c.who.address, row);
    }
  }
  const rows = [...by.values()].sort((a, b) => cmp(b.in + b.out, a.in + a.out));
  return { rows, burned, totalIn, totalOut, counted, unseen, from, to };
}

const cmp = (a: bigint, b: bigint) => (a > b ? 1 : a < b ? -1 : 0);

/** Per signing key: how many of these transactions it signed and what it spent (amount + fee). */
export function bySigner(txs: TxDetail[], account: string): Map<string, { count: number; spent: bigint; last: number | null }> {
  const out = new Map<string, { count: number; spent: bigint; last: number | null }>();
  for (const tx of txs) {
    if (!tx.signer_key || tx.from?.address !== account) continue;
    const s = out.get(tx.signer_key) ?? { count: 0, spent: 0n, last: null };
    s.count++;
    if (tx.status !== "failed") s.spent += big(tx.amount) + big(tx.fee);
    else s.spent += big(tx.fee);
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
 * The node's summary, from `me`'s side: "vlad.boo sent 1 FANG to mina.boo" reads
 * "You sent 1 FANG to mina.boo" for vlad and "vlad.boo sent 1 FANG to you" for mina.
 */
export function fromMySide(summary: string, me: Me | null): string {
  if (!me) return summary;
  const ids = [...me.names, me.address, me.short].filter((s): s is string => !!s).sort((a, b) => b.length - a.length);
  if (!ids.length) return summary;
  // A whole name or address, not part of a longer one, and not the object of "the name x.boo".
  const re = new RegExp(`(?<![\\w.…-])(?<!the name )(?:${ids.map(escapeRe).join("|")})(?![\\w…-])`, "g");
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
