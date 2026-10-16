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
  | { type: "clawback"; account: string; reason: string }
  // ---- protocol v1 (tags 30–49, MAINNET-DESIGN 3.1): staking, governance, the beacon, names and distributions. Older nodes, built before these modules, fail them with "not active yet". ----
  /** Stake `amount` drops in a validator's pool (validators are named by address). */
  | { type: "delegate"; validator: string; amount: string }
  /** Start unbonding `shares` of a pool; `withdraw_unbonded` pays them out after the unbonding period. */
  | { type: "undelegate"; validator: string; shares: string }
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
  | { type: "vote"; proposal_id: number; aye: boolean; amount: string; conviction: number }
  /** Release expired vote locks. */
  | { type: "unlock" }
  /** Make this account an M-of-N multisig of its owner keys (key ids). */
  | { type: "multisig_create"; owners: string[]; threshold: number }
  /** Propose an action for a multisig account (never another multisig proposal or approval). */
  | { type: "multisig_propose"; action: Action }
  | { type: "multisig_approve"; id: number }
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
  | { type: "ghost_pause" };

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
  /** Open a community distribution of `total` drops, claimable with Merkle proofs until block `claim_until`. `distribution_id` is the id its tree was built for, which must be the proposal's (the node's `next_id`). */
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
  | { type: "text" };

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

class Writer {
  private parts: number[] = [];
  bytes(b: Uint8Array) { for (const x of b) this.parts.push(x); }
  u8(n: number) { this.parts.push(n & 0xff); }
  u16(n: number) { this.u8(n); this.u8(n >> 8); }
  u32(n: number) { this.u16(n & 0xffff); this.u16(n >>> 16); }
  u64(n: bigint) { for (let i = 0n; i < 8n; i++) this.u8(Number((n >> (8n * i)) & 0xffn)); }
  str(s: string) {
    const b = utf8.encode(s);
    if (b.length > 0xffff) throw new Error("string field too long to encode");
    this.u16(b.length);
    this.bytes(b);
  }
  ids(xs: number[]) { this.u16(xs.length); for (const x of xs) this.u64(BigInt(x)); }
  /** Raw bytes of exactly `len` bytes, from hex. */
  fixed(hex: string, len: number) {
    const b = hexToBytes(hex);
    if (b.length !== len) throw new Error(`expected ${len} bytes, got ${b.length}`);
    this.bytes(b);
  }
  strs(xs: string[]) { this.u16(xs.length); for (const x of xs) this.str(x); }
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
    w.u64(BigInt(tx.fee));
    w.u64(BigInt(tx.tip ?? "0"));
    w.u64(BigInt(tx.valid_until));
    encodeAction(w, tx.action);
    return w.done();
  }
  w.bytes(utf8.encode("BLOCKULA_TX_V3"));
  w.str(tx.chain_id);
  w.bytes(genesisHashBytes(tx.genesis_hash));
  w.str(tx.from);
  w.u64(BigInt(tx.nonce));
  w.u64(BigInt(tx.fee));
  w.u64(BigInt(tx.tip ?? "0"));
  w.u64(BigInt(tx.valid_until));
  encodeAction(w, tx.action);
  w.str(tx.memo);
  return w.done();
}

