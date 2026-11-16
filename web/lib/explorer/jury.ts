/**
 * The juror enrollment card's reading of the chain (app/contribute, components/explorer/JuryCard.tsx):
 * the Soul minimum and the bond from `/api/contributions`' rules (never numbers of this app's own),
 * where an account stands, when the next Séance can open, and why joining or leaving isn't on
 * offer, in words. Pure, so it's tested (lib/jury.test.mjs); the chain enforces every rule itself
 * (node/src/poc.rs `JoinJury`, `LeaveJury`), this only explains before anything is signed.
 */
import { aboutWhen, blocksToHuman } from "../format.ts";
import type { ActionSpec } from "./programs";

/**
 * The pool program's two jury actions (programs.ts lists them; /contribute signs them from its
 * card). The bond a form starts from here is only a placeholder for the generic form: the card
 * passes the chain's own (`juryRules`).
 */
export const JOIN_JURY: ActionSpec = {
  kind: "join_jury",
  title: "Join the jury pool",
  blurb: "Sit on Séances. Jurors who agree with the outcome earn Soul and a share of the payout; no-shows lose part of their bond.",
  who: "owner keys with 5+ Soul (humans only)",
  ownerOnly: true,
  params: [{ key: "bond", label: "Bond", type: "fang", initial: "10" }],
  build: (v) => ({ type: "join_jury", bond: String(v.bond) }),
  describe: (r) => `Join the jury pool with a ${r.bond || "…"} FANG bond`,
};

/** As `blockula contrib leave-jury`: out of the draw, and the bond comes back. */
export const LEAVE_JURY: ActionSpec = {
  kind: "leave_jury",
  title: "Leave the jury pool",
  blurb: "Stop sitting on Séances and take your juror bond back, whatever no-shows left of it. A juror seated on the open Séance leaves once it closes.",
  who: "owner keys of an account in the jury pool",
  ownerOnly: true,
  params: [],
  build: () => ({ type: "leave_jury" }),
  describe: () => "Leave the jury pool and take your bond back",
};

const DROPS = 1_000_000_000n;

/** "10 FANG", "1,250.5 FANG" → drops; null for anything that isn't an amount. */
export function displayToDrops(display: unknown): bigint | null {
  const m = /^\s*(\d[\d,]*)(?:\.(\d{1,9}))?(?:\s|$)/.exec(String(display ?? ""));
  if (!m) return null;
  return BigInt(m[1].replace(/,/g, "")) * DROPS + BigInt((m[2] ?? "").padEnd(9, "0") || "0");
}

/** Drops as an amount field takes them: "10", "2.5". */
const asInput = (drops: bigint) => {
  const frac = (drops % DROPS).toString().padStart(9, "0").replace(/0+$/, "");
  return `${drops / DROPS}${frac ? `.${frac}` : ""}`;
};

/** Soul as the node prints it ("5", "2.5") → its points (thousandths, as `soul.points`); null if it isn't a number. */
export function soulPoints(display: unknown): number | null {
  const s = String(display ?? "").trim();
  return /^\d+(\.\d{1,3})?$/.test(s) ? Math.round(Number(s) * 1000) : null;
}

export interface JuryRules {
  /** The least Soul a juror needs, as the node prints it, and in points. Null: the node didn't say. */
  minSoul: string | null;
  minSoulPoints: number | null;
  /** The juror bond: as the node prints it, in drops, and as the bond field takes it. */
  bondDisplay: string | null;
  bond: bigint | null;
  bondInput: string;
  /** How many jurors a Séance needs to open. */
  minJury: number | null;
}

/** What a juror needs, from `/api/contributions`' `rules`. Whatever the node didn't say stays unknown: nothing is filled in. */
export function juryRules(rules: Record<string, string | number> | null | undefined): JuryRules {
  const r = rules ?? {};
  const points = soulPoints(r.juror_min_soul);
  const bond = displayToDrops(r.juror_bond_display);
  const minJury = Number(r.min_jury);
  return {
    minSoul: points == null ? null : String(r.juror_min_soul).trim(),
    minSoulPoints: points,
    bondDisplay: bond == null ? null : String(r.juror_bond_display).trim(),
    bond,
    bondInput: bond == null ? "" : asInput(bond),
    minJury: Number.isInteger(minJury) && minJury > 0 ? minJury : null,
  };
}

/**
 * What joining posts: the whole bond, or for a benched juror whose bond a no-show cut, what brings
 * it back up to the rule (the chain counts what's already held: node/src/poc.rs `JoinJury`).
 */
export function bondToPost(rules: JuryRules, held: bigint | null): bigint | null {
  if (rules.bond == null) return null;
  return held != null && held > 0n && held < rules.bond ? rules.bond - held : rules.bond;
}

/** Drops as a bond field takes them. */
export const bondInput = (drops: bigint | null) => (drops == null ? "" : asInput(drops));

export type JurorStatus = "enrolled" | "benched" | "none";

