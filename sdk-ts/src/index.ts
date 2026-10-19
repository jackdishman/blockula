import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, randomBytes, utf8ToBytes } from "@noble/hashes/utils.js";
import { base64urlnopad, bech32m } from "@scure/base";
import { DEFAULT_TIP_CAP_MULTIPLE, KEY_POP_CONTEXT, PUBLIC_KEY_LEN, SIGNATURE_LEN, actionDigest, baseFee, formatFang, keyPopMessage, maxTip, parseFang, parseSoul, proposalDigest, seanceCommitment, seancePairsDigest, signingBytes, type SeanceClaimRef, type Action, type BaseFee, type KeyRole, type ProposalKind, type Tx, type UnsignedTx } from "./tx.js";
import { NAMESPACES, checkProofResponse, decodeAccount, decodeName, type ProofResponse, type ProvenRecord } from "./proof.js";
import { describeKind, reviewAction } from "./describe.js";

export * from "./tx.js";
export * from "./proof.js";
export * from "./distribution.js";
export * from "./release.js";
export * from "./describe.js";

export const DEFAULT_RPC = "http://localhost:7070";
/** Address prefixes: `boo` on mainnet (and dev chains), `tboo` on public testnets. */
export const KNOWN_PREFIXES = ["boo", "tboo"] as const;
export const DEFAULT_PREFIX = "boo";
/** Mainnet's chain id (node/src/chain.rs `MAINNET_CHAIN_ID`). */
export const MAINNET_CHAIN_ID = "blockula-1";
/** How long a transaction stays valid by default: 600 blocks (10 minutes at 1 s blocks). */
export const DEFAULT_VALID_FOR_BLOCKS = 600;

/** The address of a public key: bech32m(prefix, BLAKE3(public key)). */
export function addressFromPublicKey(publicKey: Uint8Array, prefix: string = DEFAULT_PREFIX): string {
  if (!(KNOWN_PREFIXES as readonly string[]).includes(prefix)) throw new Error(`unknown address prefix ${prefix}`);
  return bech32m.encode(prefix, bech32m.toWords(blake3(publicKey)));
}

/** A key's id: BLAKE3(public key) as hex. Same 32 bytes as inside the `boo1…` address it derives. */
export function keyId(publicKey: Uint8Array): string {
  return bytesToHex(blake3(publicKey));
}

/** The prefix of a canonical address (`boo`, `tboo`), or `undefined` if `s` isn't one. */
export function addressPrefix(s: string): string | undefined {
  // Addresses are canonical lowercase; the node rejects any other casing.
  if (s !== s.toLowerCase()) return undefined;
  try {
    const { prefix, words } = bech32m.decode(s as `${string}1${string}`);
    return (KNOWN_PREFIXES as readonly string[]).includes(prefix) && bech32m.fromWords(words).length === 32 ? prefix : undefined;
  } catch {
    return undefined;
  }
}

/** Whether `s` is an address: with any known prefix, or exactly `prefix` when given. */
export function isAddress(s: string, prefix?: string): boolean {
  const p = addressPrefix(s);
  return p !== undefined && (prefix === undefined || p === prefix);
}

/** An amount of FANG: a decimal string ("1.5"), a number, or bigint drops. */
export type Amount = string | number | bigint;

const positive = (drops: bigint, what: string) => {
  if (drops <= 0n) throw new Error(`${what} more than 0`);
  if (drops >= 1n << 64n) throw new Error(`${what}: amount too large`);
  return drops;
};

const wholeNumber = (n: number, what: string) => {
  if (!Number.isSafeInteger(n) || n < 0) throw new Error(`${what} must be a whole number`);
  return n;
};

const hexArg = (s: string, what: string) => {
  const h = s.trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(h)) throw new Error(`${what} must be 32 bytes of hex`);
  return h;
};

/** A `.boo` name as the chain stores it (node/src/types.rs `normalize_name`): lowercase, without the suffix. */
export function bareName(name: string): string {
  const n = name.trim().toLowerCase().replace(/\.boo$/, "");
  if (n.length < 3 || n.length > 32) throw new Error("names must be 3–32 characters");
  if (!/^[a-z0-9-]+$/.test(n)) throw new Error("names may only use a-z, 0-9 and '-'");
  if (n.startsWith("-") || n.endsWith("-")) throw new Error("names can't start or end with '-'");
  return n;
}

/**
 * The most `Wallet.propose` bonds by default without being told (audit gap-7-5): mainnet's genesis
 * `proposal_deposit` (node/src/params.rs). A node reporting more is refused; pass `deposit`.
 */
export const DEFAULT_MAX_PROPOSAL_DEPOSIT = 1_000n * 1_000_000_000n;

/** Highest vote conviction (node/src/params.rs `MAX_CONVICTION`). */
export const MAX_CONVICTION = 6;
/** Lock periods a vote at each conviction locks its amount for (params.rs `CONVICTION_PERIODS`) … */
export const CONVICTION_PERIODS = [0, 1, 2, 4, 8, 16, 32] as const;
/** … and its weight in tenths of the amount (`CONVICTION_TENTHS`): 0.1× with no lock, then 1×–6×. */
export const CONVICTION_TENTHS = [1, 10, 20, 30, 40, 50, 60] as const;

/** A vote's weight in drops: `amount` × the conviction multiplier (params.rs `vote_weight`). */
export function convictionWeight(amount: Amount, conviction: number): bigint {
  const t = CONVICTION_TENTHS[conviction];
  if (t === undefined) throw new Error(`conviction must be 0–${MAX_CONVICTION}`);
  return (parseFang(amount) * BigInt(t)) / 10n;
}

/**
 * Builders for `Wallet.propose` kinds (node/src/types.rs `ProposalKind`). Amounts are FANG (or
 * bigint drops), Soul is in Soul ("2.5"); names in `to`/`account` are resolved when proposing.
 */
export const proposal = {
  /** Set a governance parameter (a key of `/api/params`, e.g. "max_validators") to a raw value (drops, blocks, bps …), within its bounds. */
  paramChange(key: string, value: bigint | number | string): ProposalKind {
    const v = BigInt(value);
    if (!/^[a-z0-9_]{1,64}$/.test(key)) throw new Error("parameter keys are 1–64 characters of a-z, 0-9 and _");
    if (v < 0n || v >= 1n << 64n) throw new Error("parameter values are 0 to 2^64 - 1");
    return { type: "param_change", key, value: v.toString() };
  },
  /** Pay from the treasury (spends above a share of the treasury need a supermajority). */
  treasurySpend(to: string, amount: Amount): ProposalKind {
    return { type: "treasury_spend", to, amount: positive(parseFang(amount), "spend").toString() };
  },
  /**
   * Open a community distribution of `total`, claimable with Merkle proofs against `merkleRoot` until block `claimUntil`.
   * `distributionId` is the id the tree was built for (`buildDistribution(id, …)`): one no distribution has yet,
   * at most 1,000 past the next proposal id (`client.proposals()`'s `next_distribution_id` is free, and
   * `max_distribution_id` is the largest allowed; the range only grows). Other proposals landing first don't change
   * it, so a multisig can take its time approving; if a distribution with that id opens first, the node refuses this
   * one (nothing is escrowed), or it fails when it runs. Claims close at most 730 days after the proposal's vote ends.
   */
  distribution(distributionId: number, merkleRoot: string, total: Amount, claimUntil: number): ProposalKind {
    return { type: "distribution", distribution_id: wholeNumber(distributionId, "distributionId"), merkle_root: hexArg(merkleRoot, "merkleRoot"), total: positive(parseFang(total), "distribute").toString(), claim_until: wholeNumber(claimUntil, "claimUntil") };
  },
  /** Put a repo in the contribution pool (weight 1–100; 0 removes it). */
  setRepo(repo: string, weight: number): ProposalKind {
    if (!/^[A-Za-z0-9-]{1,39}\/[A-Za-z0-9._-]{1,100}$/.test(repo)) throw new Error("repo must be owner/name");
    if (!Number.isInteger(weight) || weight < 0 || weight > 100) throw new Error("weight must be 0–100");
    return { type: "set_repo", repo, weight };
  },
  /** Grant Soul (in Soul, "2.5"; at most 100 per grant). */
  grantSoul(to: string, soul: string | number, reason: string): ProposalKind {
    return { type: "grant_soul", to, amount: parseSoul(soul), reason };
  },
  flagSybil(account: string, reason: string): ProposalKind {
    return { type: "flag_sybil", account, reason };
  },
  /** Lift a sybil flag: the appeal path for `flagSybil` (nothing the flag took comes back). */
  unflagSybil(account: string, reason: string): ProposalKind {
    return { type: "unflag_sybil", account, reason };
  },
  rejectClaim(claimId: number, reason: string): ProposalKind {
    return { type: "reject_claim", claim_id: wholeNumber(claimId, "claimId"), reason };
  },
  clawback(account: string, reason: string): ProposalKind {
    return { type: "clawback", account, reason };
  },
  unlinkGithub(handle: string): ProposalKind {
    return { type: "unlink_github", handle };
  },
  /** Refund every funder of a hunt (escrow can be refunded, never redirected). */
  refundHunt(huntId: number): ProposalKind {
    return { type: "refund_hunt", hunt_id: wholeNumber(huntId, "huntId") };
  },
  /**
   * Switch to protocol `version` at block `activationHeight` (needs a supermajority). The version is newer than
   * every one scheduled or withdrawn (`client.upgrade()`'s `withdrawn.highest_version`) and at most 16 past the
   * newest scheduled, withdrawn or superseded (`next_version` is the number to use); the activation is at most
   * 365 days after the proposal's vote ends.
   */
  scheduleUpgrade(version: number, activationHeight: number): ProposalKind {
    if (!Number.isInteger(version) || version < 1 || version > 0xffffffff) throw new Error("version must be a positive 32-bit number");
    return { type: "schedule_upgrade", version, activation_height: wholeNumber(activationHeight, "activationHeight") };
  },
  /**
   * Withdraw the upgrade to protocol `version` scheduled at block `activationHeight` (see
   * `client.upgrade()`), and any scheduled after it, before it activates. Needs a supermajority;
   * there's no timelock (it runs the block after its vote), and it must run at least 64 blocks
   * before the activation height, so propose it at least `voting_blocks + 65` blocks ahead.
   */
  cancelUpgrade(version: number, activationHeight: number): ProposalKind {
    if (!Number.isInteger(version) || version < 1 || version > 0xffffffff) throw new Error("version must be a positive 32-bit number");
    if (wholeNumber(activationHeight, "activationHeight") < 1) throw new Error("activationHeight is the block the upgrade is scheduled at");
    return { type: "cancel_upgrade", version, activation_height: activationHeight };
  },
  /** Turn on a feature flag (e.g. "ghost"). */
  activateFeature(feature: string): ProposalKind {
    return { type: "activate_feature", feature };
  },
  deactivateFeature(feature: string): ProposalKind {
    return { type: "deactivate_feature", feature };
  },
  /** A signal with no on-chain effect. */
  text(): ProposalKind {
    return { type: "text" };
  },
};

// ---- Readers for the protocol v1 endpoints: they take what the node serves (node/src/api.rs)
// and return plain shapes with amounts as decimal strings of drops. They don't throw on a
// missing field (an older node, or a page that hasn't loaded): it reads as empty or zero. ----

/** A JSON object, or an empty one. */
const obj = (v: unknown): Record<string, any> => (v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, any>) : {});
const arr = (v: unknown): any[] => (Array.isArray(v) ? v : []);
/** Drops as a decimal string: from a string or number of drops, or "0". */
const drops = (v: unknown): string => {
  if (typeof v === "bigint") return v.toString();
  if (typeof v === "number" && Number.isSafeInteger(v) && v >= 0) return String(v);
  if (typeof v === "string" && /^\d+$/.test(v)) return v;
  return "0";
};
/** An address from a label (`{address, name}`) or a plain string. */
const addressOf = (v: unknown): string => (typeof v === "string" ? v : String(obj(v).address ?? ""));
const nameOf = (v: unknown): string | null => (typeof v === "object" && v ? (obj(v).name ?? null) : null);
const num = (v: unknown): number | undefined => (typeof v === "number" ? v : typeof v === "string" && /^\d+$/.test(v) ? Number(v) : undefined);

