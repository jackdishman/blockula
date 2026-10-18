// What the explorer reads from the node (node/src/api.rs). Fields the node may add or rename in
// later protocol versions are optional, and every page reads them tolerantly.
import type { KeyView, Label } from "../types";

export type { Label };

export interface ChainStatus {
  chain_id: string;
  token: string;
  decimals: number;
  height: number;
  block_time_ms: number;
  finalized_height?: number;
  syncing?: boolean;
  sync_progress?: number;
  network_height_claimed?: number;
  epoch?: number;
  active_validators?: number;
  validators?: Label[];
  faucet: boolean;
  supply: string;
  supply_display: string;
  burned: string;
  burned_display: string;
  accounts?: number;
  names?: number;
  mempool?: number;
  version?: string;
  git_sha?: string;
  genesis_hash?: string;
  head_age_ms?: number | null;
  history_from?: number;
  fast_synced_from?: number | null;
  consensus?: string;
  crypto?: Record<string, string>;
}

export interface BlockSummary {
  height: number;
  hash: string;
  prev_hash: string;
  timestamp_ms: number;
  proposer: Label;
  state_root: string;
  tx_count: number;
  summary: string;
}

export interface BlockView extends BlockSummary {
  txs: TxDetail[];
  signature_bytes: number;
  failed_count?: number;
}

export type TxStatus = "pending" | "finalized" | "failed";

/**
 * A transaction as `/api/tx/{id}` serves it. A failure the node remembers without its transaction
 * has only id, status, `dropped`, error, height and summary: `dropped` true means it was dropped
 * from the mempool without running (nothing charged; `height` is when, not a block holding it),
 * false that it ran in a block this node no longer holds (its fee was charged).
 */
export interface TxDetail {
  id: string;
  summary: string;
  kind?: string;
  status: TxStatus;
  /** On a failure answered without the tx: true if it was dropped unrun (nothing charged), false if it ran in a block (its fee was charged). */
  dropped?: boolean;
  error?: string | null;
  height: number | null;
  from?: Label;
  /** Who the action named; for a sealed transaction, as the chain resolved it when it ran. */
  to?: Label | null;
  /** The `.boo` name it was signed to, when it was signed to a name (protocol v1 nodes report it, null otherwise). */
  to_name?: string | null;
  valid_until?: number;
  amount?: string | null;
  amount_display?: string | null;
  fee?: string;
  fee_display?: string;
  /** Protocol v1: the most it allowed, and the part of `fee` that tipped the proposer (the rest burned). */
  max_fee?: string;
  tip?: string;
  size_bytes?: number;
  nonce?: number;
  signer_key?: string | null;
  memo?: string;
  action?: { type: string; [k: string]: unknown };
  signature_scheme?: string;
}

export interface HuntView {
  id: number;
  repo: string;
  issue: number;
  issue_url: string;
  title: string;
  amount: string;
  amount_display: string;
  status: "open" | "awarded" | "cancelled" | "expired" | string;
  creator: Label;
  created_at: number;
  expires_at: number;
  funders: { account: Label; amount: string; amount_display: string }[];
  awarded_to: Label | null;
  awarded_by: Label | null;
  awarded_at: number | null;
  cancelled_at: number | null;
  pr: number | null;
  pr_url: string | null;
  familiar: string | null;
  self_awarded: boolean;
}

export interface HuntsView {
  count: number;
  hunts: HuntView[];
  open_bounties?: string;
  open_bounties_display?: string;
}

export interface ClaimView {
  id: number;
  repo: string;
  pr: number;
  pr_url: string;
  handle: string;
  claimant: Label;
  familiar: string | null;
  status: string;
  reason?: string | null;
  submitted_at: number;
  bond: string;
  bond_display: string;
  verified_at?: number | null;
  merge_sha?: string | null;
  eligible_at?: number | null;
  expires_at?: number | null;
  seance?: number | null;
  score_display?: string | null;
  payout: string;
  payout_display: string;
  soul_awarded_display?: string;
  attestations?: { validator: Label; ok: boolean; merge_sha: string; note?: string; at: number }[];
}

export interface SeanceJuror {
  juror: Label;
  pairs: number;
  committed: boolean;
  revealed: boolean;
  agreement_display?: string | null;
  reward_display: string;
  soul_delta_display: string;
}

export interface SeanceView {
  id: number;
  phase: "sealing" | "revealing" | "closed" | string;
  opened_at: number;
  commit_until: number;
  reveal_until: number;
  budget_display: string;
  seed: string;
  claims: ClaimView[];
  jurors: SeanceJuror[];
  results?: { accepted: number; below_threshold: number; spam: number; carried_over: number; no_shows: number } | null;
  results_display?: { paid: string; returned: string } | null;
}

export interface ContributionsView {
  active: boolean;
  pool: { account: Label; balance: string; balance_display: string; next_budget_display: string };
  repos: { repo: string; weight: number }[];
  rules: Record<string, string | number>;
  jurors: { active: number; pool: number };
  open_seance: (SeanceView & { claims: ClaimView[] }) | null;
  next_seance_at: number;
  recent_seances: { id: number; claims: number; jurors: number; budget_display: string; results?: { accepted: number; spam: number } | null }[];
}

