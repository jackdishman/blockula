// A small in-memory Blockula node behind Playwright routes: enough of the HTTP API for the
// wallet's flows (status, accounts, keys, names, faucet, tx submit/lookup) and for the explorer's
// pages (blocks, the mempool, search, hunts, the contribution pool, multisigs), in protocol v1's
// shapes (node/src/api.rs). Submitted txs are checked like the real node does it: ML-DSA
// signature over the SDK's signing bytes, the signing key authorized on `from` (owner keys for
// key changes), nonce and balance. e2e/explorer-world.ts fills it with a little of everything.
import { formatFang, keyId as keyIdOf, parseFang, proposalDigest, signingBytes, txSize, verifyKeyGrant, type ProposalKind, type Tx } from "@blockula/sdk";
import { hexToBytes } from "@noble/hashes/utils.js";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { createHash } from "node:crypto";
import type { BrowserContext, Page, Route } from "@playwright/test";
import { bech32m } from "@scure/base";
import { CHAIN_ID, GENESIS, MAINNET_GENESIS, MAINNET_RPC, RPC } from "./env";

type Role = { type: "owner" } | { type: "session"; limit: string; period_blocks: number; expires_at: number };
interface KeyRec { label: string; role: Role; added_at: number }
interface Acct { balance: bigint; nonce: number; keys?: Map<string, KeyRec> }
/**
 * A transaction as the node keeps it: signed (`tx`), or described (`action`, for the explorer's
 * world). `height` null: waiting in the mempool. `fee` is what it was charged (base fee + tip),
 * `maxFee` what it allowed; `toName` the `.boo` name it was signed to (`to`: who that paid).
 */
export interface TxRec {
  id: string;
  tx: Tx | null;
  action?: Record<string, unknown> | null;
  kind: string;
  from: string;
  to: string | null;
  toName?: string | null;
  amount: bigint | null;
  fee: bigint;
  maxFee?: bigint;
  tip?: bigint;
  memo: string;
  height: number | null;
  summary: string;
  status?: "finalized" | "failed" | "pending";
  error?: string | null;
  signer?: string | null;
  nonce?: number;
}

/** A lock on an account's FANG (`/api/account` `locks`). */
export interface Lock { id: string; reason: string; amount: bigint; until: number; vesting: boolean }

/** What `/api/name` adds for a name beyond its owner: when it lapses, a pending offer. */
export interface NameInfo { status?: "active" | "grace" | "available"; registered_at?: number; expires_at?: number; grace_until?: number; offered_to?: string }

type Json = Record<string, unknown>;

const FAUCET = addressOf("fa".repeat(32));
/** Opening an account burns this from its first payer (`account_creation_fee`, 0.1 FANG). */
const CREATION_FEE = 100_000_000n;
export const VALIDATOR = addressOf("7a".repeat(32));
export const OTHER_VALIDATOR = addressOf("7b".repeat(32));

export function addressOf(keyIdHex: string) {
  return bech32m.encode("boo", bech32m.toWords(hexToBytes(keyIdHex)));
}

export function keyIdOfAddress(address: string) {
  const { words } = bech32m.decode(address as `${string}1${string}`);
  return Buffer.from(bech32m.fromWords(words)).toString("hex");
}

class HttpError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
  }
}

/** The two chains the e2e builds are pinned to. */
export const CHAINS = {
  testnet: { rpc: RPC, chainId: CHAIN_ID, genesisHash: GENESIS },
  mainnet: { rpc: MAINNET_RPC, chainId: "blockula-1", genesisHash: MAINNET_GENESIS },
};