/** One pool an account has stake in (`/api/staking/{account}` `delegations[]`). */
export interface StakeDelegation {
  validator: string;
  validator_name: string | null;
  /** Pool shares held (what `undelegate` takes). */
  shares: string;
  /** Their current value, in drops. */
  value: string;
  /** What went in, less what sales and slashes took (value above it is earnings). */
  principal: string;
  /** This is the validator's owner's self-bond. */
  self_bond: boolean;
}

/** Stake on its way out (`entries[]` of type `unbonding`): paid out by `withdrawUnbonded` from `completes_at`. */
export interface StakeUnbonding {
  id: number;
  validator: string;
  amount: string;
  started_at: number;
  completes_at: number;
  ready: boolean;
}

/** Stake moved between pools (`entries[]` of type `redelegation`): slashable by `from` until `completes_at`. */
export interface StakeRedelegation {
  id: number;
  from: string;
  to: string;
  amount: string;
  completes_at: number;
}

/** An account's stake, from `/api/staking/{account}`. */
export interface StakeSummary {
  delegations: StakeDelegation[];
  unbonding: StakeUnbonding[];
  redelegations: StakeRedelegation[];
  /** Total value staked, in drops. */
  bonded: string;
  /** Total unbonding, in drops. */
  unbonding_total: string;
  /** Unbonding that has finished (what `withdrawUnbonded` pays now), in drops. */
  withdrawable: string;
  /** The node's plain-English summary. */
  summary: string;
}

export function stakeSummary(raw: unknown): StakeSummary {
  const r = obj(raw);
  const delegations: StakeDelegation[] = arr(r.delegations).map((d0) => {
    const d = obj(d0);
    return { validator: addressOf(d.validator), validator_name: nameOf(d.validator), shares: drops(d.shares), value: drops(d.value), principal: drops(d.principal), self_bond: d.self_bond === true };
  });
  const entries = arr(r.entries).map(obj);
  const unbonding: StakeUnbonding[] = entries.filter((e) => e.type === "unbonding").map((e) => ({ id: num(e.id) ?? 0, validator: addressOf(e.validator), amount: drops(e.amount), started_at: num(e.started_at) ?? 0, completes_at: num(e.release_at) ?? 0, ready: e.ready === true }));
  const redelegations: StakeRedelegation[] = entries.filter((e) => e.type === "redelegation").map((e) => ({ id: num(e.id) ?? 0, from: addressOf(e.from), to: addressOf(e.to), amount: drops(e.amount), completes_at: num(e.release_at) ?? 0 }));
  return { delegations, unbonding, redelegations, bonded: drops(r.bonded), unbonding_total: drops(r.unbonding), withdrawable: drops(r.withdrawable), summary: String(r.summary ?? "") };
}

/**
 * Shares worth about `value` drops in a delegation (rounded down, at most all of them), at the
 * value the delegation reports. That's a node's word: to unbond an amount, pass
 * `wallet.undelegate(validator, { amount })`, which the chain converts itself (audit gap-7-5).
 */
export function sharesFor(d: StakeDelegation, value: Amount): bigint {
  const want = parseFang(value);
  const [shares, worth] = [BigInt(d.shares), BigInt(d.value)];
  if (worth === 0n || want >= worth) return shares;
  return (want * shares) / worth;
}

/** `/api/params`' governance parameters as a key → value map (values as strings: amounts can exceed 2^53). */
export function paramsOf(raw: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(obj(obj(raw).params))) {
    if ((typeof v === "number" && Number.isSafeInteger(v)) || (typeof v === "string" && /^\d+$/.test(v))) out[k] = String(v);
  }
  return out;
}

/** A proposal, from `/api/proposals` (`proposals[]`) or `/api/proposal/{id}`. Amounts are in drops. */
export interface ProposalSummary {
  id: number;
  title: string;
  url: string;
  /**
   * The node's own summary text. No digest covers it, so a lying node can make it say anything
   * (audit gap-7-6): show `does` instead, and never present this as what the proposal does.
   */
  summary: string;
  /** What it does, rendered here from `kind` (`describeKind`): what a vote on it binds. */
  does: string;
  kind: ProposalKind | null;
  /**
   * `proposalDigest(kind, title, url)`: what a vote binds. `null` when this client can't compute
   * it exactly (`digest_error` says why, e.g. a number past 2^53): it can't vote on it either.
   */
  digest: string | null;
  digest_error?: string;
  proposer: string | null;
  proposer_name: string | null;
  /** `voting`, `queued`, `rejected`, `executed` or `failed`. */
  status: string;
  deposit: string;
  /** Conviction-weighted ayes and nays (what the majority counts) … */
  aye: string;
  nay: string;
  /** … and the raw FANG behind them (what quorum counts). */
  aye_amount: string;
  nay_amount: string;
  /** aye_amount + nay_amount. */
  turnout: string;
  voters: number;
  /** The quorum it had to reach and whether it did: known once voting closed. */
  quorum?: string;
  quorum_reached?: boolean;
  /** Bootstrap-council votes so far (one per validator); `of` is the set's size at the close. */
  council: { ayes: number; nays: number; of?: number };
  /** Whether it needs `supermajority_bps` of the weight (upgrades, feature activations, issuance parameters, large spends). */
  supermajority: boolean;
  /** Last block votes count in, and when a passed proposal runs (after the timelock). */
  voting_ends_at: number;
  executes_at?: number;
  /** Why it was rejected or failed, or what it did. */
  outcome: string;
  /** With `rules` (`/api/proposals`' `params`): the share of the weight its ayes need, in basis points. */
  threshold_bps?: number;
}

/** `proposalDigest` of a proposal's content as the node served it, or why it can't be computed. */
function digestOf(kind: unknown, title: unknown, url: unknown): { digest: string | null; digest_error?: string } {
  if (!kind || typeof kind !== "object") return { digest: null, digest_error: "the node didn't say what this proposal does" };
  try {
    return { digest: proposalDigest(kind as ProposalKind, String(title ?? ""), typeof url === "string" ? url : "") };
  } catch (e) {
    return { digest: null, digest_error: `this client can't read the proposal exactly (${(e as Error).message})` };
  }
}

export function proposalSummary(raw: unknown, rules?: { supermajority_bps?: number }): ProposalSummary {
  const p = obj(raw);
  const weight = (side: unknown) => drops(obj(obj(side).weight).drops);
  const amount = (side: unknown) => drops(obj(obj(side).amount).drops);
  const tally = p.tally ? obj(p.tally) : null;
  const council = arr(p.council).map(obj);
  const [ayeAmount, nayAmount] = [amount(p.ayes), amount(p.nays)];
  const supermajority = p.supermajority === true;
  return {
    id: num(p.id) ?? -1,
    title: String(p.title ?? ""),
    url: typeof p.url === "string" ? p.url : "",
    summary: String(p.summary ?? ""),
    does: describeKind(p.kind),
    kind: p.kind && typeof p.kind === "object" ? (p.kind as ProposalKind) : null,
    ...digestOf(p.kind, p.title, p.url),
    proposer: p.proposer === undefined ? null : addressOf(p.proposer),
    proposer_name: nameOf(p.proposer),
    status: String(p.status ?? "unknown"),
    deposit: drops(p.deposit),
    aye: weight(p.ayes),
    nay: weight(p.nays),
    aye_amount: ayeAmount,
    nay_amount: nayAmount,
    turnout: (BigInt(ayeAmount) + BigInt(nayAmount)).toString(),
    voters: num(p.voters) ?? 0,
    quorum: tally ? drops(obj(tally.quorum).drops) : undefined,
    quorum_reached: tally ? tally.quorum_reached === true : undefined,
    council: { ayes: council.filter((c) => c.aye === true).length, nays: council.filter((c) => c.aye === false).length, of: tally ? num(tally.council_size) : undefined },
    supermajority,
    voting_ends_at: num(p.voting_end) ?? 0,
    executes_at: num(p.execute_at),
    outcome: String(p.outcome ?? ""),
    threshold_bps: supermajority ? rules?.supermajority_bps : undefined,
  };
}

/** Per-transaction options. */
export interface TxOptions {
  /** Paid to the block's proposer on top of the base fee: FANG ("0.001") or bigint drops. Default: the wallet's `tip` (0). At most `tip_cap_multiple` (20) × the base fee (`maxTip`), from transferable FANG. */
  tip?: string | number | bigint;
  /** Blocks from now the tx stays valid (at most the chain's `max_tx_validity_blocks`, an hour of blocks). Default: the wallet's `validForBlocks` (600). */
  validForBlocks?: number;
}

/** `Wallet.build`'s options: a tx's, plus the resolutions of names it carries as addresses. */
export interface BuildOptions extends TxOptions {
  /** `Client.resolution`s of names in `action`'s address fields (refused on mainnet unless proven). */
  resolutions?: Resolution[];
}

/**
 * A key's consent to being added to one account (audit gap-7-2): its public key and its
 * signature over `keyPopMessage(chain_id, genesis_hash, account, key_id)` (context
 * `KEY_POP_CONTEXT`). The key's holder makes it (`Wallet.keyGrant`) and hands it to the account's
 * owner (`encodeKeyGrant`: a grant code to paste), who adds the key with it (`addOwnerKey`,
 * `addSessionKey`). Nobody can put a key on an account without it, so `/api/key` lists only
 * accounts the key agreed to.
 */
export interface KeyGrant {
  /** The account the key agrees to join. */
  account: string;
  key_id: string;
  /** Hex ML-DSA-65 public key. */
  public_key: string;
  /** Hex proof of possession. */
  pop: string;
}

const GRANT_PREFIX = "bkgrant1";

/** A grant as one pasteable string: `bkgrant1.<account>.<base64url(public key ‖ pop)>`. */
export function encodeKeyGrant(g: KeyGrant): string {
  const body = new Uint8Array(PUBLIC_KEY_LEN + SIGNATURE_LEN);
  body.set(hexToBytes(g.public_key));
  body.set(hexToBytes(g.pop), PUBLIC_KEY_LEN);
  return `${GRANT_PREFIX}.${g.account}.${base64urlnopad.encode(body)}`;
}

/** Parses a grant code (`encodeKeyGrant`). Whitespace is ignored, so a wrapped paste works. */
export function decodeKeyGrant(code: string): KeyGrant {
  const c = code.replace(/\s+/g, "");
  if (/^[0-9a-f]{64}$/i.test(c)) throw new Error("that's a key id: adding a key needs its grant code now, which the key's holder makes for your account (Wallet.keyGrant, the MCP's use_account, or a new device's link screen)");
  const [prefix, account, body, ...rest] = c.split(".");
  if (prefix !== GRANT_PREFIX || !account || !body || rest.length) throw new Error("that isn't a key grant code (it starts with bkgrant1.)");
  if (!isAddress(account)) throw new Error("the grant code's account isn't an address");
  let bytes: Uint8Array;
  try {
    bytes = base64urlnopad.decode(body);
  } catch {
    throw new Error("the grant code is damaged (copy all of it)");
  }
  if (bytes.length !== PUBLIC_KEY_LEN + SIGNATURE_LEN) throw new Error("the grant code is damaged or cut short (copy all of it)");
  const pk = bytes.slice(0, PUBLIC_KEY_LEN);
  return { account, key_id: keyId(pk), public_key: bytesToHex(pk), pop: bytesToHex(bytes.slice(PUBLIC_KEY_LEN)) };
}

/** Whether `g` is a valid consent by its key to join `g.account` on the chain `(chainId, genesisHash)`. */
export function verifyKeyGrant(g: KeyGrant, chainId: string, genesisHash: string): boolean {
  try {
    const pk = hexToBytes(g.public_key);
    if (pk.length !== PUBLIC_KEY_LEN || keyId(pk) !== g.key_id) return false;
    return ml_dsa65.verify(hexToBytes(g.pop), keyPopMessage(chainId, genesisHash, g.account, g.key_id), pk, { context: KEY_POP_CONTEXT });
  } catch {
    return false;
  }
}

/**
 * Wallet = one ML-DSA-65 key (from a 32-byte seed, FIPS 204 ξ) acting for one account.
 * By default the account is the key's own address. Call `actAs(account)` to use a key that
 * was added to another account (a second device, or an agent session key).
 */