export interface SoulRow {
  account: Label;
  github?: string | null;
  juror?: boolean;
  soul: { display: string; points: number };
}

export interface ContributionProfile {
  soul: { points: number; display: string; earned_display: string };
  claims: ClaimView[];
  juror: { active: boolean; bond_display: string; served: number } | null;
  vouched_by: { voucher: Label; at: number } | null;
  vouching_for: { account: Label }[];
  rewards: { total_display: string; withdrawable: string; withdrawable_display: string; unvested_display: string };
  sybil: { by: Label; reason: string } | null;
  /** The bond the account's next claim will hold (it grows with open claims). */
  next_claim_bond?: string;
}

/** A lock on an account's FANG (protocol v1): what's still vesting, or a vote's conviction lock. */
export interface LockView {
  id: string;
  reason: string;
  amount: string;
  amount_display: string;
  /** The height it ends (vesting) or lifts (a vote lock). */
  until: number;
  vesting: boolean;
}

export interface AccountDetail {
  account: Label;
  balance: string;
  balance_display: string;
  /** Protocol v1: what it can move out now (locked FANG net of stake can't leave), its locks. */
  transferable?: string;
  transferable_display?: string;
  locked?: string;
  locked_display?: string;
  locks?: LockView[];
  /** Whether the chain has an account here yet, and what a payment that opens it burns from the payer. */
  exists?: boolean;
  creation_fee?: string;
  nonce: number;
  next_nonce: number;
  names: string[];
  keys: KeyView[];
  recent_txs: TxDetail[];
  github: { handle: string; gist_id: string; linked_at: number } | null;
  familiars: { key_id: string; label: string }[];
  hunts_created: number[];
  hunts_won: { id: number; repo: string; issue: number; amount_display: string; pr: number; self_awarded: boolean }[];
  contribution: ContributionProfile | null;
}

/**
 * `/api/name/{name}` (DEC-11): `status` is `active`; `grace` (expired: it resolves to nobody, and
 * only its owner can renew it until `grace_until`); `reserved` (held at genesis, can't be
 * registered); or `available`. `offered_to`: the account it's offered to, theirs once they accept.
 */
export interface NameDetail {
  name: string;
  available: boolean;
  status?: "active" | "grace" | "reserved" | "available" | string;
  price?: string;
  price_display?: string;
  owner?: Label;
  registered_at?: number;
  expires_at?: number | null;
  grace_until?: number | null;
  offered_to?: Label | null;
  renewal_price?: string;
  renewal_price_display?: string;
  registration_blocks?: number;
  grace_blocks?: number;
}

/**
 * One validator. Today's chain has a bond and an admin's approval; staking chains (protocol v1)
 * add stake, self-bond, commission, liveness and jail. Both are read here.
 */
export interface ValidatorRow {
  validator: Label;
  owner: Label;
  status: string;
  p2p_addr?: string;
  genesis?: boolean;
  bond?: string;
  bond_display?: string;
  stake_display?: string;
  self_bond_display?: string;
  commission_display?: string;
  registered_at?: number;
  approved_by?: Label | null;
  approved_at?: number | null;
  removed_by?: Label | null;
  removed_at?: number | null;
  active_from_epoch?: number | null;
  active_until_epoch?: number | null;
  jailed?: { at: number; until: number; reason: string } | null;
  tombstoned_at?: number | null;
  liveness?: { recorded: number; missed: number } | null;
}

export interface ValidatorsDetail {
  epoch: number;
  epoch_blocks?: number;
  blocks_until_next_epoch?: number;
  max_validators?: number;
  min_bond_display?: string;
  admins?: Label[];
  active: ValidatorRow[];
  next_epoch?: ValidatorRow[];
  pending?: ValidatorRow[];
  approved?: ValidatorRow[];
  candidates?: ValidatorRow[];
  joining?: ValidatorRow[];
  /** Protocol v1: active now, dropping out of the set at the next boundary. */
  leaving?: ValidatorRow[];
  jailed?: ValidatorRow[];
  /** Protocol v1: slashed out for double-signing, for good. */
  tombstoned?: ValidatorRow[];
  removed?: ValidatorRow[];
  /** Protocol v1: while the set carries one or two members, a newcomer joins only once a quorum of them admits it. */
  admission?: { members: Label[]; quorum: number; ready_from: number } | null;
  unbonding_blocks?: number;
  explainer?: string;
}

/** `/api/multisig/{account}`: a multisig account's threshold, owner keys and open proposals. */
export interface MultisigView {
  account: Label;
  threshold: number;
  owners: { key_id: string; label: string | null }[];
  created_at: number;
  pending: { id: number; action: { type: string; [k: string]: unknown }; summary: string; approvals: string[]; valid_approvals: number; expires_at: number; expired: boolean }[];
}

export interface MempoolView {
  count: number;
  txs: TxDetail[];
}

export interface SearchHit {
  type: "block" | "tx" | "account" | "name" | string;
  id: string;
}