export class MockNode {
  readonly rpc: string;
  chainId: string;
  genesisHash: string;
  /** Extra /api/status fields (e.g. a v1 node's address_prefix). */
  statusExtra: Record<string, unknown> = {};
  /** What `/api/genesis` serves (null: 404, as from a node that doesn't serve it). */
  genesis: Record<string, unknown> | null = null;
  faucet = true;
  height = 100;
  accounts = new Map<string, Acct>();
  names = new Map<string, string>();
  /** What `/api/name/{name}` answers instead of the truth (a lying node; gap-7-1). Execution uses `names`. */
  nameLies = new Map<string, string>();
  txs = new Map<string, TxRec>();
  /** What `/api/tx/{id}` answers as is, before any recorded tx: e.g. node/src/api.rs's answers from its mempool's memory, without the tx (a drop, a failure that ran). */
  txAnswers = new Map<string, unknown>();
  /** Runs just before `/api/account/{id}` answers: a block executing between two of the page's reads. */
  beforeAccountRead: ((id: string) => void) | null = null;
  /** Every signed tx accepted, in order. */
  submitted: Tx[] = [];
  /** Every request body the wallet sent to the node, to prove secrets never leave the page. */
  bodies: string[] = [];
  /** Protocol v1 modules: off, the node answers 404 like a node without them. */
  staking = true;
  governance = true;
  /** Validator pools: stake in drops and commission. */
  pools = new Map<string, { name: string | null; stake: bigint; shares: bigint; commission_bps: number }>([
    [VALIDATOR, { name: null, stake: 1_000_000n * 10n ** 9n, shares: 1_000_000n * 10n ** 9n, commission_bps: 500 }],
    [OTHER_VALIDATOR, { name: null, stake: 500_000n * 10n ** 9n, shares: 500_000n * 10n ** 9n, commission_bps: 1000 }],
  ]);
  /** Shares by account, then validator. */
  shares = new Map<string, Map<string, bigint>>();
  /** A lying node: what `/api/staking` reports for a delegation, given the truth (audit gap-7-5). */
  reportStake: ((truth: { shares: bigint; value: bigint }) => { shares: bigint; value: bigint }) | null = null;
  unbonding = new Map<string, { id: number; validator: string; amount: bigint; started_at: number; release_at: number }[]>();
  /** Governance proposals as node/src/api.rs serves them (`proposal_json`), before this mock adds its votes. */
  proposals: Record<string, unknown>[] = [];
  votes: { proposal_id: number; voter: string; aye: boolean; amount: bigint; conviction: number }[] = [];
  /** Failures the node remembers without their tx (`/api/tx`): dropped unrun, or ran in a block it no longer holds. */
  failures = new Map<string, { dropped: boolean; error: string; height: number }>();
  /** `/api/name` beyond the owner (expiry, grace, offers), and names reserved at genesis. */
  nameInfo = new Map<string, NameInfo>();
  reserved = new Set<string>();
  /** `/api/account` beyond balance and keys, by address: locks, GitHub, Familiars, hunts, contribution. */
  locks = new Map<string, Lock[]>();
  accountExtra = new Map<string, Json>();
  /** Programs, as node/src/api.rs serves them (`hunt_json`, `claim_json`, `seance_json`…). */
  hunts: Json[] = [];
  claims: Json[] = [];
  seances: Json[] = [];
  souls: Json[] = [];
  contributions: Json | null = null;
  multisigs = new Map<string, Json>();
  /** Merged into `/api/validators` (v1's leaving, tombstoned, admission…). */
  validatorsExtra: Json = {};
  /** When block 1 was sealed: blocks come a second apart. */
  private readonly genesisMs = Date.now() - 100_000;

  /** A node of one of the e2e chains, optionally at another URL (a "custom node"). */
  constructor(chain: keyof typeof CHAINS = "testnet", rpc?: string) {
    ({ rpc: this.rpc, chainId: this.chainId, genesisHash: this.genesisHash } = CHAINS[chain]);
    this.genesis = { chain_id: this.chainId, created_at_ms: this.genesisMs, block_time_ms: 1000, validators: [] };
    if (rpc) this.rpc = rpc;
  }

  /** Answer the wallet's RPC calls in this page or browser context. */
  async install(target: Page | BrowserContext) {
    await target.route(`${this.rpc}/api/**`, (route) => this.handle(route));
  }

  acct(address: string): Acct {
    let a = this.accounts.get(address);
    if (!a) this.accounts.set(address, (a = { balance: 0n, nonce: 0 }));
    return a;
  }

  /** The account's keys; an untouched account has just its own ("original") key. */
  keysOf(address: string): Map<string, KeyRec> {
    return this.acct(address).keys ?? new Map([[keyIdOfAddress(address), { label: "original key", role: { type: "owner" }, added_at: 0 }]]);
  }

  credit(address: string, fang: string) {
    this.acct(address).balance += parseFang(fang);
  }

  /** Labels the node gives system accounts ("🪦 contribution pool"). */
  systemLabels = new Map<string, string>();

  label(address: string) {
    const name = [...this.names].find(([, a]) => a === address)?.[0];
    return { address, name: this.systemLabels.get(address) ?? (name ? `${name}.boo` : address === VALIDATOR ? "🧛 validator" : null), short: `${address.slice(0, 10)}…${address.slice(-6)}` };
  }

  private record(r: Omit<TxRec, "id" | "height">) {
    this.height += 1;
    const id = this.nextId();
    this.txs.set(id, { ...r, id, height: this.height });
    return id;
  }

  /** A transaction id that looks like one (a hash), never all digits (which reads as a block height). */
  private nextId = () => createHash("sha256").update(`${this.chainId}/${this.txs.size + 1}`).digest("hex");

  /** Add a transaction as the explorer's world describes it: sealed in the next block, or `pending`. */
  add(r: Omit<TxRec, "id" | "height" | "tx"> & { id?: string }) {
    if (r.status === "pending") {
      const id = r.id ?? this.nextId();
      this.txs.set(id, { tx: null, ...r, id, height: null });
      return id;
    }
    this.height += 1;
    const id = r.id ?? this.nextId();
    this.txs.set(id, { tx: null, ...r, id, height: this.height });
    return id;
  }

  /** `tx_json`: charged `fee` (base fee + `tip`) out of `max_fee`, `to` as it was paid, `to_name` as it was signed. */
  private txView(r: TxRec) {
    return {
      id: r.id,
      summary: r.summary,
      kind: r.kind,
      status: r.status ?? (r.height == null ? "pending" : "finalized"),
      error: r.error ?? null,
      height: r.height,
      from: this.label(r.from),
      to: r.to ? this.label(r.to) : null,
      to_name: r.toName ?? null,
      amount: r.amount?.toString() ?? null,
      amount_display: r.amount !== null ? formatFang(r.amount) : null,
      fee: r.fee.toString(),
      fee_display: formatFang(r.fee),
      max_fee: (r.maxFee ?? r.fee).toString(),
      tip: (r.tip ?? 0n).toString(),
      valid_until: (r.height ?? this.height) + 600,
      size_bytes: r.tx ? txSize(r.tx) : 5400,
      nonce: r.nonce ?? r.tx?.nonce ?? 0,
      signer_key: r.signer ?? (r.tx ? keyIdOf(hexToBytes(r.tx.public_key)) : null),
      memo: r.memo,
      action: r.tx?.action ?? r.action ?? null,
      signature_scheme: "ML-DSA-65 (FIPS 204)",
    };
  }