export class Wallet {
  readonly publicKey: Uint8Array;
  /** WebAuthn credential id, when the wallet was unlocked by a passkey. Not secret. */
  credentialId?: Uint8Array;
  /** Default tip (drops) for this wallet's transactions. */
  tip = 0n;
  /** Default validity (blocks from now) for this wallet's transactions. */
  validForBlocks = DEFAULT_VALID_FOR_BLOCKS;
  private readonly secretKey: Uint8Array;
  private acting?: string;
  /** How `actAs` found the account, when that was one node's unproven word (checked when signing on mainnet). */
  private actingUnproven?: Resolution;

  private constructor(readonly seed: Uint8Array, public client: Client = new Client()) {
    const keys = ml_dsa65.keygen(seed);
    this.publicKey = keys.publicKey;
    this.secretKey = keys.secretKey;
  }

  /** This key's own address, with the chain's prefix once the client knows it (`await client.addressPrefix()`). */
  get address(): string {
    return addressFromPublicKey(this.publicKey, this.client.prefix);
  }

  /** The account this wallet acts for (defaults to `address`). */
  get account(): string {
    return this.acting ?? this.address;
  }

  set account(account: string) {
    this.acting = account;
    this.actingUnproven = undefined;
  }

  get keyId() {
    return keyId(this.publicKey);
  }

  /**
   * Act for another account this key has been authorized on. Accepts a name or address. A name
   * needs an address here (every tx names the account it acts for), so it's resolved: proven with
   * the client's witnesses. Without them it's one node's word, and on mainnet nothing is signed
   * for an account found that way (`UnprovenNameError`): check the address and pass it instead
   * (audit gap-7-1).
   */
  async actAs(account: string) {
    const r = await this.addressNeeded(await this.client.resolution(account));
    // An address with another chain's prefix (a relaunched testnet's `boo1…` on a `tboo1…` chain)
    // isn't an account here: every tx from it, and a faucet drip to it, would be refused.
    const prefix = await this.client.addressPrefix();
    if (!isAddress(r.address, prefix)) throw new Error(`${r.address} is an address on another chain: this chain's addresses start with ${prefix}1`);
    this.acting = r.address;
    this.actingUnproven = r.proven ? undefined : r;
    return this;
  }

  static generate(client?: Client) {
    return new Wallet(randomBytes(32), client);
  }

  static fromSeed(seed: string | Uint8Array, client?: Client) {
    const bytes = typeof seed === "string" ? hexToBytes(seed) : seed;
    if (bytes.length !== 32) throw new Error("seed must be 32 bytes");
    return new Wallet(bytes, client);
  }

  /**
   * Derive a wallet from a WebAuthn PRF output. The passkey never signs transactions
   * (its P-256 signatures aren't quantum-safe); it only unlocks this ML-DSA key.
   */
  static fromPasskeyPrf(prfOutput: Uint8Array, client?: Client) {
    return new Wallet(blake3(prfOutput, { context: utf8ToBytes("blockula passkey wallet v1") }), client);
  }

  get seedHex() {
    return bytesToHex(this.seed);
  }

  sign(tx: UnsignedTx): Tx {
    const signature = ml_dsa65.sign(signingBytes(tx), this.secretKey);
    return { ...tx, public_key: bytesToHex(this.publicKey), signature: bytesToHex(signature) };
  }

  /**
   * This key's consent to being added to `account` (an address, or a name resolved by the
   * client: proven when it has witnesses; on mainnet an unproven name is refused, pass the address)
   * on the client's chain: hand `encodeKeyGrant(grant)` to the account's owner, who adds the key
   * with it. Pass `chain` to sign offline.
   */
  async keyGrant(account: string | Resolution, chain?: { chainId: string; genesisHash: string }): Promise<KeyGrant> {
    // A resolution already made (and shown) is used as is: resolving the name again could get another answer.
    const res = typeof account !== "string" ? (account.name === null ? null : account) : isAddress(account) ? null : await this.client.resolution(account);
    const address = typeof account !== "string" ? account.address : (res?.address ?? account);
    let c = chain;
    if (!c) {
      const status = await this.client.status();
      if (typeof status.genesis_hash !== "string") throw new Error(`${this.client.rpc} doesn't report a genesis_hash; it's too old for this SDK`);
      c = { chainId: status.chain_id, genesisHash: status.genesis_hash };
    }
    if (res && !res.proven && c.chainId === MAINNET_CHAIN_ID) throw new UnprovenNameError(res, this.client.rpc);
    const pop = ml_dsa65.sign(keyPopMessage(c.chainId, c.genesisHash, address, this.keyId), this.secretKey, { context: KEY_POP_CONTEXT });
    return { account: address, key_id: this.keyId, public_key: bytesToHex(this.publicKey), pop: bytesToHex(pop) };
  }

  /**
   * The `add_key` action for a grant (an object or its code): it must be for this account and
   * verify on the client's chain, so a wrong or damaged code fails here, before anything is signed.
   */
  private async grantAction(grant: KeyGrant | string, label: string, role: KeyRole): Promise<Action> {
    const g = typeof grant === "string" ? decodeKeyGrant(grant) : grant;
    if (g.account !== this.account) throw new Error(`that grant is for ${g.account}, not this account (${this.account}): the key's holder must make it for ${this.account}`);
    const status = await this.client.status();
    if (!verifyKeyGrant(g, status.chain_id, status.genesis_hash)) throw new Error("that grant doesn't verify: it's damaged, or it was made for another chain");
    return { type: "add_key", key_id: g.key_id, label, role, public_key: g.public_key, pop: g.pop };
  }

  /**
   * Builds and signs a tx: `fee` = the highest base fee already fixed for the next blocks
   * (`/api/fees` `suggested`) + `tip`, `valid_until` = the current height + `validForBlocks`, and
   * the chain's id and genesis hash from `/api/status`. Only the base fee of the block that
   * includes it (+ the tip) is charged. A tip above `tip_cap_multiple` × the lowest upcoming base
   * fee (`/api/fees` `lowest`, see `maxTip`) is refused here, as nodes would refuse it. To sign
   * offline, build an `UnsignedTx` yourself (with the chain's `genesis_hash`) and `sign` it.
   *
   * `action` may be a function of the height `valid_until` is counted from, for actions that
   * carry an absolute block height of their own (a session key's or a hunt's expiry): computed
   * from the same status read, a node that lies about the height can stretch them by at most
   * `max_tx_validity_blocks`, since validators refuse a `valid_until` further ahead than that
   * (audit gap-7-3).
   *
   * That height is checked first (`Client.checkSigningHeight`: against the witnesses' heads and,
   * when the client knows its genesis clock, the clock), since a node reporting one the chain
   * hasn't reached gets a tx it can hold and release later.
   *
   * `opts.resolutions`: how names in fields that need an address got into `action` (their
   * `Client.resolution`s). On mainnet a tx carrying one that only one node vouched for isn't
   * signed (`UnprovenNameError`), checked against the very status the tx is signed for, as the
   * helpers do (audit gap-7-1). Fields the chain resolves itself take `Client.recipient(x).signed`.
   */
  async build(action: Action | ((height: number) => Action), memo = "", opts: BuildOptions = {}): Promise<Tx> {
    return this.buildChecked(action, memo, opts, opts.resolutions ?? []);
  }

  /**
   * `build`, refusing on mainnet a tx that carries an address only one node vouched for: `unproven`
   * (the helpers' resolutions of names in fields that need an address) or the account `actAs`
   * found that way (audit gap-7-1). Checked against the very status the tx is signed for.
   */
  private async buildChecked(action: Action | ((height: number) => Action), memo: string, opts: TxOptions, unproven: Resolution[]): Promise<Tx> {
    await this.client.addressPrefix(); // so `this.account` uses the chain's prefix
    const [status, account, fees] = await Promise.all([this.client.status(), this.client.account(this.account).catch(() => null), this.client.fees()]);
    if (!Number.isSafeInteger(status.height) || status.height < 0) throw new Error(`${this.client.rpc} doesn't report its height`);
    await this.client.checkSigningHeight(status.height);
    const max: number = status.max_tx_validity_blocks ?? Number.MAX_SAFE_INTEGER;
    const validFor = opts.validForBlocks ?? Math.min(this.validForBlocks, max);
    if (!Number.isSafeInteger(validFor) || validFor < 1) throw new Error("validForBlocks must be a positive whole number of blocks");
    if (validFor > max) throw new Error(`validForBlocks can be at most ${max} (an hour of blocks)`);
    const tip = opts.tip === undefined ? this.tip : parseFang(opts.tip);
    if (typeof status.genesis_hash !== "string") throw new Error(`${this.client.rpc} doesn't report a genesis_hash; it's too old for this SDK`);
    const guess = [...unproven, ...(this.actingUnproven ? [this.actingUnproven] : [])].find((r) => !r.proven);
    if (guess && status.chain_id === MAINNET_CHAIN_ID) throw new UnprovenNameError(guess, this.client.rpc);
    // Binds the tx to this chain instance (not just its id), so it can't replay on a relaunch.
    const act = typeof action === "function" ? action(status.height) : action;
    const unsigned: UnsignedTx = { chain_id: status.chain_id, genesis_hash: status.genesis_hash, from: this.account, nonce: account?.next_nonce ?? 0, fee: "0", tip: tip.toString(), valid_until: status.height + validFor, action: act, memo };
    unsigned.fee = (baseFee(unsigned, fees.suggested) + tip).toString();
    if (tip > 0n) {
      // The cap is a multiple of the base fee of the block that includes the tx: price it at the lowest upcoming one.
      const multiple = fees.tip_cap_multiple ?? DEFAULT_TIP_CAP_MULTIPLE;
      const cap = maxTip(unsigned, fees.lowest ?? fees.suggested, multiple);
      if (tip > cap) throw new Error(`tip too high: at most ${multiple}× the base fee, ${cap} drops for this transaction now`);
    }
    return this.sign(unsigned);
  }

  /**
   * Send FANG to a `.boo` name or an address. `amount` is in FANG ("1.5") or bigint drops. A name
   * is signed as its proven owner's address when the client has witnesses (who you checked is who
   * gets paid, even if the name changes hands before the tx runs), and as the name itself when it
   * doesn't: the chain then pays whoever owns it when the tx runs. A lying node can't point it at
   * another account now, but it can hold the signed tx until its validity window, which counts
   * from the height the node reported, and release it later, by when the name may have lapsed
   * and been registered again (audit gap-7-1, review-r3-clients-2). See `Client.recipient`.
   */
  async send(to: string, amount: string | number | bigint, opts: { memo?: string; wait?: boolean } & TxOptions = {}) {
    const recipient = await this.client.recipient(to);
    const tx = await this.build({ type: "transfer", to: recipient.signed, amount: parseFang(amount).toString() }, opts.memo, opts);
    return this.client.submit(tx, opts.wait ?? true);
  }

  async registerName(name: string, opts: { wait?: boolean } & TxOptions = {}) {
    const tx = await this.build({ type: "register_name", name }, "", opts);
    return this.client.submit(tx, opts.wait ?? true);
  }

  /**
   * Authorize another device's key (e.g. a second passkey) with full control. `grant` is the
   * key's consent to this account (`keyGrant` on that device, or its grant code).
   */
  async addOwnerKey(grant: KeyGrant | string, label: string) {
    return this.client.submit(await this.build(await this.grantAction(grant, label, { type: "owner" })));
  }

  /**
   * Let an agent/app key spend from this account, transfers only, within a limit. `grant` is the
   * key's consent to this account (the agent's grant code).
   * Example: `addSessionKey(grantCode, "claude", { limit: "10", periodBlocks: 86_400, expiresInBlocks: 30 * 86_400 })`.
   */
  async addSessionKey(grant: KeyGrant | string, label: string, opts: { limit: string | number | bigint; periodBlocks: number; expiresInBlocks: number }) {
    if (!Number.isSafeInteger(opts.periodBlocks) || opts.periodBlocks < 1) throw new Error("periodBlocks must be a positive whole number of blocks");
    if (!Number.isSafeInteger(opts.expiresInBlocks) || opts.expiresInBlocks < 1) throw new Error("expiresInBlocks must be a positive whole number of blocks");
    const limit = parseFang(opts.limit).toString();
    const base = await this.grantAction(grant, label, { type: "owner" });
    // The expiry counts from the height valid_until does (one status read): see `build`.
    return this.client.submit(await this.build((height) => ({ ...base, role: { type: "session", limit, period_blocks: opts.periodBlocks, expires_at: height + opts.expiresInBlocks } satisfies KeyRole }) as Action));
  }

