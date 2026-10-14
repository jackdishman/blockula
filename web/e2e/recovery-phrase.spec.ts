// The recovery-kit encoding (lib/recovery.ts), without a browser: BIP39's published 256-bit
// vectors (github.com/trezor/python-mnemonic vectors.json), the vendored wordlist's hash, and
// the error paths a person will hit.
import { createHash, randomBytes } from "node:crypto";
import { expect, test } from "@playwright/test";
import { WORDLIST } from "../lib/bip39-english";
import { confirmPositions, KIT_WORDS, kitText, phraseProblem, seedToWords, wordIndex, wordsToSeed } from "../lib/recovery";

const BIP39_256: [string, string][] = [
  ["00".repeat(32), `${"abandon ".repeat(23)}art`],
  ["7f".repeat(32), "legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth title"],
  ["80".repeat(32), "letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic bless"],
  ["ff".repeat(32), `${"zoo ".repeat(23)}vote`],
  ["68a79eaca2324873eacc50cb9c6eca8cc68ea5d936f98787c60c7ebc74e6ce7c", "hamster diagram private dutch cause delay private meat slide toddler razor book happy fancy gospel tennis maple dilemma loan word shrug inflict delay length"],
  ["9f6a2878b2520799a44ef18bc7df394e7061a224d2c33cd015b157d746869863", "panda eyebrow bullet gorilla call smoke muffin taste mesh discover soft ostrich alcohol speed nation flash devote level hobby quick inner drive ghost inside"],
  ["066dca1a2bb7e8a1db2832148ce9933eea0f3ac9548d793112d9a95c9407efad", "all hour make first leader extend hole alien behind guard gospel lava path output census museum junior mass reopen famous sing advance salt reform"],
  ["f585c11aec520db57dd353c69554b21a89b20fb0650966fa0a9d6f74fd989d8f", "void come effort suffer camp survey warrior heavy shoot primary clutch crush open amazing screen patrol group space point ten exist slush involve unfold"],
];

const hex = (b: Uint8Array) => Buffer.from(b).toString("hex");

test("the vendored wordlist is BIP39's english.txt", () => {
  expect(WORDLIST).toHaveLength(2048);
  expect(createHash("sha256").update(`${WORDLIST.join("\n")}\n`).digest("hex")).toBe("2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda");
  // Every word is unique in its first four letters (what makes abbreviations safe).
  expect(new Set(WORDLIST.map((w) => w.slice(0, 4))).size).toBe(2048);
});

test("encodes seeds exactly like BIP39 (256-bit vectors)", () => {
  for (const [entropy, phrase] of BIP39_256) {
    expect(seedToWords(Buffer.from(entropy, "hex")).join(" ")).toBe(phrase);
    expect(hex(wordsToSeed(phrase))).toBe(entropy);
  }
});

test("round-trips random seeds and forgives how people write words down", () => {
  for (let i = 0; i < 200; i++) {
    const seed = new Uint8Array(randomBytes(32));
    const words = seedToWords(seed);
    expect(words).toHaveLength(KIT_WORDS);
    expect(hex(wordsToSeed(words.join(" ")))).toBe(hex(seed));
    // Numbered, one per line, upper case, four-letter abbreviations.
    const written = words.map((w, n) => `${n + 1}. ${(n % 2 ? w.slice(0, 4) : w).toUpperCase()}`).join("\n");
    expect(hex(wordsToSeed(written))).toBe(hex(seed));
  }
  expect(wordIndex("aban")).toBe(0);
  expect(wordIndex("zoo")).toBe(2047);
  expect(wordIndex("ab")).toBeUndefined();
  expect(wordIndex("vampire")).toBeUndefined();
});

test("says what's wrong with a phrase", () => {
  const ok = BIP39_256[4][1];
  expect(phraseProblem(ok)).toBeNull();
  expect(phraseProblem(ok.split(" ").slice(1).join(" "))).toMatch(/has 24 words; that's 23/);
  expect(phraseProblem(ok.replace("hamster", "hamstr"))).toMatch(/Not on the word list: hamstr/);
  const w = ok.split(" ");
  [w[0], w[1]] = [w[1], w[0]];
  expect(phraseProblem(w.join(" "))).toMatch(/don't add up/);
  expect(() => seedToWords(new Uint8Array(16))).toThrow(/32 bytes/);
});

test("asks back two distinct positions, spread over the whole kit", () => {
  const seen = new Set<number>();
  for (let i = 0; i < 2000; i++) {
    const [a, b] = confirmPositions();
    expect(a).toBeLessThan(b);
    expect(a).toBeGreaterThanOrEqual(0);
    expect(b).toBeLessThan(KIT_WORDS);
    seen.add(a).add(b);
  }
  expect(seen.size).toBe(KIT_WORDS);
});

test("the kit file has everything needed to restore", () => {
  const words = BIP39_256[5][1].split(" ");
  const text = kitText({ words, keyId: "ab".repeat(32), account: "boo1example", name: "vlad.boo", network: "Testnet", chainId: "blockula-testnet", created: "2026-10-31", walletUrl: "https://wallet.blockula.test" });
  for (const [i, w] of words.entries()) expect(text).toContain(`${String(i + 1).padStart(2)}. ${w}`);
  for (const s of ["vlad.boo", "boo1example", "ab".repeat(32), "blockula-testnet", "2026-10-31", "https://wallet.blockula.test", "Restore with a recovery kit"]) expect(text).toContain(s);
});
