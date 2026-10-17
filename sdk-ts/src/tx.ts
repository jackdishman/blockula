// Canonical transaction encoding. Must stay byte-identical to node/src/types.rs `Tx::signing_bytes`.
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, concatBytes, hexToBytes } from "@noble/hashes/utils.js";

export type KeyRole =
  | { type: "owner" }
  /** Transfers only, at most `limit` drops (amount + charged fee) per `period_blocks`, until block `expires_at`. */
  | { type: "session"; limit: string; period_blocks: number; expires_at: number };

export type Action =
  | { type: "transfer"; to: string; amount: string }
  | { type: "register_name"; name: string }
  /**
   * Authorize a key on this account (owner keys only). The added key consents: `public_key` is its
   * hex ML-DSA-65 public key (`key_id` is its BLAKE3) and `pop` its signature over
   * `keyPopMessage(chain_id, genesis_hash, account, key_id)` with context `KEY_POP_CONTEXT`. Build
   * it from a `KeyGrant` (`Wallet.keyGrant`, `addOwnerKey`, `addSessionKey`).
   */
  | { type: "add_key"; key_id: string; label: string; role: KeyRole; public_key: string; pop: string }
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
  /**
   * A juror's sealed judgment: `seanceCommitment(...)`, and `seancePairsDigest(...)` of the pairs
   * (and their claims' PRs) the juror compared: other pairs on chain refuse it (audit gap-7-6).
   */
  | { type: "commit_votes"; seance_id: number; commitment: string; pairs_digest: string }
  /** One of "a", "b", "=" per assigned pair, claim ids flagged as spam, and the salt. */
  | { type: "reveal_votes"; seance_id: number; votes: string; spam: number[]; salt: string }
  | { type: "fund_pool"; amount: string }
  | { type: "withdraw_vested" }
  | { type: "clawback"; account: string; reason: string }
  // ---- protocol v1 (tags 30–49, MAINNET-DESIGN 3.1): staking, governance, the beacon, names and distributions. Older nodes, built before these modules, fail them with "not active yet". ----
  /** Stake `amount` drops in a validator's pool (validators are named by address). */
  | { type: "delegate"; validator: string; amount: string }
  /**
   * Start unbonding stake in a pool: exactly `shares`, or (with `shares` "0") the shares worth
   * `amount` drops at the pool's price when the tx runs (rounded up: at least `amount` unbonds).
   * One of the two is set. Naming an amount, a wallet never converts FANG to shares at a price
   * some node reported (audit gap-7-5). `withdraw_unbonded` pays it out after the unbonding period.
   */
  | { type: "undelegate"; validator: string; shares: string; amount?: string }
  | { type: "redelegate"; from_validator: string; to_validator: string; shares: string }
  | { type: "withdraw_unbonded" }
  /** A validator's owner: commission (bps) and, when non-empty, new addresses. */
  | { type: "update_validator"; commission_bps: number; p2p_addr: string; http_addr: string }
  | { type: "unjail"; validator: string }
  /** Two conflicting signed votes, hex (at most 16 KiB decoded). */
  | { type: "report_equivocation"; evidence: string }
  /** A validator's commitment to its secret for the randomness beacon of `epoch`: `beaconCommitment(...)`. */
  | { type: "beacon_commit"; epoch: number; commitment: string }
  | { type: "beacon_reveal"; epoch: number; secret: string }
  /** Put `kind` to a vote, bonding `deposit` drops; `url` (https) links the full text. */
  | { type: "propose"; kind: ProposalKind; deposit: string; title: string; url?: string }
  /** Lock `amount` drops for `conviction` (0–6) lock periods: weight 0.1× with no lock, then 1×–6× for 1, 2, 4, 8, 16, 32 periods. */
  /** `proposal_digest`: `proposalDigest(kind, title, url)` of the proposal the voter was shown; a vote on other content is refused (audit gap-7-6). */
  | { type: "vote"; proposal_id: number; aye: boolean; amount: string; conviction: number; proposal_digest: string }
  /** Release expired vote locks. */
  | { type: "unlock" }
  /** Make this account an M-of-N multisig of its owner keys (key ids). */
  | { type: "multisig_create"; owners: string[]; threshold: number }
  /** Propose an action for a multisig account (never another multisig proposal or approval). */
  | { type: "multisig_propose"; action: Action }
  /** `action_digest`: `actionDigest(action)` of the action the approver was shown; approving anything else is refused (audit gap-7-6). */
  | { type: "multisig_approve"; id: number; action_digest: string }
  | { type: "renew_name"; name: string; years: number }
  | { type: "transfer_name"; name: string; to: string }
  | { type: "release_name"; name: string }
  /** `proof`: hex 32-byte hashes from the leaf (distribution_id, address, amount) to the distribution's Merkle root. */
  | { type: "claim_distribution"; distribution_id: number; amount: string; proof: string[] }
  /** Validators: a matured claim's PR was reverted by commit `revert_sha` (40 hex). */
  | { type: "attest_revert"; claim_id: number; revert_sha: string }
  // ---- 👻 Ghost Mode (tags 50–54). Disabled until governance activates it: nodes refuse these with "Ghost Mode is off". ----
  /** Move `amount` drops into the shielded pool as the bundle's output notes. Signed; also burns the chain's `ghost_verify_fee`. */
  | { type: "shield"; amount: string; bundle: GhostBundle }
  /** Unsigned (the proof authorizes it): send it with `from`, `memo`, `public_key`, `signature` empty and `nonce` 0. Its `fee` leaves the pool. */
  | { type: "private_transfer"; bundle: GhostBundle }
  /** Unsigned: pay `amount` drops out of the pool to `to` (address or name), plus the fee. */
  | { type: "unshield"; to: string; amount: string; bundle: GhostBundle }
  /** Publish this account's 1,216-byte Ghost Mode address (hex) so wallets find it under the account's names. */
  | { type: "set_ghost_address"; address: string }
  /** Validators: vote to pause Ghost Mode (it pauses once f + 1 of the epoch's validators have). */
  | { type: "ghost_pause" }
  // ---- a set of one or two (tags 60–61): its next newcomer is needed for every quorum, so it joins only once a quorum of the members admitted it and its node showed up ----
  /** A candidate's consensus key: its node is running, in the last tenth of `epoch` (the epoch the tx lands in). Changes nothing but the readiness record; refused until the members admitted it. Nodes send it themselves. */
  | { type: "validator_ready"; epoch: number }
  /** An active member's consensus key admits candidate `validator` (an address) into the set at the end of `epoch` (the epoch the tx lands in): its operator's decision, once an epoch. */
  | { type: "admit_validator"; validator: string; epoch: number };

