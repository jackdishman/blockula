/**
 * The Séance and Soul diagram on /contribute (components/explorer/SeanceFlow.tsx): a claim's way
 * from a merged pull request to a vested reward, as steps, and what Soul is, as facts. Every number
 * is the chain's own (`/api/contributions` `rules`, node/src/api.rs `contributions`; the constants
 * and timings in node/src/poc.rs): a rule the node didn't send leaves its number out and keeps the
 * step. Pure, so it's tested (lib/seance-flow.test.mjs).
 */
import { aboutWhen, blocksToHuman } from "../format.ts";

type Rules = Record<string, string | number> | null | undefined;

export type FlowKey = "merge" | "claim" | "maturity" | "opens" | "commit" | "reveal" | "payout" | "vest";

export interface FlowStep {
  key: FlowKey;
  label: string;
  /** The step's number from the rules ("30 days", "up to 25% of the pool"); null when the node didn't say. */
  value: string | null;
  /** One short line on what happens. */
  note: string;
}

/** A block count from the rules: a whole number, zero included. Null for anything else. */
const blocks = (v: unknown): number | null => (typeof v === "number" && Number.isInteger(v) && v >= 0 ? v : typeof v === "string" && /^\d+$/.test(v) ? Number(v) : null);
/** A count of people or seats: a whole number above zero. */
const count = (v: unknown): number | null => {
  const n = blocks(v);
  return n ? n : null;
};
/** A share as the node prints it ("25%"). */
const percent = (v: unknown): string | null => (typeof v === "string" && /^\d+(\.\d+)?%$/.test(v.trim()) ? v.trim() : null);
/** An amount as the node prints it ("1 FANG", "1,250.5 BOO"). */
const amount = (v: unknown): string | null => (typeof v === "string" && /^\d[\d,]*(\.\d+)?( [A-Z][A-Z0-9]{1,7})?$/.test(v.trim()) ? v.trim() : null);
/** Soul as the node prints it ("5", "2.5"). */
const soul = (v: unknown): string | null => (/^\d+(\.\d{1,3})?$/.test(String(v ?? "").trim()) ? String(v).trim() : null);

/**
 * A length of blocks in words: "40 min", "2.3 days", "30 days", "1 year". Under a day it's
 * `blocksToHuman`'s; days keep one decimal under ten (a commit period of 4.7 days isn't 5), and a
 * year or more is counted in years.
 */
export function span(blockCount: number, blockMs = 1000): string {
  const days = (blockCount * blockMs) / 86_400_000;
  if (days < 1) return blocksToHuman(blockCount, blockMs);
  const one = (n: number, unit: string) => `${n} ${unit}${n === 1 ? "" : "s"}`;
  if (days >= 365) return one(Math.round((days / 365) * 10) / 10, "year");
  return one(days < 10 ? Math.round(days * 10) / 10 : Math.round(days), "day");
}

const length = (v: unknown, blockMs: number) => {
  const n = blocks(v);
  return n == null ? null : n === 0 ? "no wait" : span(n, blockMs);
};

/** The eight steps from a merged pull request to a vested reward, with the chain's numbers. */
export function seanceFlow(rules: Rules, blockMs = 1000): FlowStep[] {
  const r = rules ?? {};
  const bond = amount(r.claim_bond_display);
  const every = count(r.seance_blocks);
  const [size, least, people] = [count(r.jury_size), count(r.min_jury), count(r.min_people)];
  const jury = size != null && least != null ? `Jury of up to ${size}, at least ${least}` : size != null ? `Jury of up to ${size}` : least != null ? `Jury of at least ${least}` : "The jury";
  const [share, jurors, floor] = [percent(r.pool_share_per_seance), percent(r.juror_share), percent(r.min_score)];
  return [
    { key: "merge", label: "Merged PR", value: null, note: "A pull request merged into one of the pool's repositories." },
    { key: "claim", label: "Claim", value: bond && `${bond} bond`, note: "Validators check the merge. The bond comes back unless the claim is rejected or judged spam." },
    { key: "maturity", label: "Maturity", value: length(r.claim_maturity_blocks, blockMs), note: "The wait after the merge is checked. A reverted PR drops out." },
    {
      key: "opens",
      label: "Séance opens",
      value: every != null ? `every ${span(every, blockMs)}` : null,
      note: `${jury}, seated by the randomness beacon.${people != null ? ` Needs matured claims from ${people} or more people.` : ""}`,
    },
    { key: "commit", label: "Sealed votes", value: length(r.commit_blocks, blockMs), note: "Jurors compare claims in pairs and commit sealed votes." },
    { key: "reveal", label: "Reveal", value: length(r.reveal_blocks, blockMs), note: "Jurors reveal their votes. The votes score each claim." },
    {
      key: "payout",
      label: "Payout",
      value: share && `up to ${share} of the pool`,
      note: [jurors && `Jurors share ${jurors}.`, floor ? `Claims scoring under ${floor} get nothing.` : "Low-scoring claims get nothing."].filter(Boolean).join(" "),
    },
    { key: "vest", label: "Vesting", value: length(r.vesting_blocks, blockMs), note: "Rewards unlock evenly over this time." },
  ];
}

export interface SoulFact {
  key: "earned" | "halves" | "jurors";
  label: string;
  text: string;
}

/** Soul in three facts: how it's earned, how it fades, what a juror needs. */
export function soulFacts(rules: Rules, blockMs = 1000): SoulFact[] {
  const r = rules ?? {};
  const half = count(r.soul_half_life_blocks);
  const [min, bond] = [soul(r.juror_min_soul), amount(r.juror_bond_display)];
  return [
    { key: "earned", label: "Earned", text: "When a jury accepts your claim, by judging in line with the outcome, or by a grant." },
    { key: "halves", label: "Fades", text: half != null ? `Halves about every ${span(half, blockMs)}.` : "Halves over time." },
    { key: "jurors", label: "Jurors need", text: min && bond ? `${min} Soul and a ${bond} bond.` : min ? `${min} Soul and a bond.` : bond ? `Soul and a ${bond} bond.` : "Soul and a bond." },
  ];
}

export interface FlowMark {
  /** The step it sits on. */
  step: FlowKey;
  /** "now": the open Séance's phase. "next": the next opening. */
  kind: "now" | "next";
  text: string;
}

/**
 * Where the chain is on the flow: the open Séance's phase with the time it has left, and the next
 * opening as a block with about when it's due on the reader's clock. `now` and `timeZone` are for tests.
 */
export function flowMarks(
  c: { next_seance_at?: number; open_seance?: { id: number; phase: string; commit_until: number; reveal_until: number } | null },
  height: number,
  blockMs = 1000,
  now = Date.now(),
  timeZone?: string,
): FlowMark[] {
  const out: FlowMark[] = [];
  const s = c.open_seance;
  if (s) {
    const sealing = s.phase === "sealing";
    const left = (sealing ? s.commit_until : s.reveal_until) - height;
    out.push({
      step: sealing ? "commit" : "reveal",
      kind: "now",
      text: `Séance #${s.id} is ${sealing ? "sealing votes" : "revealing votes"}: ${left > 0 ? `${span(left, blockMs)} left` : "closing now"}`,
    });
  }
  const at = blocks(c.next_seance_at);
  if (at != null) {
    const ahead = at - height;
    const lead = s ? "The one after can open" : "Next Séance can open";
    out.push({ step: "opens", kind: "next", text: `${lead} ${ahead > 0 ? aboutWhen(ahead, blockMs, now, timeZone) : "now"} (block #${at.toLocaleString("en-US")})` });
  }
  return out;
}