  private blockHash = (h: number) => (h * 2654435761).toString(16).padStart(8, "0").repeat(8).slice(0, 64);

  /** `header_json`: one block a second, proposed by the validator. */
  private header(h: number) {
    const n = this.txsAt(h).length;
    return { height: h, hash: this.blockHash(h), prev_hash: this.blockHash(h - 1), timestamp_ms: this.genesisMs + h * 1000, proposer: this.label(VALIDATOR), state_root: this.blockHash(h + 7), tx_count: n, version: 1, signal: 1, summary: `Block #${h} sealed with ${n} transaction${n === 1 ? "" : "s"}` };
  }

  private txsAt = (h: number) => [...this.txs.values()].filter((t) => t.height === h);

  /** `block_json`: the header, its base fee, its transactions and its certificate's size. */
  private block(h: number) {
    const txs = this.txsAt(h);
    return { ...this.header(h), view: h, parent_view: h - 1, base_fee: { flat: "10000", rate: "10000", max_block_bytes: 2_097_152 }, txs: txs.map((t) => this.txView(t)), signature_bytes: 3354, failed_count: txs.filter((t) => t.status === "failed").length };
  }

  private status() {
    return {
      chain_id: this.chainId,
      genesis_hash: this.genesisHash,
      token: "FANG",
      decimals: 9,
      height: this.height,
      block_time_ms: 1000,
      validator: this.label(VALIDATOR),
      faucet: this.faucet,
      supply: "1000000000000000",
      supply_display: "1,000,000 FANG",
      burned: "0",
      burned_display: "0 FANG",
      epoch: 3,
      active_validators: this.pools.size,
      accounts: this.accounts.size,
      names: this.names.size,
      mempool: [...this.txs.values()].filter((t) => t.height == null).length,
      history_from: 1,
      head_age_ms: 500,
      finalized_height: this.height,
      ...this.statusExtra,
    };
  }

  private resolve(id: string) {
    const q = id.toLowerCase();
    if (q.startsWith("boo1")) return q;
    const name = q.replace(/\.boo$/, "");
    const owner = this.names.get(name);
    if (!owner) throw new HttpError(404, `no one has claimed ${name}.boo yet`);
    // As node/src/chain.rs `State::resolve`: an expired name points to nobody.
    const info = this.nameInfo.get(name);
    if (info?.status === "grace") throw new HttpError(400, `${name}.boo expired at block ${info.expires_at ?? 0}`);
    return owner;
  }

  /** A transfer's recipient whose account it opens (it's credited before the fee is charged). */
  private opening: string | null = null;
  /** Spent this period, per session key (`/api/account` keys). */
  spent = new Map<string, bigint>();

  private account(id: string) {
    const address = this.resolve(id);
    const exists = this.accounts.has(address);
    const a = this.acct(address);
    const keys = [...this.keysOf(address)].map(([key_id, k]) => ({ key_id, label: k.label, role: k.role, added_at: k.added_at, spent_this_period: (this.spent.get(key_id) ?? 0n).toString(), window_start: Math.max(0, this.height - 10) }));
    // Pending first, then the newest 25 sealed (node/src/api.rs `account`).
    const mine = [...this.txs.values()].filter((t) => t.from === address || t.to === address).reverse();
    const recent = [...mine.filter((t) => t.height == null), ...mine.filter((t) => t.height != null).slice(0, 25)].map((t) => this.txView(t));
    const locks = (this.locks.get(address) ?? []).filter((l) => l.amount > 0n);
    const locked = locks.reduce((m, l) => (l.amount > m ? l.amount : m), 0n);
    const transferable = a.balance > locked ? a.balance - locked : 0n;
    return {
      account: this.label(address),
      balance: a.balance.toString(),
      balance_display: formatFang(a.balance),
      transferable: transferable.toString(),
      transferable_display: formatFang(transferable),
      locked: locked.toString(),
      locked_display: formatFang(locked),
      locks: locks.map((l) => ({ id: l.id, reason: l.reason, amount: l.amount.toString(), amount_display: formatFang(l.amount), until: l.until, vesting: l.vesting })),
      exists,
      creation_fee: exists ? "0" : CREATION_FEE.toString(),
      nonce: a.nonce,
      next_nonce: a.nonce + mine.filter((t) => t.height == null && t.from === address).length,
      names: [...this.names].filter(([, o]) => o === address).map(([n]) => `${n}.boo`),
      keys,
      recent_txs: recent,
      github: null,
      familiars: [],
      hunts_created: [],
      hunts_won: [],
      contribution: null,
      ...this.accountExtra.get(address),
    };
  }

  private keyAccounts(keyId: string) {
    const accounts = [...this.accounts.keys()]
      .filter((addr) => this.acct(addr).keys?.has(keyId))
      .map((addr) => {
        const k = this.keysOf(addr).get(keyId) as KeyRec;
        return { account: this.label(addr), role: k.role, label: k.label };
      });
    const own = addressOf(keyId);
    if (!accounts.some((a) => a.account.address === own) && this.keysOf(own).has(keyId)) {
      accounts.unshift({ account: this.label(own), role: { type: "owner" }, label: "original key" });
    }
    return { key_id: keyId, accounts };
  }