  async removeKey(keyIdHex: string) {
    return this.client.submit(await this.build({ type: "remove_key", key_id: keyIdHex }));
  }

  /**
   * Register a validator owned by this account, self-bonding `bond` FANG into its stake pool (at
   * least the chain's `min_self_bond`). `consensusKey` is the validator node's ML-DSA-65 public
   * key and `pop` its proof of possession (see `blockula validator init`). It's a candidate: at
   * each epoch boundary the top `max_validators` pools by stake form the next active set.
   */
  async registerValidator(opts: { consensusKey: string; p2pAddr: string; httpAddr: string; bond: string | number | bigint; pop: string }) {
    return this.client.submit(await this.build({ type: "register_validator", consensus_key: opts.consensusKey, p2p_addr: opts.p2pAddr, http_addr: opts.httpAddr, bond: parseFang(opts.bond).toString(), pop: opts.pop }));
  }

  /** Dev and test chains with admins only: pin a validator into the set ahead of stake. */
  async approveValidator(validator: string) {
    return this.client.submit(await this.build({ type: "approve_validator", validator }));
  }

  /** Dev and test chains with admins only: take a validator out of the set for good. */
  async removeValidator(validator: string) {
    return this.client.submit(await this.build({ type: "remove_validator", validator }));
  }

  /** Claim a GitHub handle. Verify it by creating a public gist (as that user) containing this account's address. */
  async linkGithub(handle: string, gistId: string) {
    return this.client.submit(await this.build({ type: "link_github", handle, gist_id: gistId }));
  }

  async unlinkGithub(handle: string) {
    return this.client.submit(await this.build({ type: "unlink_github", handle }));
  }

  /** Put a bounty on a GitHub issue. `expiresInBlocks` from now (max ~1 year). */
  async createHunt(opts: { repo: string; issue: number; amount: string | number | bigint; title: string; expiresInBlocks: number }) {
    if (!Number.isSafeInteger(opts.expiresInBlocks) || opts.expiresInBlocks < 1) throw new Error("expiresInBlocks must be a positive whole number of blocks");
    const amount = parseFang(opts.amount).toString();
    // The expiry counts from the height valid_until does (one status read): see `build`.
    return this.client.submit(await this.build((height) => ({ type: "create_hunt", repo: opts.repo, issue: opts.issue, amount, expires_at: height + opts.expiresInBlocks, title: opts.title })));
  }

  async fundHunt(huntId: number, amount: string | number | bigint) {
    return this.client.submit(await this.build({ type: "fund_hunt", hunt_id: huntId, amount: parseFang(amount).toString() }));
  }

  /** Pay a hunt's bounty (creator or admin). `to` may be a .boo name: signed as `Client.recipient` says (the proven address, or the name for the chain to resolve). */
  async awardHunt(huntId: number, to: string, pr: number, familiarKey?: string) {
    const addr = (await this.client.recipient(to)).signed;
    return this.client.submit(await this.build({ type: "award_hunt", hunt_id: huntId, to: addr, pr, ...(familiarKey ? { familiar_key: familiarKey } : {}) }));
  }

  async cancelHunt(huntId: number) {
    return this.client.submit(await this.build({ type: "cancel_hunt", hunt_id: huntId }));
  }

  /** Mark one of this account's session keys (an agent) as a Familiar. */
  async registerFamiliar(keyIdHex: string, label: string) {
    return this.client.submit(await this.build({ type: "register_familiar", key_id: keyIdHex, label }));
  }

  // ---- 🪦 Proof of Contribution ----

  /** Claim a merged PR (authored by this account's linked GitHub handle) for the contribution pool. Bonded; validators check it on GitHub. */
  async submitClaim(repo: string, pr: number, familiarKey?: string) {
    return this.client.submit(await this.build({ type: "submit_claim", repo, pr, ...(familiarKey ? { familiar_key: familiarKey } : {}) }));
  }

  /** Stake 10 FANG on a newcomer being a real, distinct person (needs 10 Soul). */
  async vouch(account: string) {
    return this.client.submit(await this.build({ type: "vouch", account: (await this.client.recipient(account)).signed }));
  }

  /** Withdraw your vouch for `account` (after the lock), or pass your own account to decline one. */
  async unvouch(account: string) {
    return this.client.submit(await this.build({ type: "unvouch", account: (await this.client.recipient(account)).signed }));
  }

  /** Join (or top up) the Séance juror pool. Needs 5 Soul; owner keys only. */
  async joinJury(bond: string | number | bigint = "10") {
    return this.client.submit(await this.build({ type: "join_jury", bond: parseFang(bond).toString() }));
  }

  async leaveJury() {
    return this.client.submit(await this.build({ type: "leave_jury" }));
  }

  /**
   * Seal a Séance judgment: one of "a", "b" or "=" per assigned pair (see `client.seance(id)`),
   * plus claim ids you think are spam. Keep the returned `salt`: `revealVotes` needs it.
   *
   * The judgment binds the pairs it was made on (audit gap-7-6): pass `pairs`, the claims (id,
   * repo, PR) you showed the juror in order, or they're read from the client (`seancePairs`). A
   * node that showed other pairs or PRs makes the commitment fail instead of misdirecting it.
   */
  async sealVotes(seanceId: number, votes: string, spam: number[] = [], salt: string = bytesToHex(randomBytes(16)), opts: { pairs?: [SeanceClaimRef, SeanceClaimRef][] } = {}) {
    await this.client.addressPrefix(); // the commitment names this account
    const pairs = opts.pairs ?? (await this.client.seancePairs(seanceId, this.account));
    if (votes.length !== pairs.length) throw new Error(`you have ${pairs.length} assigned pairs: give one of a, b or = for each`);
    const commitment = seanceCommitment(seanceId, this.account, votes, spam, salt);
    const pairs_digest = seancePairsDigest(seanceId, this.account, pairs);
    const tx = await this.client.submit(await this.build({ type: "commit_votes", seance_id: seanceId, commitment, pairs_digest }));
    return { tx, salt, votes, spam };
  }

  async revealVotes(seanceId: number, votes: string, spam: number[], salt: string) {
    return this.client.submit(await this.build({ type: "reveal_votes", seance_id: seanceId, votes, spam, salt }));
  }

  async fundPool(amount: string | number | bigint) {
    return this.client.submit(await this.build({ type: "fund_pool", amount: parseFang(amount).toString() }));
  }

  /** Withdraw every contribution reward that has vested so far. */
  async withdrawVested() {
    return this.client.submit(await this.build({ type: "withdraw_vested" }));
  }

  /** Admins of dev and test chains only (elsewhere: a governance proposal). Weight 1–100; 0 removes the repo. */
  async setRepo(repo: string, weight: number) {
    return this.client.submit(await this.build({ type: "set_repo", repo, weight }));
  }

  /** Admins of dev and test chains only (elsewhere: a governance proposal). `soul` in Soul ("2.5"), up to 100 per grant. */
  async grantSoul(to: string, soul: string | number, reason: string) {
    return this.client.submit(await this.build({ type: "grant_soul", to: (await this.client.recipient(to)).signed, amount: parseSoul(soul), reason }));
  }

  /** Admins of dev and test chains only (elsewhere: a governance proposal). */
  async rejectClaim(claimId: number, reason: string) {
    return this.client.submit(await this.build({ type: "reject_claim", claim_id: claimId, reason }));
  }

  /** Admins of dev and test chains only (elsewhere: a governance proposal). */
  async flagSybil(account: string, reason: string) {
    return this.client.submit(await this.build({ type: "flag_sybil", account: (await this.client.recipient(account)).signed, reason }));
  }

  /** Admins of dev and test chains only (elsewhere: a governance proposal). */
  async clawback(account: string, reason: string) {
    return this.client.submit(await this.build({ type: "clawback", account: (await this.client.recipient(account)).signed, reason }));
  }

  /** Reclaim the bond of a removed validator this account owns. */
  async unbond(validator: string) {
    return this.client.submit(await this.build({ type: "unbond", validator }));
  }

  // ---- protocol v1 (tags 30–49). Each builds, signs and submits one tx and waits for it. ----

  /** Submits one action (the shared tail of the helpers below); `unproven`: see `buildChecked`. */
  private async act(action: Action, opts: TxOptions & { wait?: boolean; memo?: string } = {}, unproven: Resolution[] = []) {
    return this.client.submit(await this.buildChecked(action, opts.memo ?? "", opts, unproven), opts.wait ?? true);
  }

  /**
   * A validator by address, or by a `.boo` name its account holds. Staking needs the address, so
   * a name is resolved (proven with the client's witnesses); on mainnet a tx with an unproven one
   * isn't signed (audit gap-7-1): pass the validator's address.
   */
  private async validatorAddress(validator: string) {
    return this.addressNeeded(await this.client.resolution(validator));
  }

  /** `r`, unless it's an unproven name on mainnet (`UnprovenNameError`); `buildChecked` checks again against the signing status. */
  private async addressNeeded(r: Resolution): Promise<Resolution> {
    if (!r.proven && (await this.client.status()).chain_id === MAINNET_CHAIN_ID) throw new UnprovenNameError(r, this.client.rpc);
    return r;
  }

  /** Stake `amount` (FANG, or bigint drops) in a validator's pool. Your stake earns its share of issuance and is slashed with the pool. */
  async delegate(validator: string, amount: Amount, opts: TxOptions = {}) {
    const v = await this.validatorAddress(validator);
    return this.act({ type: "delegate", validator: v.address, amount: positive(parseFang(amount), "delegate").toString() }, opts, [v]);
  }

  /**
   * Start unbonding stake in a validator's pool: `{ amount }` (FANG, or bigint drops), `shares`,
   * or `"all"` (every share you hold there, read from `/api/staking`). An amount is signed as an
   * amount and the chain converts it at the pool's price when the tx runs, so no node's report of
   * the stake decides how much unbonds (audit gap-7-5); prefer it to `sharesFor`. It stops
   * earning now and can be withdrawn with `withdrawUnbonded` once the unbonding period (21 days
   * on mainnet) is over.
   */
  async undelegate(validator: string, shares: bigint | string | "all" | { amount: Amount }, opts: TxOptions = {}) {
    const v = await this.validatorAddress(validator);
    if (typeof shares === "object" && shares !== null) return this.act({ type: "undelegate", validator: v.address, shares: "0", amount: positive(parseFang(shares.amount), "undelegate").toString() }, opts, [v]);
    return this.act({ type: "undelegate", validator: v.address, shares: (await this.sharesOf(v.address, shares)).toString() }, opts, [v]);
  }

  /** Move `shares` (or `"all"`) from one pool to another without waiting out the unbonding period. */
  async redelegate(fromValidator: string, toValidator: string, shares: bigint | string | "all", opts: TxOptions = {}) {
    const [from, to] = await Promise.all([this.validatorAddress(fromValidator), this.validatorAddress(toValidator)]);
    if (from.address === to.address) throw new Error("redelegate to a different validator");
    return this.act({ type: "redelegate", from_validator: from.address, to_validator: to.address, shares: (await this.sharesOf(from.address, shares)).toString() }, opts, [from, to]);
  }

  /** Pay out every unbonding entry that has finished unbonding. */
  async withdrawUnbonded(opts: TxOptions = {}) {
    return this.act({ type: "withdraw_unbonded" }, opts);
  }

  /** A validator's owner: set the pool's commission (basis points, 0–10000; it moves a little per epoch at most) and, optionally, new addresses. */
  async updateValidator(opts: { commissionBps: number; p2pAddr?: string; httpAddr?: string } & TxOptions) {
    if (!Number.isInteger(opts.commissionBps) || opts.commissionBps < 0 || opts.commissionBps > 10_000) throw new Error("commissionBps must be a whole number from 0 to 10000");
    return this.act({ type: "update_validator", commission_bps: opts.commissionBps, p2p_addr: opts.p2pAddr ?? "", http_addr: opts.httpAddr ?? "" }, opts);
  }

  /** Bring a jailed validator back (its owner, after the jail period). */
  async unjail(validator: string, opts: TxOptions = {}) {
    const v = await this.validatorAddress(validator);
    return this.act({ type: "unjail", validator: v.address }, opts, [v]);
  }

