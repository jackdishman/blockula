// Canonical transaction encoding. Must stay byte-identical to node/src/types.rs `Tx::signing_bytes`.
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils.js";

export type KeyRole =
  | { type: "owner" }
  /** Transfers only, at most `limit` drops (amount + charged fee) per `period_blocks`, until block `expires_at`. */
  | { type: "session"; limit: string; period_blocks: number; expires_at: number };

export type Action =
  | { type: "transfer"; to: string; amount: string }
  | { type: "register_name"; name: string }
  | { type: "add_key"; key_id: string; label: string; role: KeyRole }
  | { type: "remove_key"; key_id: string }
  /** Bond `bond` drops and offer `consensus_key` (hex ML-DSA-65 public key) as a validator. */
  | {
      type: "register_validator"; consensus_key: string; p2p_addr: string; http_addr: string; bond: string;
      /** Proof of possession: the consensus key's ML-DSA-65 signature over `popMessage(...)` with context `POP_CONTEXT` (see `blockula validator pop`). */
      pop: string;
    }
  /** Season 0 allowlist (admin accounts only). */
  | { type: "approve_validator"; validator: string }
  | { type: "remove_validator"; validator: string }
  /** Reclaim the bond of a removed validator you own, one epoch after it left. */
  | { type: "unbond"; validator: string }
  /** Claim a GitHub handle; a public gist by that handle containing your boo1 address verifies it. */
  | { type: "link_github"; handle: string; gist_id: string }
  | { type: "unlink_github"; handle: string }
  /** Escrow a bounty on a GitHub issue ("owner/name", issue number). */
  | { type: "create_hunt"; repo: string; issue: number; amount: string; expires_at: number; title: string }
  | { type: "fund_hunt"; hunt_id: number; amount: string }
  | { type: "award_hunt"; hunt_id: number; to: string; pr: number; familiar_key?: string }
  | { type: "cancel_hunt"; hunt_id: number }
  /** Mark one of your session keys as an agent Familiar. */
  | { type: "register_familiar"; key_id: string; label: string }
  // ---- 🪦 Proof of Contribution ----
  /** Season 0 admins: put a repo in the contribution pool (weight 1–100; 0 removes it). */
  | { type: "set_repo"; repo: string; weight: number }
  /** Season 0 admins: bootstrap Soul, in milli-Soul (≤ 100 Soul per grant). */
  | { type: "grant_soul"; to: string; amount: number; reason: string }
  /** Stake 10 FANG on a newcomer `account` being a real, distinct person (needs 10 Soul). */
  | { type: "vouch"; account: string }
  /** The voucher withdraws (after the lock), or the newcomer declines (any time). */
  | { type: "unvouch"; account: string }
  | { type: "flag_sybil"; account: string; reason: string }
  /** Claim a merged PR by your linked GitHub handle. Registered Familiars may sign this. */
  | { type: "submit_claim"; repo: string; pr: number; familiar_key?: string }
  /** Validators only (nodes do this automatically). */
  | { type: "attest_claim"; claim_id: number; ok: boolean; merge_sha: string; note: string }
  | { type: "reject_claim"; claim_id: number; reason: string }
  | { type: "join_jury"; bond: string }
  | { type: "leave_jury" }
  /** A juror's sealed judgment: `seanceCommitment(...)`. */
  | { type: "commit_votes"; seance_id: number; commitment: string }
  /** One of "a", "b", "=" per assigned pair, claim ids flagged as spam, and the salt. */
  | { type: "reveal_votes"; seance_id: number; votes: string; spam: number[]; salt: string }
  | { type: "fund_pool"; amount: string }
  | { type: "withdraw_vested" }
  | { type: "clawback"; account: string; reason: string };

export interface UnsignedTx {
  chain_id: string;
  /** The account acting; the signing key must be authorized on it. */
  from: string;
  nonce: number;
  /** The most the sender will pay, in drops. The tx is charged `minFee + tip` (see `txCharge`). */
  fee: string;
  /** Drops paid to the block's proposer on top of the burned base fee (default "0"). Blocks take the highest tip per byte first. */
  tip?: string;
  /** Last block height at which the tx may execute (at most an hour of blocks ahead). After it, the tx can never execute. */
  valid_until: number;
  action: Action;
  memo: string;
}

export interface Tx extends UnsignedTx {
  public_key: string;
  signature: string;
}

export const PUBLIC_KEY_LEN = 1952;
export const SIGNATURE_LEN = 3309;
export const DROPS_PER_FANG = 1_000_000_000n;

const utf8 = new TextEncoder();