  private submit(tx: Tx) {
    if (tx.chain_id !== this.chainId) throw new HttpError(400, `wrong chain id ${tx.chain_id}`);
    if (tx.genesis_hash !== this.genesisHash) throw new HttpError(400, `signed for another instance of ${this.chainId}`);
    const pk = hexToBytes(tx.public_key);
    if (!ml_dsa65.verify(hexToBytes(tx.signature), signingBytes(tx), pk)) throw new HttpError(400, "bad signature");
    const signer = keyIdOf(pk);
    const key = this.keysOf(tx.from).get(signer);
    if (!key) throw new HttpError(400, `this key isn't authorized on ${tx.from}`);
    const a = this.acct(tx.from);
    if (tx.nonce !== a.nonce) throw new HttpError(400, `bad nonce: expected ${a.nonce}, got ${tx.nonce}`);
    const fee = BigInt(tx.fee);
    const act = tx.action;
    const amount = act.type === "transfer" ? BigInt(act.amount) : 0n;
    if (a.balance < amount + fee) throw new HttpError(400, `insufficient balance: has ${formatFang(a.balance)}, needs ${formatFang(amount + fee)}`);
    let to: string | null = null;
    let summary: string;
    if (act.type === "add_key" || act.type === "remove_key") {
      if (key.role.type !== "owner") throw new HttpError(400, `only owner keys can ${act.type === "add_key" ? "add" : "remove"} keys`);
      const keys = new Map(this.keysOf(tx.from));
      if (act.type === "add_key") {
        // As node/src/types.rs `verify_key_pop`: the added key consents to this account (gap-7-2).
        if (!verifyKeyGrant({ account: tx.from, key_id: act.key_id, public_key: act.public_key, pop: act.pop }, this.chainId, this.genesisHash)) throw new HttpError(400, "the added key's proof of possession doesn't verify");
        keys.set(act.key_id, { label: act.label, role: act.role, added_at: this.height + 1 });
        summary = `${this.label(tx.from).name ?? this.label(tx.from).short} added key ${act.key_id.slice(0, 8)}… (${act.label}) as ${act.role.type === "owner" ? "an owner" : "a session key"}`;
      } else {
        if (![...keys].some(([id, k]) => id !== act.key_id && k.role.type === "owner")) throw new HttpError(400, "can't remove the last owner key");
        keys.delete(act.key_id);
        summary = `${this.label(tx.from).name ?? this.label(tx.from).short} revoked key ${act.key_id.slice(0, 8)}…`;
      }
      a.keys = keys;
    } else if (act.type === "transfer") {
      // As node/src/chain.rs: a name in `to` is resolved when the transfer runs.
      to = this.resolve(act.to);
      if (!this.accounts.has(to) && to !== tx.from && a.balance < amount + fee + CREATION_FEE) throw new HttpError(400, `opening 1 account burns 0.1 FANG: insufficient balance`);
      if (!this.accounts.has(to)) this.opening = to;
      this.acct(to).balance += amount;
      summary = `${this.label(tx.from).short} sent ${formatFang(amount)} to ${this.label(to).short}`;
    } else if (act.type === "register_name") {
      this.names.set(act.name, tx.from);
      summary = `${this.label(tx.from).short} claimed ${act.name}.boo`;
    } else if (act.type === "delegate") {
      const pool = this.pools.get(act.validator);
      if (!pool || !this.staking) throw new HttpError(400, "not active yet");
      const value = BigInt(act.amount);
      if (a.balance < value + fee) throw new HttpError(400, `insufficient balance: has ${formatFang(a.balance)}, needs ${formatFang(value + fee)}`);
      const shares = (value * pool.shares) / pool.stake;
      pool.stake += value;
      pool.shares += shares;
      const mine = this.shares.get(tx.from) ?? new Map<string, bigint>();
      mine.set(act.validator, (mine.get(act.validator) ?? 0n) + shares);
      this.shares.set(tx.from, mine);
      a.balance -= value;
      summary = `${this.label(tx.from).short} delegated ${formatFang(value)} to validator ${this.label(act.validator).short}`;
    } else if (act.type === "undelegate") {
      const pool = this.pools.get(act.validator);
      const mine = this.shares.get(tx.from);
      const held = mine?.get(act.validator) ?? 0n;
      let shares = BigInt(act.shares);
      // As node/src/staking.rs `shares_worth`: an amount is converted at the pool's price, rounded up (gap-7-5).
      const want = BigInt(act.amount ?? "0");
      if ((shares > 0n) === (want > 0n)) throw new HttpError(400, "undelegate a number of shares or an amount, not both");
      if (want > 0n) {
        if (!pool || !mine || want > (held * pool.stake) / pool.shares) throw new HttpError(400, "that's more than the stake is worth");
        shares = (want * pool.shares + pool.stake - 1n) / pool.stake;
        if (shares > held) shares = held;
      }
      if (!pool || !mine || shares > held) throw new HttpError(400, `you hold ${held} shares of that pool`);
      const value = (shares * pool.stake) / pool.shares;
      pool.stake -= value;
      pool.shares -= shares;
      mine.set(act.validator, held - shares);
      const entries = this.unbonding.get(tx.from) ?? [];
      this.unbonding.set(tx.from, [...entries, { id: entries.length, validator: act.validator, amount: value, started_at: this.height + 1, release_at: this.height + 10 }]);
      summary = want > 0n ? `${this.label(tx.from).short} started unbonding ${formatFang(want)} of their stake in validator ${this.label(act.validator).short}'s pool` : `${this.label(tx.from).short} started unbonding ${shares} shares of validator ${this.label(act.validator).short}'s pool`;
    } else if (act.type === "withdraw_unbonded") {
      const entries = this.unbonding.get(tx.from) ?? [];
      const ready = entries.filter((u) => u.release_at <= this.height + 1);
      if (!ready.length) throw new HttpError(400, "nothing has finished unbonding");
      a.balance += ready.reduce((t, u) => t + u.amount, 0n);
      this.unbonding.set(tx.from, entries.filter((u) => u.release_at > this.height + 1));
      summary = `${this.label(tx.from).short} withdrew their unbonded stake`;
    } else if (act.type === "vote") {
      if (!this.governance) throw new HttpError(400, "not active yet");
      const p = this.proposals.find((x) => x.id === act.proposal_id);
      if (!p) throw new HttpError(400, `no proposal #${act.proposal_id}`);
      // As node/src/governance.rs `vote`: the vote binds the proposal's content (gap-7-6).
      if (act.proposal_digest !== proposalDigest(p.kind as ProposalKind, String(p.title ?? ""), String(p.url ?? ""))) throw new HttpError(400, `proposal #${act.proposal_id} isn't the proposal this vote was signed for`);
      this.votes = this.votes.filter((v) => !(v.proposal_id === act.proposal_id && v.voter === tx.from));
      this.votes.push({ proposal_id: act.proposal_id, voter: tx.from, aye: act.aye, amount: BigInt(act.amount), conviction: act.conviction });
      summary = `${this.label(tx.from).short} voted ${act.aye ? "aye" : "nay"} on proposal #${act.proposal_id} with ${formatFang(BigInt(act.amount))}`;
    } else if (act.type === "unlock") {
      summary = `${this.label(tx.from).short} unlocked their expired vote locks`;
    } else {
      const who = this.label(tx.from).short;
      const held = (x: bigint) => {
        if (a.balance < x + fee) throw new HttpError(400, `insufficient balance: has ${formatFang(a.balance)}, needs ${formatFang(x + fee)}`);
        a.balance -= x;
      };
      switch (act.type) {
        case "create_hunt":
          held(BigInt(act.amount));
          this.hunts.push({ id: this.hunts.length, repo: act.repo, issue: act.issue, title: act.title, amount: act.amount, amount_display: formatFang(BigInt(act.amount)), creator: this.label(tx.from), funders: [{ account: this.label(tx.from), amount: act.amount, amount_display: formatFang(BigInt(act.amount)) }], status: "open", created_at: this.height + 1, expires_at: act.expires_at, awarded_to: null, awarded_by: null, awarded_at: null, pr: null, pr_url: null, familiar: null, self_awarded: false, cancelled_at: null });
          summary = `${who} put ${formatFang(BigInt(act.amount))} on ${act.repo}#${act.issue}`;
          break;
        case "fund_hunt":
          held(BigInt(act.amount));
          summary = `${who} added ${formatFang(BigInt(act.amount))} to hunt #${act.hunt_id}`;
          break;
        case "award_hunt": {
          const h = this.hunts.find((x) => x.id === act.hunt_id);
          if (!h) throw new HttpError(400, `no hunt #${act.hunt_id}`);
          if ((h.creator as { address: string }).address !== tx.from) throw new HttpError(400, "only the hunt's creator can award it");
          // As node/src/chain.rs: a name in `to` is resolved when the award runs.
          to = this.resolve(act.to);
          this.acct(to).balance += BigInt(String(h.amount));
          Object.assign(h, { status: "awarded", awarded_to: this.label(to), awarded_by: this.label(tx.from), awarded_at: this.height + 1, pr: act.pr });
          summary = `${who} awarded hunt #${act.hunt_id} to ${this.label(to).short}`;
          break;
        }
        case "fund_pool":
          held(BigInt(act.amount));
          summary = `${who} gave ${formatFang(BigInt(act.amount))} to the contribution pool`;
          break;
        case "vouch":
          to = this.resolve(act.account);
          held(10n * 10n ** 9n);
          summary = `${who} vouched for ${this.label(to).short}`;
          break;
        case "transfer_name":
          to = this.resolve(act.to);
          summary = to === tx.from ? `${who} accepted ${act.name}.boo` : `${who} offered ${act.name}.boo to ${this.label(to).short}`;
          break;
        case "renew_name":
          held(parseFang("1") * BigInt(act.years));
          summary = `${who} renewed ${act.name}.boo for ${act.years} year${act.years === 1 ? "" : "s"}`;
          break;
        case "cancel_hunt":
        case "release_name":
        case "register_familiar":
        case "link_github":
        case "submit_claim":
        case "join_jury":
        case "withdraw_vested":
        case "unjail":
        case "update_validator":
        case "multisig_propose":
          summary = `${who}: ${act.type.replace(/_/g, " ")}`;
          break;
        default:
          throw new HttpError(400, `the mock node doesn't do ${act.type}`);
      }
    }
    // Opening the recipient's account burns the creation fee from the sender (econ.rs `charge_new_accounts`).
    const opens = to && to !== tx.from && (this.opening === to || !this.accounts.has(to)) ? CREATION_FEE : 0n;
    this.opening = null;
    if (a.balance < amount + fee + opens) throw new HttpError(400, `opening 1 account burns 0.1 FANG: insufficient balance`);
    if (to) this.acct(to);
    a.balance -= amount + fee + opens;
    a.nonce += 1;
    if (key.role.type === "session") this.spent.set(signer, (this.spent.get(signer) ?? 0n) + amount + fee + opens);
    this.submitted.push(tx);
    const signedTo = act.type === "transfer" || act.type === "award_hunt" || act.type === "transfer_name" ? act.to : act.type === "vouch" ? act.account : null;
    const toName = signedTo && !/^t?boo1/.test(signedTo) ? `${signedTo.replace(/\.boo$/, "")}.boo` : null;
    return this.record({ tx, kind: act.type, from: tx.from, to, toName, amount: act.type === "transfer" ? amount : null, fee, maxFee: fee, tip: BigInt(tx.tip ?? 0), signer, nonce: tx.nonce, memo: tx.memo, summary });
  }