/** Where an account stands in the juror pool, from its `contribution.juror`: in it, benched after a no-show, or not in it. */
export const jurorStatus = (juror: { active: boolean } | null | undefined): JurorStatus => (!juror ? "none" : juror.active ? "enrolled" : "benched");

export const JUROR_STATUS_TEXT: Record<JurorStatus, string> = {
  enrolled: "In the juror pool",
  benched: "Benched",
  none: "Not enrolled",
};

/** "block #1,234 (about 21:15, in 12 min)": a height ahead, in the reader's clock. One at or behind the head is "due now". */
export function blockWhen(block: number, height: number, blockMs = 1000, now = Date.now(), timeZone?: string): string {
  const at = `block #${block.toLocaleString("en-US")}`;
  const ahead = block - height;
  if (ahead <= 0) return `${at} (due now)`;
  return `${at} (${aboutWhen(ahead, blockMs, now, timeZone)}, in ${blocksToHuman(ahead, blockMs)})`;
}

export interface SeanceTiming {
  /** The Séance that's open now, and what it's waiting for. */
  open: { id: number; text: string } | null;
  /** When the next one can open. */
  next: string;
}

/** The open Séance's deadline and the next opening, as heights with about when they're due. */
export function seanceTiming(
  c: { next_seance_at: number; open_seance: { id: number; phase: string; commit_until: number; reveal_until: number } | null },
  height: number,
  blockMs = 1000,
  now = Date.now(),
  timeZone?: string,
): SeanceTiming {
  const when = (b: number) => blockWhen(b, height, blockMs, now, timeZone);
  const s = c.open_seance;
  const open = !s
    ? null
    : { id: s.id, text: s.phase === "sealing" ? `Séance #${s.id} is open: jurors seal their votes until ${when(s.commit_until)}.` : s.phase === "revealing" ? `Séance #${s.id} is open: jurors reveal their votes until ${when(s.reveal_until)}.` : `Séance #${s.id} closes at ${when(s.reveal_until)}.` };
  return { open, next: when(c.next_seance_at) };
}

export interface Blocker {
  code: "agent_key" | "sybil" | "enrolled" | "low_soul" | "low_balance" | "not_enrolled" | "seated";
  text: string;
}

const agentKey = (label: string): Blocker => ({ code: "agent_key", text: `This device signs with an agent key (“${label}”), and agent keys can only send transfers. Only a person sits on a jury: use an owner device.` });

export interface JoinInput {
  rules: JuryRules;
  status: JurorStatus;
  /** The account's Soul now: its points and as the node prints it. */
  soul: { points: number; display: string };
  /** What joining would post, when that isn't the whole bond (`bondToPost`). */
  post?: bigint | null;
  /** What the account can move now, in drops (null: not read yet). */
  transferable: bigint | null;
  /** The unlocked key's label when it's a session (agent) key. */
  agentKeyLabel?: string | null;
  sybil?: boolean;
  /** The chain's ticker. */
  unit?: string;
}

/** Why this account can't join the juror pool now; empty when the chain would take it. */
export function joinBlockers(i: JoinInput): Blocker[] {
  const out: Blocker[] = [];
  const unit = i.unit ?? "FANG";
  if (i.agentKeyLabel != null) out.push(agentKey(i.agentKeyLabel));
  if (i.status === "enrolled") out.push({ code: "enrolled", text: "This account is already in the juror pool. There's nothing more to join." });
  if (i.sybil) out.push({ code: "sybil", text: "This account is flagged as a sybil, and flagged accounts can't sit on juries." });
  const { minSoul, minSoulPoints } = i.rules;
  const bond = i.post ?? i.rules.bond;
  if (minSoulPoints != null && i.soul.points < minSoulPoints)
    out.push({ code: "low_soul", text: `Jurors need at least ${minSoul} Soul, and this account has ${i.soul.display}. Soul comes from work a jury accepted, or a grant from the chain's admins: it can't be bought or sent.` });
  // The fee comes on top of the bond, so the bond alone isn't enough; the preview checks the exact fee.
  if (i.status !== "enrolled" && bond != null && i.transferable != null && i.transferable <= bond)
    out.push({ code: "low_balance", text: `The bond is ${asInput(bond)} ${unit} and this account can move ${asInput(i.transferable)} ${unit} now: not enough for the bond and the fee.` });
  return out;
}

export interface LeaveInput {
  status: JurorStatus;
  agentKeyLabel?: string | null;
  /** The open Séance, when this account is on its jury. */
  seated?: { id: number; reveal_until: number } | null;
}

/** Why this account can't leave the juror pool now; empty when the chain would take it. */
export function leaveBlockers(i: LeaveInput): Blocker[] {
  const out: Blocker[] = [];
  if (i.agentKeyLabel != null) out.push(agentKey(i.agentKeyLabel));
  if (i.status === "none") out.push({ code: "not_enrolled", text: "This account isn't in the juror pool, so there's no bond to take back." });
  else if (i.seated) out.push({ code: "seated", text: `You're on the jury of Séance #${i.seated.id}. Leave after it closes (block #${i.seated.reveal_until.toLocaleString("en-US")}).` });
  return out;
}
