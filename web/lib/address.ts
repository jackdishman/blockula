import { bech32m } from "@scure/base";

const PREFIXES = ["boo", "tboo"];

/**
 * The 32 bytes inside a canonical address, or null. The same check the SDK uses before it
 * trusts an address: a known prefix, 32 bytes, and only the exact rendering (another checksum
 * or any uppercase is a different string, not this account).
 */
export function addressBytes(address: string): Uint8Array | null {
  if (address.length > 90) return null;
  try {
    const { prefix, words } = bech32m.decode(address as `${string}1${string}`);
    const bytes = Uint8Array.from(bech32m.fromWords(words));
    if (!PREFIXES.includes(prefix) || bytes.length !== 32) return null;
    if (bech32m.encode(prefix, words) !== address) return null;
    return bytes;
  } catch {
    return null;
  }
}

/** Lowercase hex, two characters a byte. */
export function hexOf(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}