  /** Low-level (nodes do this automatically): two conflicting signed votes, hex-encoded as the node's evidence reporter emits them. */
  async reportEquivocation(evidenceHex: string, opts: TxOptions = {}) {
    if (!/^([0-9a-f]{2})+$/.test(evidenceHex) || evidenceHex.length > 2 * 16 * 1024) throw new Error("evidence must be 1–16384 bytes of lowercase hex");
    return this.act({ type: "report_equivocation", evidence: evidenceHex }, opts);
  }

  /** Low-level (validator nodes do this automatically): commit to a beacon secret for `epoch`. */
  async beaconCommit(epoch: number, commitmentHex: string, opts: TxOptions = {}) {
    return this.act({ type: "beacon_commit", epoch: wholeNumber(epoch, "epoch"), commitment: hexArg(commitmentHex, "commitment") }, opts);
  }

  /** Low-level (validator nodes do this automatically): reveal the beacon secret committed for `epoch`. */
  async beaconReveal(epoch: number, secretHex: string, opts: TxOptions = {}) {
    return this.act({ type: "beacon_reveal", epoch: wholeNumber(epoch, "epoch"), secret: hexArg(secretHex, "secret") }, opts);
  }

  /**
   * A member of a set of one or two, signing with its consensus key: admit candidate `validator`
   * into the set at the end of `epoch` (default: the epoch the next block is in; the tx fails in
   * any other). The newcomer will be needed for every quorum, so this is the operator's decision,
   * once an epoch per member; it joins once a quorum of the members admitted it and its node
   * showed up (`validatorReady`).
   */
  async admitValidator(validator: string, opts: { epoch?: number } & TxOptions = {}) {
    const [v, epoch] = await Promise.all([this.validatorAddress(validator), opts.epoch ?? this.client.status().then((s) => s.epoch as number)]);
    return this.act({ type: "admit_validator", validator: v.address, epoch: wholeNumber(epoch, "epoch") }, opts, [v]);
  }

  /** Low-level (candidate nodes do this automatically, in the epoch's last tenth, once a set of one or two admitted them): a candidate's consensus key signals its node is running in `epoch`. */
  async validatorReady(epoch: number, opts: TxOptions = {}) {
    return this.act({ type: "validator_ready", epoch: wholeNumber(epoch, "epoch") }, opts);
  }

  /**
   * Put a proposal to a vote (build `kind` with `proposal.*`). Bonds `deposit` (default: the
   * chain's `proposal_deposit`), refunded once the proposal reaches quorum and burned if it
   * doesn't. Names in the kind (`to`, `account`) are signed as `Client.recipient` says: the proven
   * address with witnesses, else the name, which the chain resolves when the proposal is made
   * (it stores the address, which voters then see and bind).
   *
   * The default deposit is what the node reports, and a node can report anything, so it's
   * bounded (audit gap-7-5): every witness of the client must report the same value, and it may
   * be at most `maxDeposit` (default `DEFAULT_MAX_PROPOSAL_DEPOSIT`, mainnet's 1,000 FANG).
   * Pass `deposit` to bond an amount you checked yourself.
   */
  async propose(kind: ProposalKind, opts: { title: string; url?: string; deposit?: Amount; maxDeposit?: Amount } & TxOptions) {
    const title = opts.title.trim();
    if (!title || new TextEncoder().encode(title).length > 96 || /\p{Cc}/u.test(title)) throw new Error("title must be 1–96 bytes with no control characters");
    const url = opts.url ?? "";
    if (url && (!url.startsWith("https://") || url.length > 200 || /\s|\p{Cc}/u.test(url))) throw new Error("url must be an https:// link of at most 200 bytes");
    let deposit: bigint;
    if (opts.deposit !== undefined) deposit = parseFang(opts.deposit);
    else {
      const reported = await Promise.all([this.client, ...this.client.witnesses].map(async (c) => paramsOf(await c.params()).proposal_deposit));
      const d = reported[0];
      if (d === undefined) throw new Error("this node doesn't report proposal_deposit; pass deposit");
      if (reported.some((x) => x !== d)) throw new Error(`the node and its witnesses disagree about proposal_deposit (${reported.map((x) => (x === undefined ? "none" : formatFang(x))).join(", ")}); pass deposit`);
      deposit = BigInt(d);
      const max = opts.maxDeposit !== undefined ? parseFang(opts.maxDeposit) : DEFAULT_MAX_PROPOSAL_DEPOSIT;
      if (deposit > max) throw new Error(`${this.client.rpc} reports a proposal_deposit of ${formatFang(deposit)}, more than maxDeposit (${formatFang(max)}): a node can report any deposit, and all of it is burned if the vote misses quorum. Check it on another node, then pass deposit (or a higher maxDeposit)`);
    }
    return this.act({ type: "propose", kind: await this.resolveKind(kind), deposit: positive(deposit, "deposit").toString(), title, url }, opts);
  }

  /**
   * Vote on a proposal with `amount` of your unlocked holdings (liquid, staked or unbonding;
   * unvested tokens can't vote). `conviction` 0–6 locks `amount` for 0, 1, 2, 4, 8, 16 or 32 lock
   * periods after the vote ends, for 0.1×, 1×, 2× … 6× the weight (see `convictionWeight`).
   * Voting again replaces your vote. A validator's consensus key or owner voting before the
   * bootstrap council ends also casts that validator's council vote.
   *
   * The vote binds the proposal's content (audit gap-7-6): pass `proposal`, the kind, title and
   * link you showed the voter (e.g. the `client.proposal(id)` object you rendered, described with
   * `describeKind`), or it's read from the client. A node that showed another proposal's content
   * makes the vote fail. If you showed it earlier and only kept its digest
   * (`proposalSummary(raw).digest`), pass `digest`: a proposal that reads differently now is
   * refused before anything is signed.
   */
  async vote(proposalId: number, aye: boolean, amount: Amount, conviction = 1, opts: TxOptions & { proposal?: { kind: ProposalKind; title: string; url?: string }; digest?: string } = {}) {
    if (!Number.isInteger(conviction) || conviction < 0 || conviction > MAX_CONVICTION) throw new Error(`conviction must be 0–${MAX_CONVICTION}`);
    const id = wholeNumber(proposalId, "proposalId");
    const drops = positive(parseFang(amount), "vote").toString();
    const p = opts.proposal ?? (await this.client.proposal(id));
    const proposal_digest = proposalDigest(p.kind, p.title, p.url ?? "");
    shownDigest(opts.digest, proposal_digest, `proposal #${id}`);
    return this.act({ type: "vote", proposal_id: id, aye, amount: drops, conviction, proposal_digest }, opts);
  }

  /** Release every vote lock that has expired. */
  async unlock(opts: TxOptions = {}) {
    return this.act({ type: "unlock" }, opts);
  }

  /**
   * Make this account an M-of-N multisig of its owner keys (`ownerKeyIds`): owner-only actions
   * then need `multisigPropose` plus approvals up to `threshold`. Session keys keep working
   * within their limits.
   */
  async multisigCreate(ownerKeyIds: string[], threshold: number, opts: TxOptions = {}) {
    const owners = ownerKeyIds.map((k) => k.trim().toLowerCase());
    if (owners.length === 0 || owners.length > 16 || owners.some((k) => !/^[0-9a-f]{64}$/.test(k))) throw new Error("owners must be 1–16 key ids (64 hex characters)");
    if (new Set(owners).size !== owners.length) throw new Error("owners must be distinct");
    if (!Number.isInteger(threshold) || threshold < 1 || threshold > owners.length) throw new Error(`threshold must be 1–${owners.length} (the number of owners)`);
    return this.act({ type: "multisig_create", owners, threshold }, opts);
  }

  /** Propose an action for this multisig account; it runs once enough owners approve. */
  async multisigPropose(action: Action, opts: TxOptions = {}) {
    if (action.type === "multisig_propose" || action.type === "multisig_approve") throw new Error("a multisig proposal can't hold another multisig proposal or approval");
    return this.act({ type: "multisig_propose", action }, opts);
  }

  /**
   * Approve open multisig proposal `id`. The approval binds the action (audit gap-7-6): pass
   * `action`, the one you showed the approver (decode it and describe it with `describeAction`,
   * don't trust a summary), or it's read from `client.multisig(account)`. A node that showed
   * another action makes the approval fail. `digest`: the `actionDigest` of what you showed, if
   * that's all you kept; an action that reads differently now is refused before anything is signed.
   * A `vote` binds its proposal by digest, not by id: show it with `reviewAction(action, {
   * proposal: await client.proposal(action.proposal_id) })`, which describes the proposal only if
   * its content has that digest, and don't offer the approval unless it's `readable` (gap-7-6).
   * Pass that proposal read as `proposal` here: approving a vote is refused, before anything is
   * signed, unless the proposal shown (else one read now) has exactly the content the vote binds.
   * The approval counts on this wallet's account (`actAs`), which the digest doesn't bind: on
   * mainnet act for an address you checked, not a name one node resolved.
   */
  async multisigApprove(id: number, opts: TxOptions & { action?: Action; digest?: string; proposal?: unknown } = {}) {
    const n = wholeNumber(id, "id");
    let action = opts.action;
    if (!action) {
      const m = await this.client.multisig(this.account);
      action = (Array.isArray(m?.pending) ? m.pending : []).find((p: { id?: unknown }) => p.id === n)?.action as Action | undefined;
      if (!action) throw new Error(`${this.account} has no open multisig action #${n}`);
    }
    const action_digest = actionDigest(action);
    shownDigest(opts.digest, action_digest, `multisig action #${n}`);
    if (action.type === "vote") {
      // What the ballot backs is the proposal whose content has its digest, whatever the node
      // calls #id (audit gap-7-6): a node can't make other content hash to it.
      const proposal = opts.proposal ?? (await this.client.proposal(action.proposal_id).catch(() => undefined));
      const review = reviewAction(action, { proposal, freeText: false });
      if (!review.readable) throw new Error(`multisig action #${n} can't be approved from here: ${review.does}. Nothing was signed`);
    }
    return this.act({ type: "multisig_approve", id: n, action_digest }, opts);
  }

  /** Extend a name you own by `years` (1–10), at the chain's renewal price per year. */
  async renewName(name: string, years = 1, opts: TxOptions = {}) {
    if (!Number.isInteger(years) || years < 1 || years > 10) throw new Error("renew for 1–10 years");
    return this.act({ type: "renew_name", name: bareName(name), years }, opts);
  }

  /**
   * Offer a name you own to another account (a `.boo` name or address, signed as `Client.recipient` says). It
   * stays yours until they accept it (`acceptName`); offering it again replaces the offer, and
   * `withdrawNameOffer` withdraws it. Nobody can be given a name without saying yes.
   */
  async transferName(name: string, to: string, opts: TxOptions = {}) {
    return this.act({ type: "transfer_name", name: bareName(name), to: (await this.client.recipient(to)).signed }, opts);
  }

  /** Accept a name offered to the account you act for (`client.name(n).offered_to`): it becomes yours. */
  async acceptName(name: string, opts: TxOptions = {}) {
    return this.act({ type: "transfer_name", name: bareName(name), to: this.account }, opts);
  }

  /** Withdraw your offer of a name you own (the same tx as `acceptName`: the chain tells them apart by who owns the name). */
  async withdrawNameOffer(name: string, opts: TxOptions = {}) {
    return this.acceptName(name, opts);
  }

  /** Give up a name you own; anyone can register it afterwards. */
  async releaseName(name: string, opts: TxOptions = {}) {
    return this.act({ type: "release_name", name: bareName(name) }, opts);
  }

  /** Claim your share of a community distribution with the Merkle proof published alongside it (hex hashes). */
  async claimDistribution(distributionId: number, amount: Amount, proof: string[], opts: TxOptions = {}) {
    if (proof.length > 64) throw new Error("proof too long (max 64 hashes)");
    return this.act({ type: "claim_distribution", distribution_id: wholeNumber(distributionId, "distributionId"), amount: positive(parseFang(amount), "claim").toString(), proof: proof.map((h) => hexArg(h, "proof")) }, opts);
  }

  /** Low-level (validator nodes do this automatically): a matured claim's PR was reverted by commit `revertSha`. */
  async attestRevert(claimId: number, revertSha: string, opts: TxOptions = {}) {
    if (!/^[0-9a-f]{40}$/.test(revertSha)) throw new Error("revertSha must be the 40-hex revert commit");
    return this.act({ type: "attest_revert", claim_id: wholeNumber(claimId, "claimId"), revert_sha: revertSha }, opts);
  }