class Writer {
  private parts: number[] = [];
  bytes(b: Uint8Array) { for (const x of b) this.parts.push(x); }
  u8(n: number) { this.parts.push(n & 0xff); }
  u16(n: number) { this.u8(n); this.u8(n >> 8); }
  u64(n: bigint) { for (let i = 0n; i < 8n; i++) this.u8(Number((n >> (8n * i)) & 0xffn)); }
  str(s: string) {
    const b = utf8.encode(s);
    if (b.length > 0xffff) throw new Error("string field too long to encode");
    this.u16(b.length);
    this.bytes(b);
  }
  ids(xs: number[]) { this.u16(xs.length); for (const x of xs) this.u64(BigInt(x)); }
  done() { return Uint8Array.from(this.parts); }
}

export function signingBytes(tx: UnsignedTx): Uint8Array {
  const w = new Writer();
  w.bytes(utf8.encode("BLOCKULA_TX_V3"));
  w.str(tx.chain_id);
  w.str(tx.from);
  w.u64(BigInt(tx.nonce));
  w.u64(BigInt(tx.fee));
  w.u64(BigInt(tx.tip ?? "0"));
  w.u64(BigInt(tx.valid_until));
  const a = tx.action;
  switch (a.type) {
    case "transfer":
      w.u8(0); w.str(a.to); w.u64(BigInt(a.amount));
      break;
    case "register_name":
      w.u8(1); w.str(a.name);
      break;
    case "add_key":
      w.u8(2); w.str(a.key_id); w.str(a.label);
      if (a.role.type === "owner") w.u8(0);
      else { w.u8(1); w.u64(BigInt(a.role.limit)); w.u64(BigInt(a.role.period_blocks)); w.u64(BigInt(a.role.expires_at)); }
      break;
    case "remove_key":
      w.u8(3); w.str(a.key_id);
      break;
    case "register_validator":
      w.u8(4); w.str(a.consensus_key); w.str(a.p2p_addr); w.str(a.http_addr); w.u64(BigInt(a.bond)); w.str(a.pop);
      break;
    case "approve_validator":
      w.u8(5); w.str(a.validator);
      break;
    case "remove_validator":
      w.u8(6); w.str(a.validator);
      break;
    case "unbond":
      w.u8(7); w.str(a.validator);
      break;
    case "link_github":
      w.u8(8); w.str(a.handle); w.str(a.gist_id);
      break;
    case "unlink_github":
      w.u8(9); w.str(a.handle);
      break;
    case "create_hunt":
      w.u8(10); w.str(a.repo); w.u64(BigInt(a.issue)); w.u64(BigInt(a.amount)); w.u64(BigInt(a.expires_at)); w.str(a.title);
      break;
    case "fund_hunt":
      w.u8(11); w.u64(BigInt(a.hunt_id)); w.u64(BigInt(a.amount));
      break;
    case "award_hunt":
      w.u8(12); w.u64(BigInt(a.hunt_id)); w.str(a.to); w.u64(BigInt(a.pr)); w.str(a.familiar_key ?? "");
      break;
    case "cancel_hunt":
      w.u8(13); w.u64(BigInt(a.hunt_id));
      break;
    case "register_familiar":
      w.u8(14); w.str(a.key_id); w.str(a.label);
      break;
    case "set_repo":
      w.u8(15); w.str(a.repo); w.u64(BigInt(a.weight));
      break;
    case "grant_soul":
      w.u8(16); w.str(a.to); w.u64(BigInt(a.amount)); w.str(a.reason);
      break;
    case "vouch":
      w.u8(17); w.str(a.account);
      break;
    case "unvouch":
      w.u8(18); w.str(a.account);
      break;
    case "flag_sybil":
      w.u8(19); w.str(a.account); w.str(a.reason);
      break;
    case "submit_claim":
      w.u8(20); w.str(a.repo); w.u64(BigInt(a.pr)); w.str(a.familiar_key ?? "");
      break;
    case "attest_claim":
      w.u8(21); w.u64(BigInt(a.claim_id)); w.u8(a.ok ? 1 : 0); w.str(a.merge_sha); w.str(a.note);
      break;
    case "reject_claim":
      w.u8(22); w.u64(BigInt(a.claim_id)); w.str(a.reason);
      break;
    case "join_jury":
      w.u8(23); w.u64(BigInt(a.bond));
      break;
    case "leave_jury":
      w.u8(24);
      break;
    case "commit_votes":
      w.u8(25); w.u64(BigInt(a.seance_id)); w.str(a.commitment);
      break;
    case "reveal_votes":
      w.u8(26); w.u64(BigInt(a.seance_id)); w.str(a.votes); w.ids(a.spam); w.str(a.salt);
      break;
    case "fund_pool":
      w.u8(27); w.u64(BigInt(a.amount));
      break;
    case "withdraw_vested":
      w.u8(28);
      break;
    case "clawback":
      w.u8(29); w.str(a.account); w.str(a.reason);
      break;
  }
  w.str(tx.memo);
  return w.done();
}