  /** One `validator_json` row, for the world's extra groups (leaving, jailed, tombstoned…). */
  validatorRow(address: string, extra: Json = {}) {
    return {
      validator: this.label(address), owner: this.label(address), status: "candidate", consensus_key: "", p2p_addr: "203.0.113.9:30300", http_addr: "",
      stake: "0", stake_display: "0 FANG", bond: "0", bond_display: "0 FANG", shares: "0", self_bond: "0", self_bond_display: "0 FANG", commission_bps: 500, commission_display: "5%",
      eligible: true, jailed: null, tombstoned_at: null, slashed_display: "0 FANG", liveness: null, genesis: false, registered_at: 1, ready_at: null, admitted: null, ...extra,
    };
  }

  /** `/api/validators` as node/src/api.rs serves it (`validator_json`). */
  private validators() {
    const active = [...this.pools].map(([address, p]) => ({
      validator: { ...this.label(address), name: p.name }, owner: this.label(address), status: "active", consensus_key: "", p2p_addr: "203.0.113.1:30300", http_addr: "",
      stake: p.stake.toString(), stake_display: formatFang(p.stake), bond: p.stake.toString(), bond_display: formatFang(p.stake), shares: p.shares.toString(),
      self_bond: p.stake.toString(), self_bond_display: formatFang(p.stake), commission_bps: p.commission_bps, commission_display: `${p.commission_bps / 100}%`,
      eligible: true, jailed: null, tombstoned_at: null, slashed_display: "0 FANG", liveness: null, genesis: true,
    }));
    return { epoch: 3, epoch_blocks: 3600, blocks_until_next_epoch: 100, active, next_epoch: active, joining: [], leaving: [], candidates: [], jailed: [], tombstoned: [], removed: [], admins: [], min_bond: "10000000000000", min_bond_display: "10000 FANG", max_validators: 31, churn_limit: 0, admission: null, ...this.validatorsExtra };
  }

