import { formatFang } from "@blockula/sdk";

/** "123.456789 FANG" → "123.4568" with a separate unit, for big displays. */
export function fangParts(drops: string | bigint) {
  const s = formatFang(BigInt(drops)).replace(/ FANG$/, "");
  const [w, f] = s.split(".");
  const whole = BigInt(w).toLocaleString("en-US");
  return { whole, frac: f ? f.slice(0, 4).replace(/0+$/, "") : "" };
}

export const shortAddr = (a: string) => (a.length > 18 ? `${a.slice(0, 10)}…${a.slice(-6)}` : a);

/** A short, human-checkable code for a 64-hex key id, e.g. "A1B2-C3D4-…-9F8E". */
export const keyCode = (keyId: string) => {
  const k = keyId.toUpperCase();
  return `${k.slice(0, 4)}-${k.slice(4, 8)}-${k.slice(-4)}`;
};

export const isKeyId = (s: string) => /^[0-9a-f]{64}$/.test(s.trim().toLowerCase());

/** Rough human duration for a block count at the chain's block time. */
export function blocksToHuman(blocks: number, blockTimeMs = 1000) {
  const s = (blocks * blockTimeMs) / 1000;
  if (s < 90) return `${Math.round(s)}s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  if (s < 86400) return `${Math.round(s / 3600)} h`;
  const d = Math.round(s / 86400);
  return d === 1 ? "day" : `${d} days`;
}

const DROPS = 1_000_000_000n;

/**
 * FANG for reading, from drops: thousands grouped ("40,000,010.4957 FANG"). Summaries show at most
 * 4 decimals, with "≈ " in front when that rounded anything away; `exact` (reviews, what gets
 * signed) keeps every drop.
 */
export function displayFang(drops: string | bigint, opts: { exact?: boolean; unit?: boolean } = {}): string {
  let d = typeof drops === "bigint" ? drops : /^-?\d+$/.test(drops) ? BigInt(drops) : 0n;
  const neg = d < 0n;
  if (neg) d = -d;
  let approx = false;
  if (!opts.exact) {
    // Round half up to 4 decimals (100,000 drops).
    const step = 100_000n;
    const rounded = ((d + step / 2n) / step) * step;
    approx = rounded !== d;
    d = rounded;
  }
  const whole = (d / DROPS).toLocaleString("en-US");
  const frac = (d % DROPS).toString().padStart(9, "0").replace(/0+$/, "");
  const n = `${neg && d > 0n ? "−" : ""}${whole}${frac ? `.${frac}` : ""}`;
  return `${approx ? "≈ " : ""}${n}${opts.unit === false ? "" : " FANG"}`;
}

/** A FANG amount as the amount field takes it: plain digits, no grouping or unit ("49.99993508"). */
export function fangInput(drops: bigint): string {
  const whole = (drops / DROPS).toString();
  const frac = (drops % DROPS).toString().padStart(9, "0").replace(/0+$/, "");
  return frac ? `${whole}.${frac}` : whole;
}

/** "1 block", "2 blocks". */
export const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;

/**
 * When a block `blocksAhead` from now is about due, in the reader's own clock: "about 21:15"
 * within the next 18 hours ("about 03:00 tomorrow" once that's past midnight), else "about Oct 28"
 * (with the year when it isn't this one). `timeZone` is for tests.
 */
export function aboutWhen(blocksAhead: number, blockTimeMs = 1000, now = Date.now(), timeZone?: string): string {
  const ms = Math.max(0, blocksAhead) * blockTimeMs;
  const at = new Date(now + ms);
  if (ms < 18 * 3_600_000) {
    const time = at.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit", timeZone });
    // The reader's calendar day: a bare "03:00" tomorrow would read as earlier today.
    const day = (d: Date) => d.toLocaleDateString("en-CA", { timeZone });
    return day(at) === day(new Date(now)) ? `about ${time}` : `about ${time} tomorrow`;
  }
  const sameYear = new Date(now).toLocaleDateString("en-US", { year: "numeric", timeZone }) === at.toLocaleDateString("en-US", { year: "numeric", timeZone });
  return `about ${at.toLocaleDateString("en-US", { month: "short", day: "numeric", ...(sameYear ? {} : { year: "numeric" }), timeZone })}`;
}