/**
 * What a ghost action carries (node/src/types.rs `GhostBundle`), every field lowercase hex: the
 * chain `instance` (from `/api/ghost`), the `anchor` root, two 32-byte `nullifiers` and
 * `commitments`, two note ciphertexts `enc` (1,401 bytes each) and outgoing ciphertexts `out`
 * (80 bytes each), and the Action `proof`. Proofs come from the `blockula-ghost` prover; this SDK
 * only encodes.
 */
export interface GhostBundle {
  instance: string;
  anchor: string;
  nullifiers: [string, string];
  commitments: [string, string];
  enc: [string, string];
  out: [string, string];
  proof: string;
}

/** What a `propose` asks for (node/src/types.rs `ProposalKind`): every former admin lever, parameters, spending, upgrades and features. */
export type ProposalKind =
  /** Set a governance parameter (a key of node/src/params.rs `ChainParams`, e.g. "max_validators"), within its bounds. */
  | { type: "param_change"; key: string; value: string }
  | { type: "treasury_spend"; to: string; amount: string }
  /** Open a community distribution of `total` drops, claimable with Merkle proofs until block `claim_until`. `distribution_id` is the id its tree was built for, one no distribution has yet (the node's `next_distribution_id`; at most its `max_distribution_id`). */
  | { type: "distribution"; distribution_id: number; merkle_root: string; total: string; claim_until: number }
  | { type: "set_repo"; repo: string; weight: number }
  /** milli-Soul, at most 100 Soul. */
  | { type: "grant_soul"; to: string; amount: number; reason: string }
  | { type: "flag_sybil"; account: string; reason: string }
  | { type: "reject_claim"; claim_id: number; reason: string }
  | { type: "clawback"; account: string; reason: string }
  | { type: "unlink_github"; handle: string }
  | { type: "refund_hunt"; hunt_id: number }
  | { type: "schedule_upgrade"; version: number; activation_height: number }
  | { type: "activate_feature"; feature: string }
  | { type: "deactivate_feature"; feature: string }
  /** No on-chain effect: a signal. */
  | { type: "text" }
  /** Lift a sybil flag (`flag_sybil`'s appeal path): the account can earn again; nothing the flag took comes back. */
  | { type: "unflag_sybil"; account: string; reason: string }
  /** Withdraw the upgrade to `version` scheduled at `activation_height` (and any after it) before it activates (a supermajority, no timelock). Tag 15. */
  | { type: "cancel_upgrade"; version: number; activation_height: number };

