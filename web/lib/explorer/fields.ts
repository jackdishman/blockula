/**
 * An action form's fields as typed, checked before anything is built: the same rules the chain
 * applies, in words. Pure (the SDK's address check only), so `node --test` runs it directly.
 */
import { KNOWN_PREFIXES, addressPrefix, parseFang } from "@blockula/sdk";
import type { Param } from "./programs.ts";

const utf8 = (s: string) => new TextEncoder().encode(s).length;
const NAME = /^[a-z0-9-]{3,32}(\.boo)?$/i;
/** Longer than any name with its suffix (32 + ".boo"): an address is 60-odd characters. */
const NAME_MAX = 36;

/**
 * An account field as typed: a name, or an address of this chain's network (DEC-7: another
 * network's is refused, never converted). `prefix` is the chain's (`client.pin.prefix`). Only
 * something too long for a name, starting with a known network's prefix, is read as an address:
 * a name may look like `team1abc…` (audit review-main13-5).
 */
export function checkAccount(raw: string, prefix: string): string | null {
  const v = raw.trim().toLowerCase();
  const hrp = v.match(/^([a-z]+)1[a-z0-9]+$/)?.[1];
  if (hrp && (KNOWN_PREFIXES as readonly string[]).includes(hrp) && v.length > NAME_MAX) {
    const valid = addressPrefix(v);
    if (!valid) return "That address doesn't look right. Check for typos.";
    return valid === prefix ? null : `That's an address on another network (${valid}1…). Addresses here start with ${prefix}1.`;
  }
  return NAME.test(v) ? null : `A .boo name or a ${prefix}1… address.`;
}

/** What's wrong with one field as typed, or null. */
export function checkField(p: Pick<Param, "type" | "optional" | "max">, raw: string, prefix: string): string | null {
  const v = raw.trim();
  if (!v) return p.optional ? null : "Required.";
  switch (p.type) {
    case "fang":
      try {
        return parseFang(v) > 0n ? null : "More than zero.";
      } catch {
        return "A number like 1 or 0.25 (up to 9 decimals).";
      }
    case "account":
    case "validator":
      return checkAccount(v, prefix);
    case "name":
      return /^[a-z0-9](?:[a-z0-9-]{1,30}[a-z0-9])(\.boo)?$/i.test(v) ? null : "3 to 32 of a–z, 0–9 and dashes, not starting or ending with a dash.";
    case "int":
      return /^\d+$/.test(v) && Number.isSafeInteger(Number(v)) ? null : "A whole number.";
    case "years":
      return /^\d+$/.test(v) && Number(v) >= 1 && Number(v) <= 10 ? null : "1 to 10 years.";
    case "bps":
      return /^\d+$/.test(v) && Number(v) <= 10_000 ? null : "0 to 10000 (100 basis points = 1%).";
    case "days":
      return /^\d+$/.test(v) && Number(v) >= 1 && Number(v) <= 365 ? null : "1 to 365 days.";
    case "repo":
      return /^[\w.-]+\/[\w.-]+$/.test(v) ? null : "Like owner/name.";
    case "key_id":
      return /^[0-9a-f]{64}$/.test(v.toLowerCase()) ? null : "64 characters of 0–9 and a–f.";
    case "text":
      return p.max && utf8(v) > p.max ? `At most ${p.max} bytes.` : null;
  }
  return null;
}
