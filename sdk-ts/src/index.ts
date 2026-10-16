import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, randomBytes, utf8ToBytes } from "@noble/hashes/utils.js";
import { bech32m } from "@scure/base";
import { baseFee, parseFang, parseSoul, seanceCommitment, signingBytes, type Action, type BaseFee, type KeyRole, type ProposalKind, type Tx, type UnsignedTx } from "./tx.js";
import { NAMESPACES, checkProofResponse, decodeAccount, decodeName, type ProofResponse, type ProvenRecord } from "./proof.js";

export * from "./tx.js";
export * from "./proof.js";
export * from "./distribution.js";
export * from "./release.js";

export const DEFAULT_RPC = "http://localhost:7070";
/** Address prefixes: `boo` on mainnet (and dev chains), `tboo` on public testnets. */
export const KNOWN_PREFIXES = ["boo", "tboo"] as const;
export const DEFAULT_PREFIX = "boo";
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
   * `distributionId` is the id the tree was built for (`buildDistribution(id, …)`), which must be the id this proposal
   * gets (`client.proposals()`'s `next_id`): if another proposal takes it first, the node refuses this one (nothing is
   * escrowed), so rebuild the tree for the new `next_id` and propose again.
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
  /** Switch to protocol `version` at block `activationHeight` (needs a supermajority). */
  scheduleUpgrade(version: number, activationHeight: number): ProposalKind {
    if (!Number.isInteger(version) || version < 1 || version > 0xffffffff) throw new Error("version must be a positive 32-bit number");
    return { type: "schedule_upgrade", version, activation_height: wholeNumber(activationHeight, "activationHeight") };
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

/** Shares worth about `value` drops in a delegation (rounded down, at most all of them). */
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
  /** What it does, in plain English (the node's summary). */
  summary: string;
  kind: ProposalKind | null;
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
    kind: p.kind && typeof p.kind === "object" ? (p.kind as ProposalKind) : null,
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
  /** Paid to the block's proposer on top of the base fee: FANG ("0.001") or bigint drops. Default: the wallet's `tip` (0). */
  tip?: string | number | bigint;
  /** Blocks from now the tx stays valid (at most the chain's `max_tx_validity_blocks`, an hour of blocks). Default: the wallet's `validForBlocks` (600). */
  validForBlocks?: number;
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
  }

  get keyId() {
    return keyId(this.publicKey);
  }

  /** Act for another account this key has been authorized on. Accepts a name or address. */
  async actAs(account: string) {
    this.acting = await this.client.resolve(account);
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
   * Builds and signs a tx: `fee` = the highest base fee already fixed for the next blocks
   * (`/api/fees` `suggested`) + `tip`, `valid_until` = the current height + `validForBlocks`, and
   * the chain's id and genesis hash from `/api/status`. Only the base fee of the block that
   * includes it (+ the tip) is charged. To sign offline, build an `UnsignedTx` yourself (with the
   * chain's `genesis_hash`) and `sign` it.
   */
  async build(action: Action, memo = "", opts: TxOptions = {}): Promise<Tx> {
    await this.client.addressPrefix(); // so `this.account` uses the chain's prefix
    const [status, account, fees] = await Promise.all([this.client.status(), this.client.account(this.account).catch(() => null), this.client.fees()]);
    const max: number = status.max_tx_validity_blocks ?? Number.MAX_SAFE_INTEGER;
    const validFor = opts.validForBlocks ?? Math.min(this.validForBlocks, max);
    if (!Number.isSafeInteger(validFor) || validFor < 1) throw new Error("validForBlocks must be a positive whole number of blocks");
    if (validFor > max) throw new Error(`validForBlocks can be at most ${max} (an hour of blocks)`);
    const tip = opts.tip === undefined ? this.tip : parseFang(opts.tip);
    if (typeof status.genesis_hash !== "string") throw new Error(`${this.client.rpc} doesn't report a genesis_hash; it's too old for this SDK`);
    // Binds the tx to this chain instance (not just its id), so it can't replay on a relaunch.
    const unsigned: UnsignedTx = { chain_id: status.chain_id, genesis_hash: status.genesis_hash, from: this.account, nonce: account?.next_nonce ?? 0, fee: "0", tip: tip.toString(), valid_until: status.height + validFor, action, memo };
    unsigned.fee = (baseFee(unsigned, fees.suggested) + tip).toString();
    return this.sign(unsigned);
  }

  /** Send FANG to a `.boo` name or an address. `amount` is in FANG ("1.5") or bigint drops. */
  async send(to: string, amount: string | number | bigint, opts: { memo?: string; wait?: boolean } & TxOptions = {}) {
    // Sign the resolved address, not the name: what you checked is exactly who gets paid,
    // even if the name changes hands before the block is sealed.
    const recipient = await this.client.resolve(to);
    const tx = await this.build({ type: "transfer", to: recipient, amount: parseFang(amount).toString() }, opts.memo, opts);
    return this.client.submit(tx, opts.wait ?? true);
  }

  async registerName(name: string, opts: { wait?: boolean } & TxOptions = {}) {
    const tx = await this.build({ type: "register_name", name }, "", opts);
    return this.client.submit(tx, opts.wait ?? true);
  }

  /** Authorize another device's key (e.g. a second passkey) with full control. */
  async addOwnerKey(keyIdHex: string, label: string) {
    return this.client.submit(await this.build({ type: "add_key", key_id: keyIdHex, label, role: { type: "owner" } }));
  }

  /**
   * Let an agent/app key spend from this account, transfers only, within a limit.
   * Example: `addSessionKey(agentKeyId, "claude", { limit: "10", periodBlocks: 86_400, expiresInBlocks: 30 * 86_400 })`.
   */
  async addSessionKey(keyIdHex: string, label: string, opts: { limit: string | number | bigint; periodBlocks: number; expiresInBlocks: number }) {
    if (!Number.isSafeInteger(opts.periodBlocks) || opts.periodBlocks < 1) throw new Error("periodBlocks must be a positive whole number of blocks");
    if (!Number.isSafeInteger(opts.expiresInBlocks) || opts.expiresInBlocks < 1) throw new Error("expiresInBlocks must be a positive whole number of blocks");
    const { height } = await this.client.status();
    const role: KeyRole = { type: "session", limit: parseFang(opts.limit).toString(), period_blocks: opts.periodBlocks, expires_at: height + opts.expiresInBlocks };
    return this.client.submit(await this.build({ type: "add_key", key_id: keyIdHex, label, role }));
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
    const { height } = await this.client.status();
    return this.client.submit(await this.build({ type: "create_hunt", repo: opts.repo, issue: opts.issue, amount: parseFang(opts.amount).toString(), expires_at: height + opts.expiresInBlocks, title: opts.title }));
  }

  async fundHunt(huntId: number, amount: string | number | bigint) {
    return this.client.submit(await this.build({ type: "fund_hunt", hunt_id: huntId, amount: parseFang(amount).toString() }));
  }

  /** Pay a hunt's bounty (creator or admin). `to` may be a .boo name; it's resolved and the address is signed. */
  async awardHunt(huntId: number, to: string, pr: number, familiarKey?: string) {
    const addr = await this.client.resolve(to);
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
    return this.client.submit(await this.build({ type: "vouch", account: await this.client.resolve(account) }));
  }

  /** Withdraw your vouch for `account` (after the lock), or pass your own account to decline one. */
  async unvouch(account: string) {
    return this.client.submit(await this.build({ type: "unvouch", account: await this.client.resolve(account) }));
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
   */
  async sealVotes(seanceId: number, votes: string, spam: number[] = [], salt: string = bytesToHex(randomBytes(16))) {
    await this.client.addressPrefix(); // the commitment names this account
    const commitment = seanceCommitment(seanceId, this.account, votes, spam, salt);
    const tx = await this.client.submit(await this.build({ type: "commit_votes", seance_id: seanceId, commitment }));
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
    return this.client.submit(await this.build({ type: "grant_soul", to: await this.client.resolve(to), amount: parseSoul(soul), reason }));
  }

  /** Admins of dev and test chains only (elsewhere: a governance proposal). */
  async rejectClaim(claimId: number, reason: string) {
    return this.client.submit(await this.build({ type: "reject_claim", claim_id: claimId, reason }));
  }

  /** Admins of dev and test chains only (elsewhere: a governance proposal). */
  async flagSybil(account: string, reason: string) {
    return this.client.submit(await this.build({ type: "flag_sybil", account: await this.client.resolve(account), reason }));
  }

  /** Admins of dev and test chains only (elsewhere: a governance proposal). */
  async clawback(account: string, reason: string) {
    return this.client.submit(await this.build({ type: "clawback", account: await this.client.resolve(account), reason }));
  }

  /** Reclaim the bond of a removed validator this account owns. */
  async unbond(validator: string) {
    return this.client.submit(await this.build({ type: "unbond", validator }));
  }

  // ---- protocol v1 (tags 30–49). Each builds, signs and submits one tx and waits for it. ----

  /** Submits one action (the shared tail of the helpers below). */
  private async act(action: Action, opts: TxOptions & { wait?: boolean; memo?: string } = {}) {
    return this.client.submit(await this.build(action, opts.memo ?? "", opts), opts.wait ?? true);
  }

  /** A validator by address, or by a `.boo` name its account holds. */
  private async validatorAddress(validator: string) {
    return this.client.resolve(validator);
  }

  /** Stake `amount` (FANG, or bigint drops) in a validator's pool. Your stake earns its share of issuance and is slashed with the pool. */
  async delegate(validator: string, amount: Amount, opts: TxOptions = {}) {
    return this.act({ type: "delegate", validator: await this.validatorAddress(validator), amount: positive(parseFang(amount), "delegate").toString() }, opts);
  }

  /**
   * Start unbonding `shares` of your stake in a validator's pool (`"all"`: every share you hold
   * there, read from `/api/staking`). It stops earning now and can be withdrawn with
   * `withdrawUnbonded` once the unbonding period (21 days on mainnet) is over.
   */
  async undelegate(validator: string, shares: bigint | string | "all", opts: TxOptions = {}) {
    const v = await this.validatorAddress(validator);
    return this.act({ type: "undelegate", validator: v, shares: (await this.sharesOf(v, shares)).toString() }, opts);
  }

  /** Move `shares` (or `"all"`) from one pool to another without waiting out the unbonding period. */
  async redelegate(fromValidator: string, toValidator: string, shares: bigint | string | "all", opts: TxOptions = {}) {
    const [from, to] = await Promise.all([this.validatorAddress(fromValidator), this.validatorAddress(toValidator)]);
    if (from === to) throw new Error("redelegate to a different validator");
    return this.act({ type: "redelegate", from_validator: from, to_validator: to, shares: (await this.sharesOf(from, shares)).toString() }, opts);
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
    return this.act({ type: "unjail", validator: await this.validatorAddress(validator) }, opts);
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
   * Put a proposal to a vote (build `kind` with `proposal.*`). Bonds `deposit` (default: the
   * chain's `proposal_deposit`), refunded once the proposal reaches quorum and burned if it
   * doesn't. Names in the kind (`to`, `account`) are resolved and the addresses are signed.
   */
  async propose(kind: ProposalKind, opts: { title: string; url?: string; deposit?: Amount } & TxOptions) {
    const title = opts.title.trim();
    if (!title || new TextEncoder().encode(title).length > 96 || /\p{Cc}/u.test(title)) throw new Error("title must be 1–96 bytes with no control characters");
    const url = opts.url ?? "";
    if (url && (!url.startsWith("https://") || url.length > 200 || /\s|\p{Cc}/u.test(url))) throw new Error("url must be an https:// link of at most 200 bytes");
    let deposit: bigint;
    if (opts.deposit !== undefined) deposit = parseFang(opts.deposit);
    else {
      const d = paramsOf(await this.client.params()).proposal_deposit;
      if (d === undefined) throw new Error("this node doesn't report proposal_deposit; pass deposit");
      deposit = BigInt(d);
    }
    return this.act({ type: "propose", kind: await this.resolveKind(kind), deposit: positive(deposit, "deposit").toString(), title, url }, opts);
  }

  /**
   * Vote on a proposal with `amount` of your unlocked holdings (liquid, staked or unbonding;
   * unvested tokens can't vote). `conviction` 0–6 locks `amount` for 0, 1, 2, 4, 8, 16 or 32 lock
   * periods after the vote ends, for 0.1×, 1×, 2× … 6× the weight (see `convictionWeight`).
   * Voting again replaces your vote. A validator's consensus key or owner voting before the
   * bootstrap council ends also casts that validator's council vote.
   */
  async vote(proposalId: number, aye: boolean, amount: Amount, conviction = 1, opts: TxOptions = {}) {
    if (!Number.isInteger(conviction) || conviction < 0 || conviction > MAX_CONVICTION) throw new Error(`conviction must be 0–${MAX_CONVICTION}`);
    return this.act({ type: "vote", proposal_id: wholeNumber(proposalId, "proposalId"), aye, amount: positive(parseFang(amount), "vote").toString(), conviction }, opts);
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

  async multisigApprove(id: number, opts: TxOptions = {}) {
    return this.act({ type: "multisig_approve", id: wholeNumber(id, "id") }, opts);
  }

  /** Extend a name you own by `years` (1–10), at the chain's renewal price per year. */
  async renewName(name: string, years = 1, opts: TxOptions = {}) {
    if (!Number.isInteger(years) || years < 1 || years > 10) throw new Error("renew for 1–10 years");
    return this.act({ type: "renew_name", name: bareName(name), years }, opts);
  }

  /**
   * Offer a name you own to another account (a `.boo` name or address; the address is signed). It
   * stays yours until they accept it (`acceptName`); offering it again replaces the offer, and
   * `withdrawNameOffer` withdraws it. Nobody can be given a name without saying yes.
   */
  async transferName(name: string, to: string, opts: TxOptions = {}) {
    return this.act({ type: "transfer_name", name: bareName(name), to: await this.client.resolve(to) }, opts);
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

  /** A proposal kind with its names resolved to addresses (what's signed is who's paid or affected). */
  private async resolveKind(kind: ProposalKind): Promise<ProposalKind> {
    switch (kind.type) {
      case "treasury_spend":
      case "grant_soul":
        return { ...kind, to: await this.client.resolve(kind.to) };
      case "flag_sybil":
      case "clawback":
        return { ...kind, account: await this.client.resolve(kind.account) };
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

/** An error response from a Blockula node. `status` is the HTTP status (429 = rate limited). */
export class ApiError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "ApiError";
  }
}

/** A transaction that was rejected at execution (fee charged) or dropped from the mempool. */
export class TxFailedError extends Error {
  constructor(readonly id: string, readonly reason: string, readonly tx: any) {
    super(`tx ${id} failed: ${reason}`);
    this.name = "TxFailedError";
  }
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
}

export class Client {
  /** The chain's address prefix: given, learned from `/api/status`, or `boo` until then. */
  prefix: string;
  private prefixKnown: boolean;
  private readonly pinnedPrefix?: string;
  private readonly pinnedChainId?: string;
  private readonly pinnedGenesis?: string;

  /** `opts` pins the chain: a node reporting another prefix, chain id or genesis hash is refused with `ChainMismatchError`. */
  constructor(readonly rpc: string = DEFAULT_RPC, opts: ClientOptions = {}) {
    if (opts.prefix !== undefined && !(KNOWN_PREFIXES as readonly string[]).includes(opts.prefix)) throw new Error(`unknown address prefix ${opts.prefix}`);
    if (opts.genesisHash !== undefined && !/^[0-9a-f]{64}$/i.test(opts.genesisHash)) throw new Error("genesisHash must be 64 hex characters");
    this.pinnedPrefix = opts.prefix;
    this.pinnedChainId = opts.chainId;
    this.pinnedGenesis = opts.genesisHash?.toLowerCase();
    this.prefix = opts.prefix ?? DEFAULT_PREFIX;
    this.prefixKnown = opts.prefix !== undefined;
  }

  /** The chain's address prefix, asking the node once if it isn't known yet. */
  async addressPrefix(): Promise<string> {
    if (!this.prefixKnown) await this.status();
    return this.prefix;
  }

  private async call(path: string, body?: unknown) {
    const res = await fetch(this.rpc.replace(/\/$/, "") + "/api" + path, body === undefined ? undefined : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    const text = await res.text();
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
  fees(): Promise<{ height: number; lag: number; slots: ({ height: number; max_block_bytes: number } & BaseFee)[]; suggested: BaseFee; account_creation_fee: string; [k: string]: unknown }> { return this.call("/fees"); }
  /** The governance parameters (node/src/params.rs `ChainParams`; amounts as strings) under `params`, genesis constants under `genesis`, and the issuance year. `paramsOf` flattens it. */
  params() { return this.call("/params"); }
  /** Accounts a key is authorized on (its own address, plus any it was added to). */
  keyAccounts(keyIdHex: string) { return this.call(`/key/${keyIdHex}`); }
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
   */
  async provenRecord(namespace: number, key: string | Uint8Array, opts: { witnesses?: Client[]; waitMs?: number } = {}): Promise<ProvenRecord> {
    const r = checkProofResponse(await this.proof(namespace, key), namespace, key);
    await Promise.all((opts.witnesses ?? []).map((w) => w.confirmStateRoot(r.height, r.root, opts.waitMs ?? 5_000)));
    return r;
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

  /** An address's balance, nonce and keys, proven (see `provenRecord`). `null` if the account provably doesn't exist. */
  async provenAccount(address: string, opts: { witnesses?: Client[]; waitMs?: number } = {}) {
    if (!isAddress(address)) throw new Error(`${address} isn't an address`);
    const r = await this.provenRecord(NAMESPACES.ACCOUNTS, address, opts);
    return r.value === null ? null : { address, ...decodeAccount(r.value), height: r.height, root: r.root };
  }

  /** Who owns a `.boo` name, proven (see `provenRecord`). `null` if it provably isn't registered. */
  async provenName(name: string, opts: { witnesses?: Client[]; waitMs?: number } = {}) {
    const r = await this.provenRecord(NAMESPACES.NAMES, bareName(name), opts);
    return r.value === null ? null : { ...decodeName(r.value), height: r.height, root: r.root };
  }

  async resolve(nameOrAddress: string): Promise<string> {
    if (isAddress(nameOrAddress)) return nameOrAddress;
    if (/^t?boo1/i.test(nameOrAddress.trim())) throw new Error(`${nameOrAddress} isn't a valid address (check for typos; addresses are lowercase)`);
    const r = await this.name(nameOrAddress);
    if (r.status === "grace") throw new Error(`${r.name} has expired (at block ${r.expires_at})`);
    if (r.available || !r.owner) throw new Error(`${r.name} isn't registered`);
    return r.owner.address;
  }

  async submit(tx: Tx, wait = true) {
    const { id } = await this.call("/tx", tx);
    return wait ? this.waitForTx(id) : this.tx(id);
  }

  async faucet(to: string, wait = true) {
    const { id } = await this.call("/faucet", { to });
    return wait ? this.waitForTx(id) : this.tx(id);
  }

  async waitForTx(id: string, timeoutMs = 15_000) {
    const start = Date.now();
    for (;;) {
      const tx = await this.tx(id);
      if (tx.status === "finalized") return tx;
      if (tx.status === "failed") throw new TxFailedError(id, tx.error ?? "unknown reason", tx);
      if (Date.now() - start > timeoutMs) throw new Error(`tx ${id} still pending after ${timeoutMs}ms`);
      await new Promise((r) => setTimeout(r, 250));
    }
  }
}