export interface UnsignedTx {
  chain_id: string;
  /**
   * The chain instance the tx is for: hex BLAKE3 digest of its genesis block (`genesis_hash` in
   * `/api/status`). Signed right after `chain_id`, so the tx can't replay on another chain that
   * reuses the id (e.g. a relaunched testnet). `Wallet.build` fills it in; offline signers pass it.
   */
  genesis_hash: string;
  /** The account acting; the signing key must be authorized on it. */
  from: string;
  nonce: number;
  /** The most the sender will pay, in drops. The tx is charged its block's base fee + `tip` (see `txCharge`). */
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
/** Domain of the unsigned ghost wire form (`private_transfer`, `unshield`). */
export const GHOST_TX_DOMAIN = "BLOCKULA_GHOST_V1";
export const GHOST_ENC_BYTES = 1401;
export const GHOST_OUT_BYTES = 80;
export const GHOST_ADDRESS_BYTES = 1216;
/** The Action circuit the node verifies proofs against (blockula-ghost `CIRCUIT_ID`). */
export const GHOST_CIRCUIT_ID = 1;
export const DROPS_PER_FANG = 1_000_000_000n;

const utf8 = new TextEncoder();

const U64_LIMIT = 1n << 64n;

/**
 * The largest value of a whole-number field a tx carries as a JSON number, not a string (ids,
 * heights, PR numbers, the nonce): 2^53 − 1, what a JavaScript number holds exactly
 * (node/src/types.rs `MAX_JSON_INT`). The chain refuses larger ones, so every stored proposal
 * kind and multisig action reads back exactly here, and its digest can be computed.
 */
export const MAX_JSON_INT = (1n << 53n) - 1n;

/** An optional field's JSON default (`#[serde(default)]` in node/src/types.rs): only when it's absent, never for `null`. */
const orDefault = <T>(v: T | undefined, d: T): T => (v === undefined ? d : v);

/**
 * An unsigned integer field as the node reads it from JSON (node/src/types.rs: a JSON number,
 * or for amounts a decimal string), checked to be in `0 ..= max`. Anything the node's JSON
 * parser would refuse is refused here, so `signingBytes`/`txBytes` never encode a value other
 * than the one in the tx object (no wrapping of negative or oversized numbers).
 */
export function uintField(v: unknown, max: bigint, what = "integer field"): bigint {
  let n: bigint;
  if (typeof v === "bigint") n = v;
  else if (typeof v === "number") {
    if (!Number.isInteger(v)) throw new RangeError(`${what} must be a whole number, got ${v}`);
    // Above 2^53 − 1 a JavaScript number may not be the value that was meant: pass a string of
    // digits (where the field can be that large).
    if (!Number.isSafeInteger(v)) {
      if (max <= MAX_JSON_INT) throw new RangeError(`${what} must be 0 to ${max}, got ${v}${v > Number.MAX_SAFE_INTEGER ? " (past 2^53 − 1: not even exact as a JavaScript number)" : ""}`);
      throw new RangeError(`${what} must be 0 to ${Number.MAX_SAFE_INTEGER} as a number (larger values as a string of digits), got ${v}`);
    }
    n = BigInt(v);
  } else if (typeof v === "string") {
    if (!/^\d+$/.test(v)) throw new RangeError(`${what} must be a decimal string of digits, got ${JSON.stringify(v)}`);
    n = BigInt(v);
  } else throw new RangeError(`${what} must be a number or a decimal string, got ${typeof v}`);
  if (n < 0n || n > max) throw new RangeError(`${what} must be 0 to ${max}, got ${n}`);
  return n;
}

/** A u64 field (`uintField` up to 2^64 − 1). */
export const u64Field = (v: unknown, what?: string) => uintField(v, U64_LIMIT - 1n, what);

class Writer {
  private parts: number[] = [];
  bytes(b: Uint8Array) { for (const x of b) this.parts.push(x); }
  /** A byte the encoder chose (tags, bools, lengths' bytes): always in range. */
  private byte(n: number) { this.parts.push(n & 0xff); }
  u8(n: unknown, what?: string) { this.byte(Number(uintField(n, 0xffn, what))); }
  u16(n: number, what?: string) { const v = Number(uintField(n, 0xffffn, what)); this.byte(v); this.byte(v >> 8); }
  u32(n: unknown, what?: string) { const v = Number(uintField(n, 0xffff_ffffn, what)); this.byte(v); this.byte(v >> 8); this.byte(v >> 16); this.byte(v >>> 24); }
  u64(n: unknown, what?: string) { this.le64(u64Field(n, what)); }
  /** A u64 the tx carries as a JSON number: at most `MAX_JSON_INT`, as the node's format rules require. */
  int(n: unknown, what: string) { this.le64(uintField(n, MAX_JSON_INT, what)); }
  private le64(v: bigint) { for (let i = 0n; i < 8n; i++) this.byte(Number((v >> (8n * i)) & 0xffn)); }
  bool(b: unknown, what = "flag") {
    if (typeof b !== "boolean") throw new TypeError(`${what} must be true or false`);
    this.byte(b ? 1 : 0);
  }
  str(s: unknown, what = "string field") {
    if (typeof s !== "string") throw new TypeError(`${what} must be a string`);
    const b = utf8.encode(s);
    if (b.length > 0xffff) throw new Error(`${what} too long to encode`);
    this.u16(b.length);
    this.bytes(b);
  }
  ids(xs: unknown, what = "id list") {
    if (!Array.isArray(xs)) throw new TypeError(`${what} must be an array`);
    this.u16(xs.length, `${what} length`);
    for (const x of xs) this.int(x, what);
  }
  /** Raw bytes of exactly `len` bytes, from hex. */
  fixed(hex: string, len: number) {
    const b = hexToBytes(hex);
    if (b.length !== len) throw new Error(`expected ${len} bytes, got ${b.length}`);
    this.bytes(b);
  }
  strs(xs: unknown, what = "string list") {
    if (!Array.isArray(xs)) throw new TypeError(`${what} must be an array`);
    this.u16(xs.length, `${what} length`);
    for (const x of xs) this.str(x, what);
  }
  done() { return Uint8Array.from(this.parts); }
}

/** A chain's genesis hash as its 32 bytes (`/api/status` `genesis_hash`, lowercase hex). */
export function genesisHashBytes(genesisHash: string): Uint8Array {
  if (typeof genesisHash !== "string" || !/^[0-9a-f]{64}$/.test(genesisHash)) {
    throw new Error("genesis_hash must be the chain's genesis hash: 32 bytes of lowercase hex (genesis_hash in /api/status)");
  }
  return hexToBytes(genesisHash);
}

/** Unsigned ghost txs (`private_transfer`, `unshield`): no key signs them, their proof authorizes them. */
export function isUnsignedAction(a: Action): boolean {
  return a.type === "private_transfer" || a.type === "unshield";
}

/**
 * The bytes a key signs (node/src/types.rs `Tx::signing_bytes`). For an unsigned ghost tx this is
 * its whole wire form instead: `BLOCKULA_GHOST_V1 ‖ chain_id ‖ genesis hash ‖ fee ‖ tip ‖ valid_until ‖ action`.
 */
export function signingBytes(tx: UnsignedTx): Uint8Array {
  const w = new Writer();
  if (isUnsignedAction(tx.action)) {
    w.bytes(utf8.encode(GHOST_TX_DOMAIN));
    w.str(tx.chain_id);
    w.bytes(genesisHashBytes(tx.genesis_hash));
    w.u64(tx.fee, "fee");
    w.u64(orDefault(tx.tip, "0"), "tip");
    w.int(tx.valid_until, "valid_until");
    encodeAction(w, tx.action);
    return w.done();
  }
  w.bytes(utf8.encode("BLOCKULA_TX_V3"));
  w.str(tx.chain_id);
  w.bytes(genesisHashBytes(tx.genesis_hash));
  w.str(tx.from);
  w.int(tx.nonce, "nonce");
  w.u64(tx.fee, "fee");
  w.u64(orDefault(tx.tip, "0"), "tip");
  w.int(tx.valid_until, "valid_until");
  encodeAction(w, tx.action);
  w.str(orDefault(tx.memo, ""), "memo");
  return w.done();
}

/** An action's tag and fields, as inside the signing bytes (node/src/types.rs `Action::encode`). */
function encodeAction(w: Writer, a: Action) {
  switch (a.type) {
    case "transfer":
      w.u8(0); w.str(a.to); w.u64(a.amount);
      break;
    case "register_name":
      w.u8(1); w.str(a.name);
      break;
    case "add_key":
      w.u8(2); w.str(a.key_id); w.str(a.label);
      if (a.role.type === "owner") w.u8(0);
      else { w.u8(1); w.u64(a.role.limit); w.int(a.role.period_blocks, "period_blocks"); w.int(a.role.expires_at, "expires_at"); }
      w.str(a.public_key); w.str(a.pop);
      break;
    case "remove_key":
      w.u8(3); w.str(a.key_id);
      break;
    case "register_validator":
      w.u8(4); w.str(a.consensus_key); w.str(a.p2p_addr); w.str(a.http_addr); w.u64(a.bond); w.str(a.pop);
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
      w.u8(10); w.str(a.repo); w.int(a.issue, "issue"); w.u64(a.amount); w.int(a.expires_at, "expires_at"); w.str(a.title);
      break;
    case "fund_hunt":
      w.u8(11); w.int(a.hunt_id, "hunt_id"); w.u64(a.amount);
      break;
    case "award_hunt":
      w.u8(12); w.int(a.hunt_id, "hunt_id"); w.str(a.to); w.int(a.pr, "pr"); w.str(a.familiar_key ?? "");
      break;
    case "cancel_hunt":
      w.u8(13); w.int(a.hunt_id, "hunt_id");
      break;
    case "register_familiar":
      w.u8(14); w.str(a.key_id); w.str(a.label);
      break;
    case "set_repo":
      w.u8(15); w.str(a.repo); w.int(a.weight, "weight");
      break;
    case "grant_soul":
      w.u8(16); w.str(a.to); w.int(a.amount, "amount"); w.str(a.reason);
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
      w.u8(20); w.str(a.repo); w.int(a.pr, "pr"); w.str(a.familiar_key ?? "");
      break;
    case "attest_claim":
      w.u8(21); w.int(a.claim_id, "claim_id"); w.bool(a.ok, "ok"); w.str(a.merge_sha); w.str(orDefault(a.note, ""));
      break;
    case "reject_claim":
      w.u8(22); w.int(a.claim_id, "claim_id"); w.str(a.reason);
      break;
    case "join_jury":
      w.u8(23); w.u64(a.bond);
      break;
    case "leave_jury":
      w.u8(24);
      break;
    case "commit_votes":
      w.u8(25); w.int(a.seance_id, "seance_id"); w.str(a.commitment); w.str(a.pairs_digest);
      break;
    case "reveal_votes":
      w.u8(26); w.int(a.seance_id, "seance_id"); w.str(a.votes); w.ids(a.spam, "spam"); w.str(a.salt);
      break;
    case "fund_pool":
      w.u8(27); w.u64(a.amount);
      break;
    case "withdraw_vested":
      w.u8(28);
      break;
    case "clawback":
      w.u8(29); w.str(a.account); w.str(a.reason);
      break;
    case "delegate":
      w.u8(30); w.str(a.validator); w.u64(a.amount);
      break;
    case "undelegate":
      w.u8(31); w.str(a.validator); w.u64(a.shares); w.u64(orDefault(a.amount, "0"));
      break;
    case "redelegate":
      w.u8(32); w.str(a.from_validator); w.str(a.to_validator); w.u64(a.shares);
      break;
    case "withdraw_unbonded":
      w.u8(33);
      break;
    case "update_validator":
      w.u8(34); w.int(a.commission_bps, "commission_bps"); w.str(a.p2p_addr); w.str(a.http_addr);
      break;
    case "unjail":
      w.u8(35); w.str(a.validator);
      break;
    case "report_equivocation":
      w.u8(36); w.str(a.evidence);
      break;
    case "beacon_commit":
      w.u8(37); w.int(a.epoch, "epoch"); w.str(a.commitment);
      break;
    case "beacon_reveal":
      w.u8(38); w.int(a.epoch, "epoch"); w.str(a.secret);
      break;
    case "propose":
      w.u8(39); encodeProposalKind(w, a.kind); w.u64(a.deposit); w.str(a.title); w.str(orDefault(a.url, ""));
      break;
    case "vote":
      w.u8(40); w.int(a.proposal_id, "proposal_id"); w.bool(a.aye, "aye"); w.u64(a.amount); w.u8(a.conviction, "conviction"); w.str(a.proposal_digest);
      break;
    case "unlock":
      w.u8(41);
      break;
    case "multisig_create":
      w.u8(42); w.strs(a.owners); w.u8(a.threshold, "threshold");
      break;
    case "multisig_propose":
      if (a.action.type === "multisig_propose") throw new Error("a multisig proposal can't contain another");
      w.u8(43); encodeAction(w, a.action);
      break;
    case "multisig_approve":
      w.u8(44); w.int(a.id, "id"); w.str(a.action_digest);
      break;
    case "renew_name":
      w.u8(45); w.str(a.name); w.u8(a.years, "years");
      break;
    case "transfer_name":
      w.u8(46); w.str(a.name); w.str(a.to);
      break;
    case "release_name":
      w.u8(47); w.str(a.name);
      break;
    case "claim_distribution":
      w.u8(48); w.int(a.distribution_id, "distribution_id"); w.u64(a.amount); w.strs(a.proof);
      break;
    case "attest_revert":
      w.u8(49); w.int(a.claim_id, "claim_id"); w.str(a.revert_sha);
      break;
    case "shield":
      w.u8(50); w.u64(a.amount); encodeBundle(w, a.bundle);
      break;
    case "private_transfer":
      w.u8(51); encodeBundle(w, a.bundle);
      break;
    case "unshield":
      w.u8(52); w.str(a.to); w.u64(a.amount); encodeBundle(w, a.bundle);
      break;
    case "set_ghost_address":
      w.u8(53); w.fixed(a.address, GHOST_ADDRESS_BYTES);
      break;
    case "ghost_pause":
      w.u8(54);
      break;
    case "validator_ready":
      w.u8(60); w.int(a.epoch, "epoch");
      break;
    case "admit_validator":
      w.u8(61); w.str(a.validator); w.int(a.epoch, "epoch");
      break;
    default:
      throw new Error(`unknown action type ${(a as { type: string }).type}`);
  }
}

/** A bundle without its proof, raw and in wire order: what the sighash covers. */
function encodeBundlePublic(w: Writer, b: GhostBundle) {
  w.fixed(b.instance, 32);
  w.fixed(b.anchor, 32);
  for (const x of [...b.nullifiers, ...b.commitments]) w.fixed(x, 32);
  for (const x of b.enc) w.fixed(x, GHOST_ENC_BYTES);
  for (const x of b.out) w.fixed(x, GHOST_OUT_BYTES);
}

/** node/src/types.rs `GhostBundle::encode`: the fixed fields, then the proof with a u32 length. */
function encodeBundle(w: Writer, b: GhostBundle) {
  encodeBundlePublic(w, b);
  const proof = hexToBytes(b.proof);
  w.u32(proof.length);
  w.bytes(proof);
}

/**
 * What a ghost tx's Action proof is bound to (node/src/types.rs `Tx::ghost_sighash`):
 * BLAKE3-KDF("blockula ghost v1 tx sighash", chain_id ‖ genesis hash ‖ tag ‖ valid_until ‖ fee ‖
 * tip ‖ the tag's own fields (shield: from, nonce, amount; unshield: to, amount) ‖ the bundle
 * without its proof ‖ circuit id). A wallet proves with this as the witness's sighash, then fills
 * in the proof.
 */
export function ghostSighash(tx: UnsignedTx): Uint8Array {
  const a = tx.action;
  if (a.type !== "shield" && a.type !== "private_transfer" && a.type !== "unshield") throw new Error("not a ghost transfer");
  const w = new Writer();
  w.str(tx.chain_id);
  w.bytes(genesisHashBytes(tx.genesis_hash));
  w.u8(a.type === "shield" ? 50 : a.type === "private_transfer" ? 51 : 52);
  w.int(tx.valid_until, "valid_until");
  w.u64(tx.fee, "fee");
  w.u64(orDefault(tx.tip, "0"), "tip");
  if (a.type === "shield") { w.str(tx.from); w.int(tx.nonce, "nonce"); w.u64(a.amount); }
  if (a.type === "unshield") { w.str(a.to); w.u64(a.amount); }
  encodeBundlePublic(w, a.bundle);
  w.u32(GHOST_CIRCUIT_ID);
  return blake3(w.done(), { context: utf8.encode("blockula ghost v1 tx sighash") });
}

/** A proposal kind's own tag and fields (node/src/types.rs `ProposalKind::encode`). */
function encodeProposalKind(w: Writer, k: ProposalKind) {
  switch (k.type) {
    case "param_change":
      w.u8(0); w.str(k.key); w.u64(k.value);
      break;
    case "treasury_spend":
      w.u8(1); w.str(k.to); w.u64(k.amount);
      break;
    case "distribution":
      w.u8(2); w.int(k.distribution_id, "distribution_id"); w.str(k.merkle_root); w.u64(k.total); w.int(k.claim_until, "claim_until");
      break;
    case "set_repo":
      w.u8(3); w.str(k.repo); w.int(k.weight, "weight");
      break;
    case "grant_soul":
      w.u8(4); w.str(k.to); w.int(k.amount, "amount"); w.str(k.reason);
      break;
    case "flag_sybil":
      w.u8(5); w.str(k.account); w.str(k.reason);
      break;
    case "reject_claim":
      w.u8(6); w.int(k.claim_id, "claim_id"); w.str(k.reason);
      break;
    case "clawback":
      w.u8(7); w.str(k.account); w.str(k.reason);
      break;
    case "unlink_github":
      w.u8(8); w.str(k.handle);
      break;
    case "refund_hunt":
      w.u8(9); w.int(k.hunt_id, "hunt_id");
      break;
    case "schedule_upgrade":
      w.u8(10); w.u32(k.version, "version"); w.int(k.activation_height, "activation_height");
      break;
    case "activate_feature":
      w.u8(11); w.str(k.feature);
      break;
    case "deactivate_feature":
      w.u8(12); w.str(k.feature);
      break;
    case "text":
      w.u8(13);
      break;
    case "unflag_sybil":
      w.u8(14); w.str(k.account); w.str(k.reason);
      break;
    case "cancel_upgrade":
      w.u8(15); w.u32(k.version, "version"); w.int(k.activation_height, "activation_height");
      break;
    default:
      throw new Error(`unknown proposal kind ${(k as { type: string }).type}`);
  }
}

/**
 * What a Séance juror seals before revealing (node/src/types.rs `seance_commitment`): hex
 * BLAKE3 of the Séance id, the juror's account, the votes, spam flags and a secret salt.
 */
export function seanceCommitment(seanceId: number, juror: string, votes: string, spam: number[], salt: string): string {
  const w = new Writer();
  w.bytes(utf8.encode("BLOCKULA_SEANCE_V1"));
  w.int(seanceId, "seance_id");
  w.str(juror);
  w.str(votes);
  w.ids(spam, "spam");
  w.str(salt);
  return bytesToHex(blake3(w.done()));
}

/**
 * A validator's randomness-beacon commitment (node/src/beacon.rs `commitment`): hex BLAKE3 in
 * derive-key mode ("blockula beacon commitment v1") over the chain id, the target epoch, the
 * validator's address and its 32-byte hex secret. Strings are u64-length-prefixed, integers
 * u64 little-endian.
 */
export function beaconCommitment(chainId: string, epoch: number, validator: string, secret: string): string {
  const u64 = (n: number) => {
    const b = new Uint8Array(8);
    // setBigUint64 would wrap a negative or oversized value: check it first.
    new DataView(b.buffer).setBigUint64(0, u64Field(n, "epoch"), true);
    return b;
  };
  const str = (s: string) => {
    const b = utf8.encode(s);
    return concatBytes(u64(b.length), b);
  };
  const key = hexToBytes(secret);
  if (key.length !== 32) throw new Error("a beacon secret is 32 bytes");
  return bytesToHex(blake3(concatBytes(str(chainId), u64(epoch), str(validator), key), { context: utf8.encode("blockula beacon commitment v1") }));
}

/** "2.5" Soul → 2500 milli-Soul. */
export function parseSoul(soul: string | number): number {
  const s = String(soul).trim();
  if (!/^\d+(\.\d{1,3})?$/.test(s)) throw new Error(`invalid Soul amount: ${s}`);
  const [whole, frac = ""] = s.split(".");
  return Number(whole) * 1000 + Number(frac.padEnd(3, "0"));
}

/**
 * What a `multisig_approve` binds (node/src/types.rs `action_digest`): BLAKE3 (derive-key mode,
 * context "blockula multisig action v1") of the action's encoding, as inside the signing bytes.
 * Compute it from the action you show the approver (`client.multisig(account)` `pending[].action`).
 */
export function actionDigest(action: Action): string {
  return digestOf("multisig action", "blockula multisig action v1", (w) => encodeAction(w, action));
}

/**
 * What a `vote` binds (node/src/types.rs `proposal_digest`): the proposal's kind (accounts as
 * stored, resolved), title and link. Compute it from the proposal you show the voter.
 */
export function proposalDigest(kind: ProposalKind, title: string, url = ""): string {
  return digestOf("proposal", "blockula proposal v1", (w) => {
    encodeProposalKind(w, kind);
    w.str(title);
    w.str(url);
  });
}

/**
 * BLAKE3 (derive-key mode, `context`) of what `encode` writes. A field out of range (a JSON
 * number past 2^53 − 1 arrives rounded, and can't be the value the chain stored) fails with
 * what's wrong and where it likely came from, not a bare encoder error (review-r2-clients-1).
 */
function digestOf(what: string, context: string, encode: (w: Writer) => void): string {
  const w = new Writer();
  try {
    encode(w);
  } catch (e) {
    if (e instanceof RangeError) throw new RangeError(`can't compute this ${what}'s digest: ${e.message}. The chain stores no ${what} with a whole number past 2^53 − 1 (the most a JSON number carries exactly), so the node it was read from may be misreporting it: read it from another node`);
    throw e;
  }
  return bytesToHex(blake3(w.done(), { context: utf8.encode(context) }));
}

/** One claim of a Séance pair, as the juror judged it. */
export interface SeanceClaimRef { id: number; repo: string; pr: number }

/**
 * What a `commit_votes` binds (node/src/types.rs `seance_pairs_digest`): the Séance, the juror and,
 * per assigned pair in order, both claims' ids, repos and PR numbers.
 */
export function seancePairsDigest(seanceId: number, juror: string, pairs: [SeanceClaimRef, SeanceClaimRef][]): string {
  const w = new Writer();
  w.int(seanceId, "seance_id");
  w.str(juror);
  w.u16(pairs.length);
  for (const pair of pairs) for (const c of pair) { w.int(c.id, "claim id"); w.str(c.repo); w.int(c.pr, "pr"); }
  return bytesToHex(blake3(w.done(), { context: utf8.encode("blockula seance pairs v1") }));
}

/** FIPS 204 context string for a key's proof of possession in `add_key` (node/src/types.rs `KEY_POP_CONTEXT`). */
export const KEY_POP_CONTEXT = utf8.encode("blockula/key-pop/v1");

/**
 * What a key signs to consent to being added to `account` (node/src/types.rs `key_pop_message`):
 * the chain instance, the account and the key's own id (audit gap-7-2).
 */
export function keyPopMessage(chainId: string, genesisHash: string, account: string, keyId: string): Uint8Array {
  const w = new Writer();
  w.bytes(utf8.encode("BLOCKULA_KEY_POP_V1"));
  for (const s of [chainId, genesisHash, account, keyId]) w.str(s);
  return w.done();
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

/** Wire size in bytes (node/src/types.rs `Tx::size`): signing bytes plus the key and signature (none for an unsigned ghost tx). Fees are priced on it. */
export function txSize(tx: UnsignedTx): number {
  return signingBytes(tx).length + (isUnsignedAction(tx.action) ? 0 : PUBLIC_KEY_LEN + SIGNATURE_LEN);
}

/**
 * A block's base fee (node/src/econ.rs `FeeSlot`): `flat` drops per tx plus `rate` milli-drops per
 * byte. It moves with block fullness and is fixed 17 blocks ahead; `/api/fees` serves the upcoming
 * ones and `suggested`, the highest of them (what `Wallet.build` prices txs at).
 */
export interface BaseFee {
  flat: string | bigint;
  rate: string | bigint;
}

/** The base fee at its genesis floor: 10,000 drops + 10 per byte (`min_fee`). */
export const FLOOR_BASE_FEE: BaseFee = { flat: 10_000n, rate: 10_000n };

/** Mirrors `FeeSlot::fee` in node/src/econ.rs: `flat + rate × size / 1000`, rounded down. Burned. */
export function baseFee(tx: UnsignedTx, fee: BaseFee = FLOOR_BASE_FEE): bigint {
  return BigInt(fee.flat) + (BigInt(fee.rate) * BigInt(txSize(tx))) / 1000n;
}

/** Mirrors `min_fee` in node/src/types.rs: the base fee at its floor (an idle chain's). */
export function minFee(tx: UnsignedTx): bigint {
  return baseFee(tx, FLOOR_BASE_FEE);
}

/** The chain's default `tip_cap_multiple` (node/src/params.rs): a tip may be at most this many times the base fee of the block that includes it. */
export const DEFAULT_TIP_CAP_MULTIPLE = 20;

/**
 * The most this tx may tip in a block with this base fee (node/src/econ.rs `max_tip`): `multiple` × its base fee. Nodes refuse
 * a bigger tip. `/api/fees` serves the chain's `tip_cap_multiple` and `lowest`, the base fee a tip fits under in any upcoming block.
 * Tips are also paid only from transferable FANG (locks hold them), and a multisig's owner keys or a validator's consensus key
 * acting for its owner can't tip at all.
 */
export function maxTip(tx: UnsignedTx, fee: BaseFee = FLOOR_BASE_FEE, multiple: number | bigint = DEFAULT_TIP_CAP_MULTIPLE): bigint {
  return BigInt(multiple) * baseFee(tx, fee);
}

/** What the tx is charged in a block with this base fee (node/src/econ.rs `charge`): base fee + tip. Its `fee` must cover it; nothing more is debited. */
export function txCharge(tx: UnsignedTx, fee: BaseFee = FLOOR_BASE_FEE): bigint {
  return baseFee(tx, fee) + u64Field(orDefault(tx.tip, "0"), "tip");
}

/**
 * Wire form (node/src/types.rs `Tx::to_bytes`): signing bytes ‖ public key (1,952 B) ‖ signature (3,309 B), or just the
 * unsigned form for an unsigned ghost tx. Blocks and gossip carry this; `POST /api/tx` takes it as `application/octet-stream`.
 */
export function txBytes(tx: Tx): Uint8Array {
  const sb = signingBytes(tx);
  if (isUnsignedAction(tx.action)) return sb;
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