/**
 * What a Séance juror seals before revealing (node/src/types.rs `seance_commitment`): hex
 * BLAKE3 of the Séance id, the juror's account, the votes, spam flags and a secret salt.
 */
export function seanceCommitment(seanceId: number, juror: string, votes: string, spam: number[], salt: string): string {
  const w = new Writer();
  w.bytes(utf8.encode("BLOCKULA_SEANCE_V1"));
  w.u64(BigInt(seanceId));
  w.str(juror);
  w.str(votes);
  w.ids(spam);
  w.str(salt);
  return bytesToHex(blake3(w.done()));
}

/** "2.5" Soul → 2500 milli-Soul. */
export function parseSoul(soul: string | number): number {
  const s = String(soul).trim();
  if (!/^\d+(\.\d{1,3})?$/.test(s)) throw new Error(`invalid Soul amount: ${s}`);
  const [whole, frac = ""] = s.split(".");
  return Number(whole) * 1000 + Number(frac.padEnd(3, "0"));
}

/** FIPS 204 context string for validator proofs of possession (node/src/types.rs `POP_CONTEXT`). */
export const POP_CONTEXT = utf8.encode("blockula/validator-pop/v1");

/** What a validator's consensus key signs to register (node/src/types.rs `pop_message`). */
export function popMessage(chainId: string, owner: string, p2pAddr: string, httpAddr: string): Uint8Array {
  const w = new Writer();
  w.bytes(utf8.encode("BLOCKULA_VALIDATOR_POP_V1"));
  for (const s of [chainId, owner, p2pAddr, httpAddr]) w.str(s);
  return w.done();
}

/** Wire size in bytes (node/src/types.rs `Tx::size`): signing bytes plus the key and signature. Fees are priced on it. */
export function txSize(tx: UnsignedTx): number {
  return signingBytes(tx).length + PUBLIC_KEY_LEN + SIGNATURE_LEN;
}

/** Mirrors `min_fee` in node/src/types.rs: base fee is priced per byte and burned. */
export function minFee(tx: UnsignedTx): bigint {
  return 10_000n + BigInt(txSize(tx)) * 10n;
}

/** What the tx is charged (node/src/types.rs `Tx::charge`): `minFee + tip`. Its `fee` must cover it; nothing more is debited. */
export function txCharge(tx: UnsignedTx): bigint {
  return minFee(tx) + BigInt(tx.tip ?? "0");
}

/** Wire form (node/src/types.rs `Tx::to_bytes`): signing bytes ‖ public key (1,952 B) ‖ signature (3,309 B). Blocks and gossip carry this. */
export function txBytes(tx: Tx): Uint8Array {
  const sb = signingBytes(tx);
  const out = new Uint8Array(sb.length + PUBLIC_KEY_LEN + SIGNATURE_LEN);
  out.set(sb);
  out.set(hexToBytes(tx.public_key), sb.length);
  out.set(hexToBytes(tx.signature), sb.length + PUBLIC_KEY_LEN);
  return out;
}

/** A tx's id (node/src/types.rs `Tx::id`): hex BLAKE3 of the signing bytes and the signature. Known before submitting. */
export function txId(tx: Tx): string {
  const sb = signingBytes(tx);
  const sig = hexToBytes(tx.signature);
  const buf = new Uint8Array(sb.length + sig.length);
  buf.set(sb);
  buf.set(sig, sb.length);
  return bytesToHex(blake3(buf));
}

/** "1.5" → 1500000000n drops. Accepts numbers, decimal strings or bigint drops. */
export function parseFang(amount: string | number | bigint): bigint {
  if (typeof amount === "bigint") return amount;
  const s = String(amount).trim();
  if (!/^\d+(\.\d{1,9})?$/.test(s)) throw new Error(`invalid FANG amount: ${s}`);
  const [whole, frac = ""] = s.split(".");
  return BigInt(whole) * DROPS_PER_FANG + BigInt(frac.padEnd(9, "0"));
}

export function formatFang(drops: bigint | string): string {
  const d = BigInt(drops);
  const whole = d / DROPS_PER_FANG;
  const frac = (d % DROPS_PER_FANG).toString().padStart(9, "0").replace(/0+$/, "");
  return `${whole}${frac ? "." + frac : ""} FANG`;
}
