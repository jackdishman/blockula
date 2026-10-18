import { RPC } from "./config";

/** Turn node / WebAuthn / network error strings into something a human can act on. */
export function friendly(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e);
  const name = e instanceof Error ? e.name : "";
  const rules: [RegExp, string | ((m: RegExpMatchArray) => string)][] = [
    [/insufficient balance: has (.+?), needs (.+)$/, (m) => `Not enough FANG. You have ${m[1]}, but this needs ${m[2]} (including the network fee).`],
    [/(\S+) is already taken/, (m) => `${m[1]} is already taken. Try another spelling.`],
    [/limit reached: (.+) per (\d+) blocks/, (m) => `This key hit its spending limit (${m[1]} per period). It resets automatically.`],
    [/session key expired/, "This key's permission has expired. The account owner can grant a new one."],
    [/session keys can only send transfers/, "Agent keys can only send FANG, not manage names or keys."],
    [/isn't authorized on/, "This device's key isn't authorized on that account. It may have been revoked."],
    [/only owner keys can/, "Only an owner device can manage keys on this account."],
    [/no one has claimed (\S+) yet/, (m) => `${m[1]} isn't registered yet. Double-check the spelling.`],
    [/(\S+) isn't registered/, (m) => `${m[1]} isn't registered yet. Double-check the spelling.`],
    [/bad nonce/, "Your previous transaction is still settling. Give it a second and try again."],
    [/fee too low/, "The fee was too low for this transaction's size. Try again."],
    [/this chain has no faucet/, "This chain has no faucet. Ask a friend to send you some FANG."],
    [/can't remove the last owner key/, "That's your last owner key. Removing it would lock you out forever."],
    [/that key is already on this account/, "That key is already linked to this account."],
    [/at most \d+ keys/, "This account already has the maximum number of keys. Revoke one first."],
    [/key_id must be 32 bytes hex/, "That doesn't look like a key id. It should be 64 hex characters."],
    [/memo too long/, "That memo is too long (max 256 bytes)."],
    [/not active yet/, "This isn't active on this network yet."],
    [/no stake with/, "You don't have stake with that validator."],
    [/invalid FANG amount/, "Enter an amount like 1 or 0.5 (up to 9 decimal places)."],
    [/names must be 3/, "Names must be 3 to 32 characters."],
    [/names may only use/, "Names can only use a–z, 0–9 and dashes."],
    [/names can't start or end/, "Names can't start or end with a dash."],
    [/still pending/, "Sent, but the network hasn't confirmed it yet. Check your activity before sending again: it may still go through."],
    [/was dropped by this node without running/, "Your node dropped this transaction before running it, so nothing was charged, but another node may still include it for a few minutes. Check your activity before sending again."],
    [/reports height \d+, but by .* the chain can't be past block/, "Your node reports a block height the network can't have reached yet, so nothing was signed (a transaction signed against it could be held and sent later). Use another node, or check this device's clock."],
    [/disagrees about the state at block/, "The nodes this wallet checks names against disagree about who owns that name, so it wasn't used. Try again in a moment, or pay the address directly."],
    [/didn't answer within/, `The Blockula node at ${RPC} didn't answer in time. If you were sending, check your activity before trying again: it may have gone through.`],
    [/PRF extension/, "Your passkey provider doesn't support the PRF extension, which Blockula needs to derive your quantum-safe key."],
    [/Failed to fetch|NetworkError|Load failed|fetch failed/i, `Can't reach the Blockula node at ${RPC}. Pick another network in the menu at the top, or start a local node with \`blockula dev\`.`],
  ];
  if (name === "NotAllowedError") return "The passkey prompt was cancelled or timed out.";
  if (name === "InvalidStateError") return "This device already has a passkey for this wallet.";
  if (name === "SecurityError") return "Passkeys need a secure origin (https or localhost).";
  for (const [re, out] of rules) {
    const m = raw.match(re);
    if (m) return typeof out === "string" ? out : out(m);
  }
  return raw;
}

export const isPrfUnsupported = (e: unknown) =>
  (e instanceof Error && /PRF/.test(e.message)) || (typeof window !== "undefined" && !("PublicKeyCredential" in window));