/** An action's tag and fields, as inside the signing bytes (node/src/types.rs `Action::encode`). */
function encodeAction(w: Writer, a: Action) {
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
    case "delegate":
      w.u8(30); w.str(a.validator); w.u64(BigInt(a.amount));
      break;
    case "undelegate":
      w.u8(31); w.str(a.validator); w.u64(BigInt(a.shares));
      break;
    case "redelegate":
      w.u8(32); w.str(a.from_validator); w.str(a.to_validator); w.u64(BigInt(a.shares));
      break;
    case "withdraw_unbonded":
      w.u8(33);
      break;
    case "update_validator":
      w.u8(34); w.u64(BigInt(a.commission_bps)); w.str(a.p2p_addr); w.str(a.http_addr);
      break;
    case "unjail":
      w.u8(35); w.str(a.validator);
      break;
    case "report_equivocation":
      w.u8(36); w.str(a.evidence);
      break;
    case "beacon_commit":
      w.u8(37); w.u64(BigInt(a.epoch)); w.str(a.commitment);
      break;
    case "beacon_reveal":
      w.u8(38); w.u64(BigInt(a.epoch)); w.str(a.secret);
      break;
    case "propose":
      w.u8(39); encodeProposalKind(w, a.kind); w.u64(BigInt(a.deposit)); w.str(a.title); w.str(a.url ?? "");
      break;
    case "vote":
      w.u8(40); w.u64(BigInt(a.proposal_id)); w.u8(a.aye ? 1 : 0); w.u64(BigInt(a.amount)); w.u8(a.conviction);
      break;
    case "unlock":
      w.u8(41);
      break;
    case "multisig_create":
      w.u8(42); w.strs(a.owners); w.u8(a.threshold);
      break;
    case "multisig_propose":
      if (a.action.type === "multisig_propose") throw new Error("a multisig proposal can't contain another");
      w.u8(43); encodeAction(w, a.action);
      break;
    case "multisig_approve":
      w.u8(44); w.u64(BigInt(a.id));
      break;
    case "renew_name":
      w.u8(45); w.str(a.name); w.u8(a.years);
      break;
    case "transfer_name":
      w.u8(46); w.str(a.name); w.str(a.to);
      break;
    case "release_name":
      w.u8(47); w.str(a.name);
      break;
    case "claim_distribution":
      w.u8(48); w.u64(BigInt(a.distribution_id)); w.u64(BigInt(a.amount)); w.strs(a.proof);
      break;
    case "attest_revert":
      w.u8(49); w.u64(BigInt(a.claim_id)); w.str(a.revert_sha);
      break;
    case "shield":
      w.u8(50); w.u64(BigInt(a.amount)); encodeBundle(w, a.bundle);
      break;
    case "private_transfer":
      w.u8(51); encodeBundle(w, a.bundle);
      break;
    case "unshield":
      w.u8(52); w.str(a.to); w.u64(BigInt(a.amount)); encodeBundle(w, a.bundle);
      break;
    case "set_ghost_address":
      w.u8(53); w.fixed(a.address, GHOST_ADDRESS_BYTES);
      break;
    case "ghost_pause":
      w.u8(54);
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
  w.u64(BigInt(tx.valid_until));
  w.u64(BigInt(tx.fee));
  w.u64(BigInt(tx.tip ?? "0"));
  if (a.type === "shield") { w.str(tx.from); w.u64(BigInt(tx.nonce)); w.u64(BigInt(a.amount)); }
  if (a.type === "unshield") { w.str(a.to); w.u64(BigInt(a.amount)); }
  encodeBundlePublic(w, a.bundle);
  w.u32(GHOST_CIRCUIT_ID);
  return blake3(w.done(), { context: utf8.encode("blockula ghost v1 tx sighash") });
}

/** A proposal kind's own tag and fields (node/src/types.rs `ProposalKind::encode`). */
function encodeProposalKind(w: Writer, k: ProposalKind) {
  switch (k.type) {
    case "param_change":
      w.u8(0); w.str(k.key); w.u64(BigInt(k.value));
      break;
    case "treasury_spend":
      w.u8(1); w.str(k.to); w.u64(BigInt(k.amount));
      break;
    case "distribution":
      w.u8(2); w.u64(BigInt(k.distribution_id)); w.str(k.merkle_root); w.u64(BigInt(k.total)); w.u64(BigInt(k.claim_until));
      break;
    case "set_repo":
      w.u8(3); w.str(k.repo); w.u64(BigInt(k.weight));
      break;
    case "grant_soul":
      w.u8(4); w.str(k.to); w.u64(BigInt(k.amount)); w.str(k.reason);
      break;
    case "flag_sybil":
      w.u8(5); w.str(k.account); w.str(k.reason);
      break;
    case "reject_claim":
      w.u8(6); w.u64(BigInt(k.claim_id)); w.str(k.reason);
      break;
    case "clawback":
      w.u8(7); w.str(k.account); w.str(k.reason);
      break;
    case "unlink_github":
      w.u8(8); w.str(k.handle);
      break;
    case "refund_hunt":
      w.u8(9); w.u64(BigInt(k.hunt_id));
      break;
    case "schedule_upgrade":
      w.u8(10); w.u32(k.version); w.u64(BigInt(k.activation_height));
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
  w.u64(BigInt(seanceId));
  w.str(juror);
  w.str(votes);
  w.ids(spam);
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
    new DataView(b.buffer).setBigUint64(0, BigInt(n), true);
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
  return baseFee(tx, fee) + BigInt(tx.tip ?? "0");
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
