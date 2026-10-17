// Recovery kits (WAL-2): an offline ML-DSA-65 key, added to the account as an owner, written
// down as 24 words. The seed is made in the browser, shown once, and never stored or sent.
import { Wallet, encodeKeyGrant, type Client } from "@blockula/sdk";
import { sha256 } from "@noble/hashes/sha2.js";
import { WORDLIST } from "./bip39-english";

/** The label the kit's key gets on the account. */
export const KIT_LABEL = "recovery kit";
export const KIT_WORDS = 24;

const INDEX = new Map(WORDLIST.map((w, i) => [w, i]));

/**
 * 32 bytes → 24 words. The encoding is BIP39's (English wordlist, checksum = first byte of
 * SHA-256(seed), 264 bits read as 11-bit indexes), but the words ARE the ML-DSA-65 seed: no
 * passphrase, PBKDF2 or BIP32, so they open nothing in other wallets and vice versa.
 */
export function seedToWords(seed: Uint8Array): string[] {
  if (seed.length !== 32) throw new Error("a recovery seed is 32 bytes");
  const data = new Uint8Array(33);
  const hash = sha256(seed);
  data.set(seed);
  data[32] = hash[0];
  const words: string[] = [];
  let acc = 0;
  let bits = 0;
  for (const byte of data) {
    acc = (acc << 8) | byte;
    bits += 8;
    while (bits >= 11) {
      bits -= 11;
      words.push(WORDLIST[(acc >>> bits) & 0x7ff]);
    }
    acc &= (1 << bits) - 1;
  }
  data.fill(0);
  hash.fill(0);
  return words;
}

/** The words someone typed: lowercase letters only, so numbering, commas and line breaks don't matter. */
export const splitWords = (text: string) => text.toLowerCase().split(/[^a-z]+/).filter(Boolean);

/** A word's index. BIP39 words are unique in their first four letters, so those are enough. */
export function wordIndex(word: string): number | undefined {
  const w = word.trim().toLowerCase();
  const exact = INDEX.get(w);
  if (exact !== undefined || w.length < 4) return exact;
  const hits = WORDLIST.filter((x) => x.startsWith(w));
  return hits.length === 1 ? INDEX.get(hits[0]) : undefined;
}

export class PhraseError extends Error {}

/** 24 words → the 32-byte seed. Throws a `PhraseError` a person can act on. */
export function wordsToSeed(text: string): Uint8Array {
  const words = splitWords(text);
  if (words.length !== KIT_WORDS) throw new PhraseError(`A recovery kit has ${KIT_WORDS} words; that's ${words.length}.`);
  const unknown = words.filter((w) => wordIndex(w) === undefined);
  if (unknown.length) throw new PhraseError(`Not on the word list: ${unknown.join(", ")}. Check the spelling against your kit.`);
  const data = new Uint8Array(33);
  let acc = 0;
  let bits = 0;
  let n = 0;
  for (const w of words) {
    acc = (acc << 11) | (wordIndex(w) as number);
    bits += 11;
    while (bits >= 8) {
      bits -= 8;
      data[n++] = (acc >>> bits) & 0xff;
    }
    acc &= (1 << bits) - 1;
  }
  const seed = data.slice(0, 32);
  const hash = sha256(seed);
  const ok = hash[0] === data[32];
  data.fill(0);
  hash.fill(0);
  if (!ok) {
    seed.fill(0);
    throw new PhraseError("These words don't add up: one is probably wrong or out of order. Check them against your kit.");
  }
  return seed;
}

/** What's wrong with a typed phrase, or null if it's a valid kit. */
export function phraseProblem(text: string): string | null {
  try {
    wordsToSeed(text).fill(0);
    return null;
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}

/** Two distinct word positions (0-based, ascending) to ask back, so we know the kit was written down. */
export function confirmPositions(): [number, number] {
  const r = crypto.getRandomValues(new Uint32Array(2));
  const a = r[0] % KIT_WORDS;
  const b = (a + 1 + (r[1] % (KIT_WORDS - 1))) % KIT_WORDS;
  return a < b ? [a, b] : [b, a];
}

/**
 * A fresh kit key for `account`, made in this browser. Only its words and its grant (the kit key's
 * public consent to joining `account` on `client`'s chain, which adding it needs: audit gap-7-2)
 * leave this function; the seed is wiped.
 */
export async function generateKit(account: string, client: Client) {
  const seed = crypto.getRandomValues(new Uint8Array(32));
  const words = seedToWords(seed);
  const wallet = Wallet.fromSeed(seed, client);
  try {
    const grant = encodeKeyGrant(await wallet.keyGrant(account));
    return { words, keyId: wallet.keyId, grant };
  } finally {
    wipeWallet(wallet);
    seed.fill(0);
  }
}

/**
 * Best effort: zero a wallet's seed and secret key once it's no longer needed. JS can't promise
 * more (strings and engine copies aren't ours to wipe), but nothing keeps a reference after this.
 */
export function wipeWallet(w: Wallet) {
  w.seed.fill(0);
  const sk = (w as unknown as { secretKey?: unknown }).secretKey;
  if (sk instanceof Uint8Array) sk.fill(0);
}

/** Everything printed on a kit. */
export interface Kit {
  words: string[];
  keyId: string;
  /** The kit key's grant code for `account` (public; not printed). */
  grant?: string;
  account: string;
  name?: string;
  network: string;
  chainId: string;
  created: string;
  walletUrl: string;
}

/** The kit as a plain-text file. */
export function kitText(k: Kit) {
  const rows = Array.from({ length: KIT_WORDS / 4 }, (_, r) =>
    [0, 1, 2, 3].map((c) => `${String(r * 4 + c + 1).padStart(2)}. ${k.words[r * 4 + c].padEnd(9)}`).join("  ").trimEnd(),
  );
  return [
    "BLOCKULA RECOVERY KIT",
    "",
    "Anyone holding these 24 words controls the account below. Keep this offline: print it or",
    "write it down, and delete any digital copy. Only ever type the words into the Blockula wallet.",
    "",
    `Account    ${k.name ? `${k.name}  ` : ""}${k.account}`,
    `Network    ${k.network} (chain ${k.chainId})`,
    `Kit key    ${k.keyId}  (an owner key labeled "${KIT_LABEL}")`,
    `Made       ${k.created}`,
    "",
    "Recovery words",
    ...rows,
    "",
    "To restore",
    `1. Open ${k.walletUrl} on a new device and choose "Restore with a recovery kit".`,
    "2. Type the 24 words. The wallet finds the account this kit belongs to.",
    "3. Create a passkey on the new device; the kit adds it to your account as an owner.",
    "4. Revoke the lost device, then make a new kit and revoke this one (its words have been typed in).",
    "",
    "The words use the BIP39 wordlist but are not a BIP39 wallet seed: they only work with Blockula.",
    "",
  ].join("\n");
}
