/**
 * A juror's Séance ballot in the wallet (docs/PLAN.md "Séances"): sealing commits to
 * BLAKE3(id, juror, votes, spam flags, salt), and revealing needs the very same votes, spam flags
 * and salt. The salt is a secret only this browser holds until the reveal, so it's kept in its
 * storage for this chain, account and Séance, and written there before the seal is sent: a seal
 * that lands always has its salt here. Lose it (clear the site's data) and the vote can't be
 * revealed, which costs the no-show penalty.
 *
 * Which ballot was sealed is read from the chain, not from storage: a kept ballot counts only if
 * it recomputes to the commitment the node lists for this juror. A new seal is kept beside the
 * earlier ones, never over them, so a seal that's still on its way keeps its salt; one that never
 * landed is just ignored.
 *
 * Pure but for the storage it's handed, so `node --test` runs it directly.
 */
import { seanceCommitment } from "@blockula/sdk";

export type Vote = "a" | "b" | "=";
export const VOTES: readonly Vote[] = ["a", "b", "="];

/** What a seal commits to, as kept between sealing and revealing. */
export interface Ballot {
  votes: string;
  spam: number[];
  salt: string;
}

/** The part of `Storage` used here (localStorage in the browser). */
export type BallotStore = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/** Where a ballot is kept: one per chain (its genesis), account and Séance. */
export function ballotKey(chain: { chainId?: string | null; genesisHash?: string | null }, account: string, seanceId: number): string {
  return `blockula.ballot:v1:${chain.chainId ?? ""}:${chain.genesisHash ?? ""}:${account}:${seanceId}`;
}

/** A fresh salt: 16 random bytes, as hex (what the SDK and the CLI use). */
export function newSalt(): string {
  return Array.from(globalThis.crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Ballots kept per Séance: every seal tried from this browser, newest last, so none is overwritten. */
export const KEPT = 8;

const valid = (x: Partial<Ballot> | null | undefined): x is Ballot =>
  !!x && typeof x.votes === "string" && typeof x.salt === "string" && Array.isArray(x.spam) && /^[ab=]*$/.test(x.votes) && /^[0-9a-f]{1,64}$/.test(x.salt) && x.spam.every((n) => Number.isSafeInteger(n) && n >= 0);

/** The ballots kept under `key`, oldest first: none if there are none (or what's there isn't ballots). */
export function loadBallots(store: BallotStore | null, key: string): Ballot[] {
  try {
    const raw = store?.getItem(key);
    if (!raw) return [];
    const x = JSON.parse(raw) as { v?: number; ballots?: Partial<Ballot>[] };
    if (x?.v !== 1 || !Array.isArray(x.ballots)) return [];
    return x.ballots.filter(valid).map((b) => ({ votes: b.votes, spam: b.spam, salt: b.salt }));
  } catch {
    return [];
  }
}

/**
 * Keeps a ballot beside the ones already kept for this Séance, never in their place: a seal sent
 * earlier may still land, and its salt must stay. False when the browser won't store it (private
 * mode, storage blocked or full).
 */
export function saveBallot(store: BallotStore | null, key: string, b: Ballot): boolean {
  try {
    if (!store) return false;
    const ballots = [...loadBallots(store, key), { votes: b.votes, spam: b.spam, salt: b.salt }].slice(-KEPT);
    store.setItem(key, JSON.stringify({ v: 1, ballots }));
    return loadBallots(store, key).some((x) => x.salt === b.salt);
  } catch {
    return false;
  }
}

export function forgetBallots(store: BallotStore | null, key: string): void {
  try {
    store?.removeItem(key);
  } catch {
    // Nothing to do: they stay until the browser clears them.
  }
}

/**
 * The kept ballot this juror sealed: the one that recomputes to `commitment`, the one the node
 * lists for the juror in this Séance. Null if none does (sealed elsewhere, or not yet).
 */
export function sealedBallot(ballots: Ballot[], seanceId: number, juror: string, commitment: string | null | undefined): Ballot | null {
  if (!commitment) return null;
  for (const b of ballots) {
    try {
      if (seanceCommitment(seanceId, juror, b.votes, b.spam, b.salt) === commitment) return b;
    } catch {
      // Not one this juror could have sealed: skip it.
    }
  }
  return null;
}

/** The votes as one string (one of a, b or = per pair, in order), or what's still missing. */
export function voteString(picks: (Vote | undefined)[], pairs: number): { votes: string } | { missing: number } {
  const missing = Array.from({ length: pairs }, (_, i) => picks[i]).filter((v) => !v || !VOTES.includes(v)).length;
  return missing ? { missing } : { votes: picks.slice(0, pairs).join("") };
}

/** The claim ids marked as spam, sorted and once each, limited to claims in the pairs shown. */
export function spamIds(marked: Iterable<number>, shown: Iterable<number>): number[] {
  const ok = new Set(shown);
  return [...new Set(marked)].filter((id) => ok.has(id)).sort((x, y) => x - y);
}

/**
 * A claim's pull request on GitHub, or null when the repo isn't a plain `owner/name` (it came
 * from the chain, so it's shown as text and never turned into some other link).
 */
export function prUrl(repo: string, pr: number): string | null {
  const m = /^([A-Za-z0-9](?:[A-Za-z0-9-]{0,38}))\/([A-Za-z0-9._-]{1,100})$/.exec(repo);
  if (!m || m[2] === "." || m[2] === ".." || !Number.isSafeInteger(pr) || pr <= 0) return null;
  return `https://github.com/${m[1]}/${m[2]}/pull/${pr}`;
}