  /** This account's shares in a validator's pool, for `"all"`; anything else is parsed and checked. */
  private async sharesOf(validator: string, shares: bigint | string | "all"): Promise<bigint> {
    if (shares !== "all") {
      const s = typeof shares === "bigint" ? shares : /^\d+$/.test(shares.trim()) ? BigInt(shares.trim()) : -1n;
      if (s <= 0n || s >= 1n << 64n) throw new Error("shares must be a positive whole number (see client.staking(account))");
      return s;
    }
    const d = stakeSummary(await this.client.staking(this.account)).delegations.find((x) => x.validator === validator);
    if (!d || BigInt(d.shares) === 0n) throw new Error(`${this.account} has no stake with validator ${validator}`);
    return BigInt(d.shares);
  }

  /** A proposal kind with its accounts as `Client.recipient` signs them (the chain resolves names when the proposal is made). */
  private async resolveKind(kind: ProposalKind): Promise<ProposalKind> {
    switch (kind.type) {
      case "treasury_spend":
      case "grant_soul":
        return { ...kind, to: (await this.client.recipient(kind.to)).signed };
      case "flag_sybil":
      case "unflag_sybil":
      case "clawback":
        return { ...kind, account: (await this.client.recipient(kind.account)).signed };
      default:
        return kind;
    }
  }

  balance() {
    return this.client.addressPrefix().then(() => this.client.account(this.account)).then((a) => a.balance_display as string, () => "0 FANG");
  }

  /** Dev/test chains only: drip 100 FANG to this wallet. */
  async faucet(wait = true) {
    await this.client.addressPrefix();
    return this.client.faucet(this.account, wait);
  }
}

/** Throws unless `shown` (a digest the caller displayed, if any) is `now`, the digest about to be signed. */
function shownDigest(shown: string | undefined, now: string, what: string) {
  if (shown === undefined) return;
  if (shown.trim().toLowerCase() !== now) throw new Error(`${what} doesn't read the way it did when it was shown: its digest is now ${now}, not ${shown}. Nothing was signed: the node may be showing different content to different reads.`);
}

/** An error response from a Blockula node. `status` is the HTTP status (429 = rate limited). */
export class ApiError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "ApiError";
  }
}

/**
 * The node reports a transaction as failed. Two different things (audit review-fix-clients-2):
 * - `dropped` false: it executed in block `tx.height` and failed there. Its fee was charged; it
 *   can't execute again.
 * - `dropped` true: this node dropped it from its mempool without running it (evicted by a
 *   higher tip, replaced, not included, or an earlier tx from the account was dropped). Nothing
 *   was charged, but other nodes may still hold it and include it until its `valid_until`
 *   (`validUntil`, when known): check `client.tx(id)` before sending it again, or a retry can pay twice.
 */
/**
 * Whether a `/api/tx` answer of status "failed" is a tx the node dropped from its mempool without
 * running it (nothing charged; another node may still include it until its `valid_until`), not
 * one that ran in a block and failed (its fee was charged). Mempool failures are answered without
 * the tx itself (node/src/api.rs `tx`), and older nodes don't say `dropped`.
 */
export function txDropped(answer: unknown): boolean {
  const tx = answer as { dropped?: unknown; action?: unknown } | null;
  return tx?.dropped === true || (tx !== null && typeof tx === "object" && tx.action === undefined && tx.dropped !== false);
}

export class TxFailedError extends Error {
  readonly dropped: boolean;
  constructor(readonly id: string, readonly reason: string, readonly tx: any, readonly validUntil?: number) {
    const dropped = txDropped(tx);
    super(
      dropped
        ? `tx ${id} was dropped by this node without running (${reason}); nothing was charged, but another node may still include it${validUntil !== undefined ? ` until block ${validUntil}` : " until its valid_until"}, so check it (client.tx) before sending it again`
        : `tx ${id} failed${typeof tx?.height === "number" ? ` in block ${tx.height}` : ""}: ${reason} (its fee was charged)`,
    );
    this.dropped = dropped;
    this.name = "TxFailedError";
  }
}

/**
 * The node accepted a transaction, but it wasn't seen finalized (or failed) in time. It may still
 * execute until its `valid_until`: check it with `client.tx(id)` before sending it again, or a
 * retry can pay twice.
 */
export class TxPendingError extends Error {
  constructor(readonly id: string, readonly waitedMs: number, readonly lastError?: unknown) {
    super(`tx ${id} still pending ${waitedMs > 0 ? `after ${waitedMs}ms` : "(its status couldn't be read)"}; it was submitted and may still execute, so check it (client.tx) before sending again`);
    this.name = "TxPendingError";
  }
}

/** The node didn't answer within the client's `timeoutMs`. Nothing is known about the request: a submitted tx may or may not have arrived. */
export class RequestTimeoutError extends Error {
  constructor(readonly rpc: string, readonly path: string, readonly timeoutMs: number) {
    super(`${rpc} didn't answer within ${timeoutMs}ms (${path})`);
    this.name = "RequestTimeoutError";
  }
}

/** A proof older than the newest head seen (this node's and every witness's) by more than `maxLagBlocks`. */
export class StaleProofError extends Error {
  constructor(readonly height: number, readonly head: number, readonly maxLagBlocks: number) {
    super(`the proof is for block #${height}, but the chain is at #${head} (at most ${maxLagBlocks} blocks behind is accepted): a node serving old state, or one that's far behind`);
    this.name = "StaleProofError";
  }
}

/**
 * On mainnet, a transaction would carry an address that only one node vouched for: a `.boo` name
 * resolved without witnesses, in a field that needs an address (staking's validator, the account
 * `actAs` acts for, a key grant's account). Nothing was signed (audit gap-7-1). Check
 * `resolution.address` with its owner and pass it instead of the name, or give the client witnesses.
 */
export class UnprovenNameError extends Error {
  constructor(readonly resolution: Resolution, rpc: string) {
    super(`${resolution.name ?? resolution.input}.boo resolves to ${resolution.address} only by ${rpc}'s word (unproven: a node can point a name anywhere), and on mainnet this needs an address: nothing was signed. Check that address with its owner and pass it instead of the name, or give the client witnesses to prove names`);
    this.name = "UnprovenNameError";
  }
}

/**
 * A node reports a height the chain can't have reached (`Client.checkSigningHeight`): a tx signed
 * against it could be held and released later. Nothing was signed.
 */
export class HeightAheadError extends Error {
  constructor(readonly rpc: string, readonly height: number, readonly limit: number, by: string) {
    super(`${rpc} reports height ${height}, but by ${by} the chain can't be past block ${limit}: a transaction signed against that height could be held and released later. Nothing was signed: use another node (or check this machine's clock)`);
    this.name = "HeightAheadError";
  }
}

/**
 * How far ahead of the lowest head a client's witnesses report a tx's height may be, by default
 * (`ClientOptions.maxHeadLeadBlocks`): 30 blocks, room for propagation between nodes.
 */
export const DEFAULT_MAX_HEAD_LEAD_BLOCKS = 30;

/**
 * How far behind the network's clocks this machine's may be before `heightLimit` refuses an honest
 * node: 15 minutes (the CLI uses the same, node/src/cli_net.rs `HEIGHT_CLOCK_TOLERANCE_MS`).
 */
export const HEIGHT_CLOCK_TOLERANCE_MS = 15 * 60 * 1000;

/** How far ahead of their clocks validators accept a block's timestamp (node/src/consensus.rs `MAX_CLOCK_DRIFT_MS`). */
const MAX_CLOCK_DRIFT_MS = 1_000;

/**
 * The highest height a chain can have reached at `nowMs` (node/src/cli_net.rs `height_limit`):
 * every block's timestamp is at least its parent's plus the block time, and validators refuse one
 * more than a second ahead of their clocks, so block h is stamped no earlier than genesis + h ×
 * block time. A chain that ran slower than its block time is below this by that much, so it
 * refuses only heights the chain can't have, not every lie.
 */
export function heightLimit(createdAtMs: number, blockTimeMs: number, nowMs: number): number {
  return Math.floor(Math.max(0, nowMs + MAX_CLOCK_DRIFT_MS + HEIGHT_CLOCK_TOLERANCE_MS - createdAtMs) / Math.max(1, blockTimeMs));
}

/** The node reports a different chain than the client is pinned to (`ClientOptions`). */
export class ChainMismatchError extends Error {
  constructor(readonly rpc: string, readonly field: "chain_id" | "genesis_hash" | "address_prefix", readonly expected: string, readonly reported: string | undefined) {
    super(`${rpc} reports ${field} ${reported ?? "(none)"}, but this client is pinned to ${expected}; refusing to use it`);
    this.name = "ChainMismatchError";
  }
}

/** What a `Client` requires of the node. Every `/api/status` (including the one `Wallet.build` signs against) is checked. */
export interface ClientOptions {
  /** The address prefix you expect (`boo` or `tboo`). */
  prefix?: string;
  /** The chain id the node must report (e.g. `blockula-1`). */
  chainId?: string;
  /** The genesis hash (hex) the node must report. */
  genesisHash?: string;
  /** How long one request may take before it fails with `RequestTimeoutError` (default 15 s, at most `MAX_TIMEOUT_MS`). */
  timeoutMs?: number;
  /**
   * Clients for independent nodes of the same chain. With witnesses, every name this client
   * resolves for a signature is a proven read (`provenName`): the record's Merkle proof must check
   * out against a state root each witness reports for that block, and be fresh. Without them
   * (audit gap-7-1):
   * - where the chain resolves names itself (`Wallet.send`, `awardHunt`, `vouch`/`unvouch`,
   *   `grantSoul`, `flagSybil`, `clawback`, `transferName`, proposal targets), the name is signed
   *   as the name (`recipient`): the chain pays whoever owns it when the tx runs, so a lying node
   *   can't point it at another account now, only hold it within its validity window;
   * - where an address is needed (`delegate`/`undelegate`/`redelegate`/`unjail`, `actAs`,
   *   `keyGrant`), a name is one node's unproven answer, and on mainnet nothing is signed with it
   *   (`UnprovenNameError`): pass the address.
   */
  witnesses?: Client[];
  /** Refuse to resolve a `.boo` name for a signature unless it's proven (needs `witnesses`). */
  requireProvenNames?: boolean;
  /**
   * With witnesses: how far ahead of the lowest head they report the height a tx is signed
   * against may be (default `DEFAULT_MAX_HEAD_LEAD_BLOCKS`). See `Client.checkSigningHeight`.
   */
  maxHeadLeadBlocks?: number;
  /**
   * The chain's genesis time and block time (`created_at_ms` and `block_time_ms` of the
   * genesis.json that hashes to `genesisHash`, as published with it), as pins: with them, a
   * status claiming a height the chain can't have reached by this machine's clock is refused
   * before anything is signed (`Client.checkSigningHeight`). Not the node's word, so don't take
   * them from it.
   */
  genesisClock?: { createdAtMs: number; blockTimeMs: number };
}

/** Where a recipient (a `.boo` name or an address) goes, and how sure that is (`Client.resolution`). */
export interface Resolution {
  /** What was asked for. */
  input: string;
  /** The address that gets signed. */
  address: string;
  /** The bare name, when `input` was one. */
  name: string | null;
  /** An address as given, or a name proven against the client's witnesses; `false`: one node's word. */
  proven: boolean;
  /** How it was resolved, in plain English (show it with the address before signing). */
  how: string;
  /** The block a proven name was read at. */
  height?: number;
}

/** A recipient in a field the chain resolves itself, as it gets signed (`Client.recipient`). */
export interface Recipient extends Resolution {
  /**
   * What goes into the action: an address as given, a name's proven owner (with witnesses), or
   * the name itself (`alice.boo`) when nothing proved it.
   */
  signed: string;
  /**
   * `signed` is the name: the chain resolves it to whoever owns it when the tx runs, so no node
   * can point it at another account now (one holding the signed tx can still release it later,
   * by when the name may have changed hands: see `recipient`). `address` is then just the node's
   * current answer, for display.
   */
  byChain: boolean;
}

/** Default per-request timeout of a `Client`. */
export const DEFAULT_TIMEOUT_MS = 15_000;

/**
 * The longest timeout a `Client` takes: 2^31 − 1 ms (about 24.8 days). Timers store their delay
 * as a signed 32-bit number, and a longer one fires at once (audit review-fix-clients-1).
 */