  /** `/api/staking/{account}` as node/src/api.rs serves it. */
  private stakingOf(account: string) {
    const mine = this.shares.get(account) ?? new Map<string, bigint>();
    const delegations = [...mine].filter(([, s]) => s > 0n).map(([v, held]) => {
      const p = this.pools.get(v) as { stake: bigint; shares: bigint };
      const truth = { shares: held, value: (held * p.stake) / p.shares };
      const { shares, value } = this.reportStake ? this.reportStake(truth) : truth;
      return { validator: this.label(v), shares: shares.toString(), value: value.toString(), value_display: formatFang(value), principal: value.toString(), earned_display: "0 FANG", self_bond: false };
    });
    const next = this.height + 1;
    const entries = (this.unbonding.get(account) ?? []).map((u) => ({ id: u.id, type: "unbonding", validator: this.label(u.validator), amount: u.amount.toString(), amount_display: formatFang(u.amount), started_at: u.started_at, release_at: u.release_at, ready: u.release_at <= next }));
    const sum = (xs: bigint[]) => xs.reduce((t, x) => t + x, 0n);
    const bonded = sum(delegations.map((d) => BigInt(d.value)));
    const unbonding = sum((this.unbonding.get(account) ?? []).map((u) => u.amount));
    const withdrawable = sum((this.unbonding.get(account) ?? []).filter((u) => u.release_at <= next).map((u) => u.amount));
    return { account: this.label(account), bonded: bonded.toString(), unbonding: unbonding.toString(), withdrawable: withdrawable.toString(), delegations, entries, validators: [], summary: `${this.label(account).short} has ${formatFang(bonded)} staked` };
  }

  /** A proposal (`proposal_json`) with the votes cast here added to its tallies (weights at 0.1× … 6× conviction). */
  private proposalView(p: Record<string, unknown>) {
    const tenths = [1n, 10n, 20n, 30n, 40n, 50n, 60n];
    const mine = this.votes.filter((v) => v.proposal_id === p.id);
    const big = (d: bigint) => ({ drops: d.toString(), display: formatFang(d) });
    const side = (aye: boolean, base: unknown) => {
      const b = (base ?? {}) as { amount?: { drops: string }; weight?: { drops: string } };
      const votes = mine.filter((v) => v.aye === aye);
      return { amount: big(BigInt(b.amount?.drops ?? "0") + votes.reduce((t, v) => t + v.amount, 0n)), weight: big(BigInt(b.weight?.drops ?? "0") + votes.reduce((t, v) => t + (v.amount * tenths[v.conviction]) / 10n, 0n)) };
    };
    return { ...p, ayes: side(true, p.ayes), nays: side(false, p.nays), voters: Number(p.voters ?? 0) + mine.length };
  }

