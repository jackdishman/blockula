import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, randomBytes, utf8ToBytes } from "@noble/hashes/utils.js";
import { bech32m } from "@scure/base";
import { minFee, parseFang, parseSoul, seanceCommitment, signingBytes, type Action, type KeyRole, type Tx, type UnsignedTx } from "./tx.js";

export * from "./tx.js";

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
   * Builds and signs a tx: `fee` = `minFee + tip` (exactly what's charged) and `valid_until` =
   * the current height + `validForBlocks`.
   */
  async build(action: Action, memo = "", opts: TxOptions = {}): Promise<Tx> {
    await this.client.addressPrefix(); // so `this.account` uses the chain's prefix
    const [status, account] = await Promise.all([this.client.status(), this.client.account(this.account).catch(() => null)]);
    const max: number = status.max_tx_validity_blocks ?? Number.MAX_SAFE_INTEGER;
    const validFor = opts.validForBlocks ?? Math.min(this.validForBlocks, max);
    if (!Number.isSafeInteger(validFor) || validFor < 1) throw new Error("validForBlocks must be a positive whole number of blocks");
    if (validFor > max) throw new Error(`validForBlocks can be at most ${max} (an hour of blocks)`);
    const tip = opts.tip === undefined ? this.tip : parseFang(opts.tip);
    const unsigned: UnsignedTx = { chain_id: status.chain_id, from: this.account, nonce: account?.next_nonce ?? 0, fee: "0", tip: tip.toString(), valid_until: status.height + validFor, action, memo };
    unsigned.fee = (minFee(unsigned) + tip).toString();
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
   * Offer a validator: bonds `bond` FANG from this account. `consensusKeyHex` is the validator
   * node's ML-DSA-65 public key (see `blockula validator init`). Pending until an admin approves.
   */
  async registerValidator(opts: { consensusKey: string; p2pAddr: string; httpAddr: string; bond: string | number | bigint; pop: string }) {
    return this.client.submit(await this.build({ type: "register_validator", consensus_key: opts.consensusKey, p2p_addr: opts.p2pAddr, http_addr: opts.httpAddr, bond: parseFang(opts.bond).toString(), pop: opts.pop }));
  }

  /** Season 0 admins only. */
  async approveValidator(validator: string) {
    return this.client.submit(await this.build({ type: "approve_validator", validator }));
  }

  /** Season 0 admins only. */
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

  /** Season 0 admins only. Weight 1–100; 0 removes the repo. */
  async setRepo(repo: string, weight: number) {
    return this.client.submit(await this.build({ type: "set_repo", repo, weight }));
  }

  /** Season 0 admins only. `soul` in Soul ("2.5"), up to 100 per grant. */
  async grantSoul(to: string, soul: string | number, reason: string) {
    return this.client.submit(await this.build({ type: "grant_soul", to: await this.client.resolve(to), amount: parseSoul(soul), reason }));
  }

  /** Season 0 admins only. */
  async rejectClaim(claimId: number, reason: string) {
    return this.client.submit(await this.build({ type: "reject_claim", claim_id: claimId, reason }));
  }

  /** Season 0 admins only. */
  async flagSybil(account: string, reason: string) {
    return this.client.submit(await this.build({ type: "flag_sybil", account: await this.client.resolve(account), reason }));
  }

  /** Season 0 admins only. */
  async clawback(account: string, reason: string) {
    return this.client.submit(await this.build({ type: "clawback", account: await this.client.resolve(account), reason }));
  }

  /** Reclaim the bond of a removed validator this account owns. */
  async unbond(validator: string) {
    return this.client.submit(await this.build({ type: "unbond", validator }));
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

export class Client {
  /** The chain's address prefix: given, learned from `/api/status`, or `boo` until then. */
  prefix: string;
  private prefixKnown: boolean;
  private readonly pinnedPrefix?: string;

  /** `opts.prefix` pins the address prefix you expect (`boo` or `tboo`); a node reporting another is refused. */
  constructor(readonly rpc: string = DEFAULT_RPC, opts: { prefix?: string } = {}) {
    if (opts.prefix !== undefined && !(KNOWN_PREFIXES as readonly string[]).includes(opts.prefix)) throw new Error(`unknown address prefix ${opts.prefix}`);
    this.pinnedPrefix = opts.prefix;
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
    if (typeof s.address_prefix === "string") {
      if (this.pinnedPrefix !== undefined && s.address_prefix !== this.pinnedPrefix) {
        throw new Error(`${this.rpc} is a ${s.address_prefix} chain, but this client expects ${this.pinnedPrefix} addresses`);
      }
      this.prefix = s.address_prefix;
      this.prefixKnown = true;
    }
    return s;
  }
  blocks(limit = 20) { return this.call(`/blocks?limit=${limit}`); }
  block(heightOrHash: number | string) { return this.call(`/block/${heightOrHash}`); }
  tx(id: string) { return this.call(`/tx/${id}`); }
  account(addressOrName: string) { return this.call(`/account/${encodeURIComponent(addressOrName)}`); }
  name(name: string) { return this.call(`/name/${encodeURIComponent(name)}`); }
  /** Accounts a key is authorized on (its own address, plus any it was added to). */
  keyAccounts(keyIdHex: string) { return this.call(`/key/${keyIdHex}`); }
  search(q: string) { return this.call(`/search/${encodeURIComponent(q)}`); }
  /** Active set for the current epoch, pending/approved validators, next-epoch set, epoch timing. */
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

  async resolve(nameOrAddress: string): Promise<string> {
    if (isAddress(nameOrAddress)) return nameOrAddress;
    if (/^t?boo1/i.test(nameOrAddress.trim())) throw new Error(`${nameOrAddress} isn't a valid address (check for typos; addresses are lowercase)`);
    const r = await this.name(nameOrAddress);
    if (r.available) throw new Error(`${r.name} isn't registered`);
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