export const MAX_TIMEOUT_MS = 2_147_483_647;

/** `ms` as a timeout: a whole number of milliseconds from 1 to `MAX_TIMEOUT_MS`, or an error naming `what`. */
export function checkTimeoutMs(ms: number, what = "timeoutMs"): number {
  if (!Number.isSafeInteger(ms) || ms < 1 || ms > MAX_TIMEOUT_MS) throw new Error(`${what} must be a whole number of milliseconds from 1 to ${MAX_TIMEOUT_MS} (about 24.8 days)`);
  return ms;
}

/**
 * How far behind the newest head (the node's own and each witness's `/api/status` height) a
 * proven read's proof may be, by default: 10 blocks (10 s at 1 s blocks), room for propagation
 * between nodes. Older proofs are refused (`StaleProofError`), so a node can't answer with a
 * genuine but outdated record (a name's previous owner, a balance before a spend, a removed key).
 */
export const DEFAULT_MAX_PROOF_LAG_BLOCKS = 10;

/** Options of the proven reads (`provenRecord`, `provenAccount`, `provenName`). */
export interface ProvenOptions {
  /** Clients for independent nodes: each must report the same state root for the proof's block, and its head bounds how old the proof may be. */
  witnesses?: Client[];
  /** How long to wait for a witness that hasn't reached the proof's block yet (default 5 s). */
  waitMs?: number;
  /** How many blocks the proof may be behind the newest head seen (default `DEFAULT_MAX_PROOF_LAG_BLOCKS`). */
  maxLagBlocks?: number;
}

/** A record proven against a state root, and the newest head height that root was checked against. */
export type ProvenRead = ProvenRecord & { head: number };

export class Client {
  /** The chain's address prefix: given, learned from `/api/status`, or `boo` until then. */
  prefix: string;
  private prefixKnown: boolean;
  private readonly pinnedPrefix?: string;
  private readonly pinnedChainId?: string;
  private readonly pinnedGenesis?: string;
  /** Per-request timeout (ms). */
  readonly timeoutMs: number;
  /** Independent nodes names are proven against (`ClientOptions.witnesses`). */
  readonly witnesses: Client[];
  readonly requireProvenNames: boolean;
  /** See `ClientOptions.maxHeadLeadBlocks`. */
  readonly maxHeadLeadBlocks: number;
  /** See `ClientOptions.genesisClock`. */
  readonly genesisClock?: { createdAtMs: number; blockTimeMs: number };

  /** `opts` pins the chain: a node reporting another prefix, chain id or genesis hash is refused with `ChainMismatchError`. */
  constructor(readonly rpc: string = DEFAULT_RPC, opts: ClientOptions = {}) {
    if (opts.prefix !== undefined && !(KNOWN_PREFIXES as readonly string[]).includes(opts.prefix)) throw new Error(`unknown address prefix ${opts.prefix}`);
    if (opts.genesisHash !== undefined && !/^[0-9a-f]{64}$/i.test(opts.genesisHash)) throw new Error("genesisHash must be 64 hex characters");
    this.timeoutMs = checkTimeoutMs(opts.timeoutMs ?? DEFAULT_TIMEOUT_MS);
    this.witnesses = [...(opts.witnesses ?? [])];
    if (this.witnesses.some((w) => w === this || w.rpc.replace(/\/$/, "") === rpc.replace(/\/$/, ""))) throw new Error("a witness must be another node, not the client's own");
    this.requireProvenNames = opts.requireProvenNames === true;
    if (this.requireProvenNames && this.witnesses.length === 0) throw new Error("requireProvenNames needs witnesses to prove names against");
    this.maxHeadLeadBlocks = opts.maxHeadLeadBlocks ?? DEFAULT_MAX_HEAD_LEAD_BLOCKS;
    if (!Number.isSafeInteger(this.maxHeadLeadBlocks) || this.maxHeadLeadBlocks < 0) throw new Error("maxHeadLeadBlocks must be a whole number of blocks");
    if (opts.genesisClock) {
      const { createdAtMs, blockTimeMs } = opts.genesisClock;
      if (!Number.isSafeInteger(createdAtMs) || createdAtMs < 0 || !Number.isSafeInteger(blockTimeMs) || blockTimeMs < 1) throw new Error("genesisClock needs the genesis's created_at_ms and block_time_ms (whole milliseconds)");
      this.genesisClock = { createdAtMs, blockTimeMs };
    }
    this.pinnedPrefix = opts.prefix;
    this.pinnedChainId = opts.chainId;
    this.pinnedGenesis = opts.genesisHash?.toLowerCase();
    this.prefix = opts.prefix ?? DEFAULT_PREFIX;
    this.prefixKnown = opts.prefix !== undefined;
  }

  /**
   * Refuses (`HeightAheadError`, before anything is signed) a height a tx's validity window would
   * count from that this client has reason to think the chain hasn't reached: more than
   * `maxHeadLeadBlocks` past the lowest head its witnesses report, or, with a `genesisClock`, past
   * what the chain can have reached by this machine's clock (`heightLimit`). A node reporting a
   * future height gets a tx it can hold and release later, when the window it picked opens; with
   * a name signed in it, after the name may have changed hands (audit review-r3-clients-2).
   * Every witness must report its height, as every witness must confirm a proof: one honest
   * witness is enough to refuse a height the chain hasn't reached, whatever the others say.
   * Without witnesses or a genesis clock, the height is the node's word.
   */
  async checkSigningHeight(height: number): Promise<void> {
    if (this.genesisClock) {
      const limit = heightLimit(this.genesisClock.createdAtMs, this.genesisClock.blockTimeMs, Date.now());
      if (height > limit) throw new HeightAheadError(this.rpc, height, limit, "this machine's clock and the chain's genesis");
    }
    if (!this.witnesses.length) return;
    const heads = await Promise.all(this.witnesses.map((w) => w.status().then((s) => (Number.isSafeInteger(s.height) && s.height >= 0 ? (s.height as number) : undefined), () => undefined)));
    const silent = this.witnesses.filter((_, i) => heads[i] === undefined);
    if (silent.length) throw new Error(`${silent.map((w) => w.rpc).join(", ")} didn't report ${silent.length === 1 ? "its height" : "their heights"}, so ${this.rpc}'s height ${height} can't be checked against every witness: nothing was signed`);
    const lowest = Math.min(...(heads as number[]));
    if (height > lowest + this.maxHeadLeadBlocks) throw new HeightAheadError(this.rpc, height, lowest + this.maxHeadLeadBlocks, `the lowest head its witnesses report (#${lowest})`);
  }

  /** The chain's address prefix, asking the node once if it isn't known yet. */
  async addressPrefix(): Promise<string> {
    if (!this.prefixKnown) await this.status();
    return this.prefix;
  }

