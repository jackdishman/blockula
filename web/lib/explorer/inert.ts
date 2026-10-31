/**
 * What a program or its deployer chose (a method's name, an argument's, an abort message) is text a
 * stranger wrote. These make it fit to show as text: never markup (React escapes it) and never able
 * to hide or reorder what's around it. Pure and dependency-free, so `node --test` runs it directly.
 */

/** The node's short form of an address ("boo1ab12cd…ef34gh"). */
export const shortAddress = (a: string) => (a.length > 18 ? `${a.slice(0, 10)}…${a.slice(-6)}` : a);

/**
 * Text a program or its deployer chose, made safe to show: control, format, private-use and bidi
 * characters removed (they hide or reorder the text around it), whitespace collapsed, cut to `max`
 * characters by code point. React shows it as text; this is for what the eye reads.
 */
export function inertText(v: unknown, max = 64): string {
  const clean = String(v ?? "").replace(/[\p{Cc}\p{Cf}\p{Co}\p{Cs}\p{Zl}\p{Zp}]+/gu, " ").replace(/ {2,}/g, " ").trim();
  const chars = [...clean];
  return chars.length > max ? `${chars.slice(0, max).join("")}…` : clean;
}

/** An interface type (`/api/program`'s JSON: "u64", `{ "vec": ["address", 8] }`, …) as a short signature. */
export function typeText(t: unknown, depth = 0): string {
  if (typeof t === "string") return inertText(t, 16);
  if (depth > 4 || !t || typeof t !== "object") return "?";
  const [key, value] = Object.entries(t as Record<string, unknown>)[0] ?? [];
  if (key === "option") return `option<${typeText(value, depth + 1)}>`;
  if (key === "vec" && Array.isArray(value)) return `vec<${typeText(value[0], depth + 1)}, ${inertText(value[1], 6)}>`;
  if (key === "bytes" || key === "text") return `${key}(${inertText(value, 6)})`;
  return "?";
}

/** The fields of a program action that carry bytes (code, an interface, a call's input) or the data of a receipt's claims: hex that runs to hundreds of kilobytes. */
const PROGRAM_BLOBS = new Set(["wasm", "interface", "input", "data", "return_data"]);

/**
 * `JSON.stringify(v, null, 2)` for a program's raw data: a blob field (`wasm`, `interface`, `input`, `data`,
 * `return_data`) past `max` characters is cut (a program's code, in hex, is hundreds of kilobytes), and the
 * characters that hide or reorder text are written out as `\uXXXX` so they show. Every other field, a key or a
 * proof included, is whole: this abridges blobs, never what a reader checks.
 */
export function abridged(v: unknown, max = 160): string {
  const shown = (x: string) => x.replace(/[\p{Cf}\p{Co}\p{Cs}\p{Zl}\p{Zp}]/gu, (c) => `\\u${(c.codePointAt(0) as number).toString(16).padStart(4, "0")}`);
  return JSON.stringify(v, (k, x) => (typeof x === "string" ? shown(PROGRAM_BLOBS.has(k) && x.length > max ? `${x.slice(0, 64)}… (${x.length.toLocaleString("en-US")} characters)` : x) : x), 2);
}

/** The actions of protocol v2's programs that run or store code (tags 62–64): the only ones whose blobs are abridged. */
const PROGRAM_ACTIONS = new Set(["upload_code", "create_program", "call_program"]);

/**
 * An action as "What was signed, exactly" shows it. A program action's blobs are abridged (`abridged`); **every
 * other action prints exactly as `JSON.stringify(action, null, 2)` does, whole**: an added key's public key, a
 * proof of possession or evidence is what a reader compares, so a v1 action is never cut.
 */
export function signedAction(action: { type?: unknown } | null | undefined): string {
  return typeof action?.type === "string" && PROGRAM_ACTIONS.has(action.type) ? abridged(action) : JSON.stringify(action, null, 2);
}