  private async handle(route: Route) {
    const req = route.request();
    const path = decodeURIComponent(new URL(req.url()).pathname.replace(/^\/api/, ""));
    const body = req.postData();
    if (body) this.bodies.push(body);
    const cors = { "access-control-allow-origin": "*", "access-control-allow-headers": "content-type", "access-control-allow-methods": "GET, POST" };
    const reply = (status: number, json: unknown) => route.fulfill({ status, contentType: "application/json", headers: cors, body: JSON.stringify(json) });
    if (req.method() === "OPTIONS") return route.fulfill({ status: 204, headers: cors });
    try {
      let m: RegExpMatchArray | null;
      if (req.method() === "GET" && path === "/status") return reply(200, this.status());
      if (req.method() === "GET" && path === "/genesis") return this.genesis ? reply(200, this.genesis) : reply(404, { error: "no genesis here" });
      if (req.method() === "GET" && (m = path.match(/^\/account\/(.+)$/))) {
        this.beforeAccountRead?.(m[1]);
        return reply(200, this.account(m[1]));
      }
      if (req.method() === "GET" && (m = path.match(/^\/key\/([0-9a-f]{64})$/))) return reply(200, this.keyAccounts(m[1]));
      if (req.method() === "GET" && (m = path.match(/^\/name\/(.+)$/))) {
        const n = m[1].toLowerCase().replace(/\.boo$/, "");
        const owner = this.nameLies.get(n) ?? this.names.get(n);
        const prices = { price: parseFang("5").toString(), price_display: "5 FANG", renewal_price: parseFang("1").toString(), renewal_price_display: "1 FANG", registration_blocks: 31_557_600, grace_blocks: 2_592_000 };
        // node/src/api.rs `name`: active, grace (expired; only its owner can renew it), reserved, available.
        if (this.reserved.has(n)) return reply(200, { name: `${n}.boo`, ...prices, status: "reserved", available: false });
        const info = this.nameInfo.get(n) ?? {};
        const status = info.status ?? "active";
        return reply(
          200,
          owner
            ? { name: `${n}.boo`, ...prices, owner: this.label(owner), registered_at: info.registered_at ?? 1, expires_at: info.expires_at ?? 31_557_601, grace_until: info.grace_until ?? 34_149_601, ...(info.offered_to ? { offered_to: this.label(info.offered_to) } : {}), status, available: status === "available" }
            : { name: `${n}.boo`, ...prices, status: "available", available: true },
        );
      }
      if (req.method() === "GET" && (m = path.match(/^\/tx\/([0-9a-f]{64})$/))) {
        if (this.txAnswers.has(m[1])) return reply(200, this.txAnswers.get(m[1]));
        const r = this.txs.get(m[1]);
        if (r) return reply(200, this.txView(r));
        // node/src/api.rs `tx`: a failure remembered without its transaction.
        const f = this.failures.get(m[1]);
        if (f) return reply(200, { id: m[1], status: "failed", dropped: f.dropped, error: f.error, height: f.height, summary: f.dropped ? `Transaction dropped without running: ${f.error}` : `Transaction failed: ${f.error}` });
        return reply(404, { error: "transaction not found" });
      }
      if (req.method() === "GET" && path === "/health") return reply(200, { ok: true, reason: null, halted: null, height: this.height, head_age_ms: 500, finalized_height: this.height });
      if (req.method() === "GET" && path === "/blocks") {
        const q = new URL(req.url()).searchParams;
        const limit = Math.min(Number(q.get("limit") ?? 20), 100);
        const top = Math.min(Number(q.get("before") ?? Infinity) - 1, this.height);
        const out = [];
        for (let h = top; h >= 1 && out.length < limit; h--) out.push(this.header(h));
        return reply(200, out);
      }
      if (req.method() === "GET" && (m = path.match(/^\/block\/(\d+)$/))) {
        const h = Number(m[1]);
        return h >= 1 && h <= this.height ? reply(200, this.block(h)) : reply(404, { error: "block not found" });
      }
      if (req.method() === "GET" && path === "/mempool") {
        const waiting = [...this.txs.values()].filter((t) => t.height == null).reverse();
        return reply(200, { count: waiting.length, txs: waiting.map((t) => this.txView(t)) });
      }
      if (req.method() === "GET" && (m = path.match(/^\/search\/(.+)$/))) {
        const q = m[1].trim().toLowerCase();
        const n = q.replace(/\.boo$/, "");
        if (/^\d+$/.test(q) && Number(q) >= 1 && Number(q) <= this.height) return reply(200, { type: "block", id: q });
        if (this.txs.has(q) || this.failures.has(q)) return reply(200, { type: "tx", id: q });
        if (/^t?boo1/.test(q)) return reply(200, { type: "account", id: q });
        if (/^[a-z0-9-]{3,32}$/.test(n)) return reply(200, this.names.has(n) ? { type: "account", id: `${n}.boo` } : { type: "name", id: n });
        return reply(404, { error: "nothing matching that not found" });
      }
      if (req.method() === "GET" && path === "/hunts") {
        const status = new URL(req.url()).searchParams.get("status");
        const list = this.hunts.filter((h) => !status || h.status === status);
        const open = this.hunts.filter((h) => h.status === "open").reduce((t, h) => t + BigInt(String(h.amount)), 0n);
        return reply(200, { count: this.hunts.length, open_bounties: open.toString(), open_bounties_display: formatFang(open), hunts: list });
      }
      if (req.method() === "GET" && (m = path.match(/^\/hunt\/(\d+)$/))) {
        const h = this.hunts.find((x) => x.id === Number(m?.[1]));
        return h ? reply(200, h) : reply(404, { error: "hunt not found" });
      }
      if (req.method() === "GET" && path === "/contributions") return this.contributions ? reply(200, this.contributions) : reply(404, { error: "no contribution pool" });
      if (req.method() === "GET" && path === "/claims") {
        const q = new URL(req.url()).searchParams;
        const list = this.claims.filter((c) => !q.get("status") || c.status === q.get("status")).slice(0, Number(q.get("limit") ?? 100));
        return reply(200, { count: this.claims.length, claims: list });
      }
      if (req.method() === "GET" && (m = path.match(/^\/claim\/(\d+)$/))) {
        const c = this.claims.find((x) => x.id === Number(m?.[1]));
        return c ? reply(200, c) : reply(404, { error: "claim not found" });
      }
      if (req.method() === "GET" && (m = path.match(/^\/seance\/(\d+)$/))) {
        const x = this.seances.find((y) => y.id === Number(m?.[1]));
        return x ? reply(200, x) : reply(404, { error: "Séance not found" });
      }
      if (req.method() === "GET" && path === "/souls") return reply(200, { souls: this.souls.slice(0, Number(new URL(req.url()).searchParams.get("limit") ?? 50)) });
      if (req.method() === "GET" && (m = path.match(/^\/multisig\/(.+)$/))) {
        const ms = this.multisigs.get(this.resolve(m[1]));
        return ms ? reply(200, ms) : reply(404, { error: "not a multisig account" });
      }
      if (req.method() === "GET" && this.governance && (m = path.match(/^\/proposal\/(\d+)$/))) {
        const p = this.proposals.find((x) => x.id === Number(m?.[1]));
        return p ? reply(200, { ...this.proposalView(p), votes: [], distribution: null }) : reply(404, { error: "proposal not found" });
      }
      if (req.method() === "POST" && path === "/faucet") {
        if (!this.faucet) throw new HttpError(400, "this chain has no faucet");
        const to = (JSON.parse(body ?? "{}") as { to: string }).to;
        // As node/src/types.rs: an address with another chain's prefix is refused.
        const prefix = String(this.statusExtra.address_prefix ?? "boo");
        const theirs = to.match(/^(t?boo)1/)?.[1];
        if (theirs && theirs !== prefix) throw new HttpError(400, `\`to\` is a ${theirs} address, but this chain's addresses start with ${prefix}1`);
        this.credit(to, "100");
        return reply(200, { id: this.record({ tx: null, kind: "transfer", from: FAUCET, to, amount: parseFang("100"), fee: 0n, memo: "faucet", summary: `faucet sent 100 FANG to ${this.label(to).short}` }) });
      }
      if (req.method() === "POST" && path === "/tx") return reply(200, { id: this.submit(JSON.parse(body ?? "{}") as Tx) });
      if (req.method() === "GET" && path === "/validators") return reply(200, this.validators());
      if (req.method() === "GET" && this.staking && (m = path.match(/^\/staking\/(.+)$/))) return reply(200, this.stakingOf(this.resolve(m[1])));
      if (req.method() === "GET" && this.governance && path.startsWith("/proposals")) return reply(200, { count: this.proposals.length, next_id: this.proposals.length, council_active: true, spending_open: false, params: { supermajority_bps: 6667, voting_blocks: 604_800, timelock_blocks: 172_800, lock_period_blocks: 604_800 }, proposals: this.proposals.map((p) => this.proposalView(p)) });
      if (req.method() === "GET" && path === "/params") return reply(200, { params: { unbonding_blocks: "1814400", lock_period_blocks: "604800", proposal_deposit: (1000n * 10n ** 9n).toString(), max_validators: "31" }, genesis: { block_time_ms: 1000, epoch_blocks: 3600, address_prefix: "tboo", epochs_per_year: 8766 }, issuance: { year: 1, rate_ppm: 60000 } });
      // The base fee at its genesis floor (node/src/api.rs `fees`): what Wallet.build prices at.
      if (req.method() === "GET" && path === "/fees") return reply(200, { height: this.height, lag: 17, rate_scale: 1000, slots: [], suggested: { flat: "10000", rate: "10000" }, floor: { flat: "10000", per_byte: "10" }, target_fullness_bps: 5000, max_change_bps: 1250, account_creation_fee: "100000000", account_creation_fee_display: "0.1 FANG" });
      return reply(404, { error: `mock node: no route for ${req.method()} ${path}` });
    } catch (e) {
      return reply(e instanceof HttpError ? e.status : 500, { error: e instanceof Error ? e.message : String(e) });
    }
  }
}