  /** One request, bounded by `timeoutMs` (headers and body): a node that never answers fails with `RequestTimeoutError` instead of hanging. */
  private async call(path: string, body?: unknown) {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), this.timeoutMs);
    let res: Response;
    let text: string;
    try {
      const init: RequestInit = body === undefined ? { signal: ctl.signal } : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body), signal: ctl.signal };
      res = await fetch(this.rpc.replace(/\/$/, "") + "/api" + path, init);
      text = await res.text();
    } catch (e) {
      if (ctl.signal.aborted) throw new RequestTimeoutError(this.rpc, path, this.timeoutMs);
      throw e;
    } finally {
      clearTimeout(timer);
    }
    let json: any;
    try {
      json = JSON.parse(text);
    } catch {
      throw new ApiError(res.status, res.ok ? "the node returned something that isn't JSON" : `node error ${res.status} ${res.statusText}`.trim());
    }
    if (!res.ok) throw new ApiError(res.status, json.error ?? res.statusText);
    return json;
  }

  async status() {
    const s = await this.call("/status");
    if (this.pinnedChainId !== undefined && s.chain_id !== this.pinnedChainId) throw new ChainMismatchError(this.rpc, "chain_id", this.pinnedChainId, s.chain_id);
    if (this.pinnedGenesis !== undefined && String(s.genesis_hash ?? "").toLowerCase() !== this.pinnedGenesis) throw new ChainMismatchError(this.rpc, "genesis_hash", this.pinnedGenesis, s.genesis_hash);
    if (typeof s.address_prefix === "string") {
      if (this.pinnedPrefix !== undefined && s.address_prefix !== this.pinnedPrefix) throw new ChainMismatchError(this.rpc, "address_prefix", this.pinnedPrefix, s.address_prefix);
      this.prefix = s.address_prefix;
      this.prefixKnown = true;
    }
    return s;
  }
  blocks(limit = 20) { return this.call(`/blocks?limit=${limit}`); }
  block(heightOrHash: number | string) { return this.call(`/block/${heightOrHash}`); }
  tx(id: string) { return this.call(`/tx/${id}`); }
  account(addressOrName: string) { return this.call(`/account/${encodeURIComponent(addressOrName)}`); }
  /** A name: `status` (`active`, `grace` (expired; only its owner can renew it), `reserved` or `available`), owner, `expires_at`, `offered_to` (if its owner offered it to an account: theirs once they accept), prices. */
  name(name: string) { return this.call(`/name/${encodeURIComponent(name)}`); }
  /** The base fee: each upcoming block's (`slots`) and `suggested`, which covers any of the next 17 blocks. */
  fees(): Promise<{ height: number; lag: number; slots: ({ height: number; max_block_bytes: number } & BaseFee)[]; suggested: BaseFee; lowest?: BaseFee; tip_cap_multiple?: number; account_creation_fee: string; [k: string]: unknown }> { return this.call("/fees"); }
  /** The governance parameters (node/src/params.rs `ChainParams`; amounts as strings) under `params`, genesis constants under `genesis`, and the issuance year. `paramsOf` flattens it. */
  params() { return this.call("/params"); }
  /** The chain's genesis.json as this node serves it: the node's word (nothing here checks it against the genesis hash). */
  genesis() { return this.call("/genesis"); }
  /** Upgrades: the protocol version in force, what the node's binary supports, the `schedule` (`[{ version, activation_height, active }]`, what `proposal.cancelUpgrade` names), halts and each validator's readiness signal. */
  upgrade() { return this.call("/upgrade"); }
  /** Accounts a key is authorized on (its own address, plus any it was added to). */
  /**
   * Accounts a key is on (`{ key_id, accounts, next }`), a page at a time: pass the previous page's
   * `next` as `after`. Only accounts the key agreed to join (its grant) can list it (audit gap-7-2).
   */
  keyAccounts(keyIdHex: string, page: { after?: string; limit?: number } = {}) {
    const q = new URLSearchParams();
    if (page.after) q.set("after", page.after);
    if (page.limit !== undefined) q.set("limit", String(page.limit));
    return this.call(`/key/${keyIdHex}${q.size ? `?${q}` : ""}`);
  }
  search(q: string) { return this.call(`/search/${encodeURIComponent(q)}`); }
  /** The validator set: `active` this epoch, `next_epoch` (with `joining`/`leaving`), `candidates`, `jailed`, `tombstoned` and `removed`, each with stake, self-bond, commission, jail and liveness; epoch timing; the churn limit and slashing parameters. */
  validators() { return this.call("/validators"); }
  /** Hunts (bounties on GitHub issues), biggest first. */
  hunts(status?: "open" | "awarded" | "cancelled" | "expired") { return this.call(`/hunts${status ? `?status=${status}` : ""}`); }
  hunt(id: number) { return this.call(`/hunt/${id}`); }
  /** Off-chain check of a GitHub link (gist owned by the handle containing the address). */
  github(handle: string) { return this.call(`/github/${encodeURIComponent(handle)}`); }
  /** The contribution pool: balance, repos, rules, the open Séance and recent results. */
  contributions() { return this.call("/contributions"); }
  claims(opts: { status?: "pending" | "verified" | "in_seance" | "scored" | "spam" | "rejected"; account?: string } = {}) {
    const q = new URLSearchParams(Object.entries(opts).filter(([, v]) => v !== undefined) as [string, string][]).toString();
    return this.call(`/claims${q ? `?${q}` : ""}`);
  }
  claim(id: number) { return this.call(`/claim/${id}`); }
  /** A Séance, with each juror's assigned pairs (what they must judge). */
  seance(id: number) { return this.call(`/seance/${id}`); }
  /** A juror's assigned pairs in Séance `id`, in order, with each claim's repo and PR (what `sealVotes` binds). */
  async seancePairs(id: number, juror: string): Promise<[SeanceClaimRef, SeanceClaimRef][]> {
    const s = await this.seance(id);
    const seat = (Array.isArray(s?.jurors) ? s.jurors : []).find((j: { juror?: { address?: string } }) => j.juror?.address === juror);
    if (!seat) throw new Error(`${juror} isn't on the jury of Séance #${id}`);
    const claims: { id: number; repo: string; pr: number }[] = Array.isArray(s.claims) ? s.claims : [];
    const ref = (cid: number): SeanceClaimRef => {
      const c = claims.find((x) => x.id === cid);
      if (!c) throw new Error(`Séance #${id} doesn't list claim #${cid}`);
      return { id: cid, repo: c.repo, pr: c.pr };
    };
    return (Array.isArray(seat.assigned_pairs) ? seat.assigned_pairs : []).map(([a, b]: [number, number]) => [ref(a), ref(b)] as [SeanceClaimRef, SeanceClaimRef]);
  }
  /** Soul leaderboard. */
  souls(limit = 50) { return this.call(`/souls?limit=${limit}`); }

  // ---- protocol v1 reads, as the node serves them; `stakeSummary` / `proposalSummary` / `paramsOf` read them into plain shapes. ----

  /** An account's stake: `delegations` (shares, value, principal), `entries` (unbonding and redelegations, with `release_at`), `bonded`, `unbonding` and `withdrawable` totals, the `validators` it runs, a `summary`. */
  staking(account: string) { return this.call(`/staking/${encodeURIComponent(account)}`); }
  /** Governance proposals, newest first (`status`: `voting`, `queued`, `rejected`, `executed`, `failed`), the rules (`params`), treasury and community balances. */
  proposals(opts: { status?: string; limit?: number } = {}) {
    const q = new URLSearchParams(Object.entries(opts).filter(([, v]) => v !== undefined).map(([k, v]) => [k, String(v)])).toString();
    return this.call(`/proposals${q ? `?${q}` : ""}`);
  }
  /** One proposal with its votes. */
  proposal(id: number) { return this.call(`/proposal/${id}`); }
  /** A community distribution: Merkle root, total, claimed so far, claim deadline. */
  distribution(id: number) { return this.call(`/distribution/${id}`); }
  /** A multisig account: owners, threshold, open proposals and their approvals. */
  multisig(account: string) { return this.call(`/multisig/${encodeURIComponent(account)}`); }

  /** `/api/proof/{namespace}/{key}`, unchecked. Prefer `provenRecord`. */
  proof(namespace: number, key: string | Uint8Array): Promise<ProofResponse> {
    const k = typeof key === "string" ? utf8ToBytes(key) : key;
    return this.call(`/proof/${namespace}/${bytesToHex(k)}`);
  }

  /**
   * A record with its Merkle proof checked against the state root the node reports. On its own
   * that only shows the node is consistent; `witnesses` (clients for independent nodes) must
   * each report the same state root for that block, so a single lying node can't fake balances,
   * names or keys. A witness that hasn't reached the block yet is retried for `waitMs`.
   *
   * The proof must also be fresh: its block may be at most `maxLagBlocks` behind the newest head
   * this node and the witnesses report (`StaleProofError` otherwise). Witnesses confirm any
   * genuine root, old ones included, so without this a lying node could serve a real but outdated
   * record (a name's previous owner, a balance before a spend, a key since removed). `head` is
   * the newest head seen. Without witnesses only this node's own head is compared, which a lying
   * node controls: pass witnesses for anything that matters.
   */
  async provenRecord(namespace: number, key: string | Uint8Array, opts: ProvenOptions = {}): Promise<ProvenRead> {
    const maxLag = opts.maxLagBlocks ?? DEFAULT_MAX_PROOF_LAG_BLOCKS;
    if (!Number.isSafeInteger(maxLag) || maxLag < 0) throw new Error("maxLagBlocks must be a whole number of blocks");
    const witnesses = opts.witnesses ?? [];
    // Heads first, so the honest ones can only be behind the proof, never far ahead of it.
    const heads = await Promise.all([this, ...witnesses].map(async (c) => {
      const h = (await c.status()).height;
      if (!Number.isSafeInteger(h) || h < 0) throw new Error(`${c.rpc} doesn't report its height`);
      return h as number;
    }));
    const r = checkProofResponse(await this.proof(namespace, key), namespace, key);
    const head = Math.max(...heads);
    if (r.height + maxLag < head) throw new StaleProofError(r.height, head, maxLag);
    await Promise.all(witnesses.map((w) => w.confirmStateRoot(r.height, r.root, opts.waitMs ?? 5_000)));
    return { ...r, head };
  }

  /** Throws unless this node's block `height` has state root `root` (waiting up to `waitMs` for it to get there). */
  async confirmStateRoot(height: number, root: string, waitMs = 5_000): Promise<void> {
    const until = Date.now() + waitMs;
    for (;;) {
      try {
        const b = await this.block(height);
        if (b.state_root !== root) throw new Error(`${this.rpc} disagrees about the state at block #${height}: root ${String(b.state_root).slice(0, 16)}…, not ${root.slice(0, 16)}…`);
        return;
      } catch (e) {
        if (!(e instanceof ApiError && e.status === 404) || Date.now() > until) throw e;
        await new Promise((r) => setTimeout(r, 250));
      }
    }
  }

  /** An address's balance, nonce and keys, proven and fresh (see `provenRecord`). `null` if the account provably doesn't exist. */
  async provenAccount(address: string, opts: ProvenOptions = {}) {
    if (!isAddress(address)) throw new Error(`${address} isn't an address`);
    const r = await this.provenRecord(NAMESPACES.ACCOUNTS, address, opts);
    return r.value === null ? null : { address, ...decodeAccount(r.value), height: r.height, root: r.root, head: r.head };
  }

  /**
   * Who a `.boo` name resolves to, proven and fresh (see `provenRecord`). `null` if it provably
   * doesn't resolve: never registered, or expired (`expires_at` passed; in its grace period only
   * the owner can renew it, and after that anyone can register it). `includeExpired` returns an
   * expired record too, with `active: false`: never pay its `owner`.
   */
  async provenName(name: string, opts: ProvenOptions & { includeExpired?: boolean } = {}) {
    const r = await this.provenRecord(NAMESPACES.NAMES, bareName(name), opts);
    if (r.value === null) return null;
    const rec = decodeName(r.value);
    // node/src/econ.rs `name_active`: the next block (height + 1) must be at most `expires_at`.
    const active = r.height + 1 <= rec.expires_at;
    if (!active && !opts.includeExpired) return null;
    return { ...rec, active, height: r.height, root: r.root, head: r.head };
  }

  /**
   * The address a name or address resolves to, for fields that need an address (see
   * `resolution`): proven with witnesses, else one node's word. Where the chain resolves names
   * itself (payments, vouches, name offers…), use `recipient` instead, which signs the name.
   */
  async resolve(nameOrAddress: string): Promise<string> {
    return (await this.resolution(nameOrAddress)).address;
  }

  /**
   * How to sign a recipient in a field the chain resolves itself (`transfer.to`, `award_hunt.to`,
   * `vouch`/`unvouch`/`flag_sybil`/`clawback.account`, `grant_soul.to`, `transfer_name.to`, and
   * proposal targets): an address as given; with witnesses a name's proven owner; without them
   * the name itself, which the chain resolves to its owner when the tx runs, so a lying node
   * can't point it at another account now (audit gap-7-1). It can still hold the signed tx and
   * release it later within its validity window, which counts from the height that node reported
   * (bounded by `checkSigningHeight` only with witnesses or a `genesisClock`): whoever owns the
   * name then is paid, so a name that lapses in between pays its next owner (review-r3-clients-2).
   * Show `how` before signing.
   */
  async recipient(nameOrAddress: string): Promise<Recipient> {
    const r = await this.resolution(nameOrAddress);
    if (r.name === null || r.proven) return { ...r, signed: r.address, byChain: false };
    return {
      ...r,
      signed: `${r.name}.boo`,
      byChain: true,
      how: `${r.name}.boo is signed as the name: the chain pays whoever owns it when the transaction runs, so ${this.rpc} can't point it at another account now, though a node holding the signed transaction could release it later in its validity window, after the name changed hands (${this.rpc} says the owner is ${r.address} now, unproven)`,
    };
  }

  /**
   * Where a recipient goes and how sure that is: show `address` and `how` before signing. With the
   * client's `witnesses` a name is a proven, fresh read (`provenName`), so a lying node can't
   * point it elsewhere; without them it's this node's answer alone (`proven: false`), which a
   * lying node controls (audit gap-7-1).
   */
  async resolution(nameOrAddress: string): Promise<Resolution> {
    const input = nameOrAddress;
    if (isAddress(input)) return { input, address: input, name: null, proven: true, how: "an address, signed as given" };
    if (/^t?boo1/i.test(input.trim())) throw new Error(`${input} isn't a valid address (check for typos; addresses are lowercase)`);
    const name = bareName(input);
    if (this.witnesses.length) {
      const rec = await this.provenName(name, { witnesses: this.witnesses });
      if (!rec) throw new Error(`${name}.boo doesn't resolve: it isn't registered or has expired (proven against ${this.witnesses.length} witness${this.witnesses.length === 1 ? "" : "es"})`);
      if (!isAddress(rec.owner)) throw new Error(`${name}.boo's record names ${rec.owner}, which isn't an address`);
      return { input, address: rec.owner, name, proven: true, how: `${name}.boo proven at block #${rec.height} against ${this.witnesses.length} independent node${this.witnesses.length === 1 ? "" : "s"}`, height: rec.height };
    }
    if (this.requireProvenNames) throw new Error(`${name}.boo can't be proven: this client has no witnesses`);
    const r = await this.name(name);
    if (r.status === "grace") throw new Error(`${r.name} has expired (at block ${r.expires_at})`);
    if (r.available || !r.owner) throw new Error(`${r.name} isn't registered`);
    const address = String(r.owner.address ?? "");
    if (!isAddress(address)) throw new Error(`${this.rpc} resolved ${name}.boo to ${JSON.stringify(address)}, which isn't an address`);
    return { input, address, name, proven: false, how: `${name}.boo as ${this.rpc} reports it, unproven (a node can point a name anywhere; give the client witnesses to prove it)` };
  }

  /**
   * Submits a signed tx, then (with `wait`) waits for it. What a failure means:
   * - `ApiError`: the node refused the tx; it wasn't admitted.
   * - `RequestTimeoutError` or a network error: unknown; the tx may have arrived.
   * - `TxFailedError`: it executed and failed (`dropped` false: its fee was charged), or this node
   *   dropped it unrun (`dropped` true: nothing charged, but it may still execute elsewhere until
   *   its `valid_until`).
   * - `TxPendingError`: it was admitted and may still execute. Don't send it again blindly.
   */
  async submit(tx: Tx, wait = true) {
    const { id } = await this.call("/tx", tx);
    if (wait) return this.waitForTx(id, undefined, tx.valid_until);
    try {
      return await this.tx(id);
    } catch (e) {
      throw new TxPendingError(id, 0, e);
    }
  }

  async faucet(to: string, wait = true) {
    const { id } = await this.call("/faucet", { to });
    return wait ? this.waitForTx(id) : this.tx(id);
  }

  /**
   * Polls a tx until it's finalized (returns it) or failed (`TxFailedError`, which says whether it
   * executed or was dropped unrun). A poll that fails (rate limited, a node error, a timeout, not
   * visible yet) is retried; after `timeoutMs` it throws `TxPendingError`, since the tx may still
   * execute. `validUntil` (the tx's, when the caller has it) goes into a dropped tx's error.
   */
  async waitForTx(id: string, timeoutMs = 15_000, validUntil?: number) {
    const start = Date.now();
    let lastError: unknown;
    for (;;) {
      try {
        const tx = await this.tx(id);
        if (tx.status === "finalized") return tx;
        if (tx.status === "failed") throw new TxFailedError(id, tx.error ?? "unknown reason", tx, validUntil);
      } catch (e) {
        if (e instanceof TxFailedError || (e instanceof ApiError && e.status === 400)) throw e;
        lastError = e;
      }
      if (Date.now() - start > timeoutMs) throw new TxPendingError(id, timeoutMs, lastError);
      await new Promise((r) => setTimeout(r, 250));
    }
  }
}
