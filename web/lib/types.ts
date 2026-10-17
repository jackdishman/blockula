// Shapes returned by the node's HTTP API (node/src/api.rs). The SDK's Client returns `any`.

export interface Label {
  address: string;
  name: string | null;
  short: string;
}

export interface Status {
  chain_id: string;
  token: string;
  decimals: number;
  height: number;
  block_time_ms: number;
  validator: Label;
  faucet: boolean;
  supply_display: string;
  burned_display: string;
  genesis_hash?: string;
  /** bech32m address prefix, on nodes that report it (protocol v1). */
  address_prefix?: string;
  prefix?: string;
}

export interface TxView {
  id: string;
  summary: string;
  kind: "transfer" | "register_name" | "add_key" | "remove_key";
  status: "pending" | "finalized";
  height: number | null;
  from: Label;
  to: Label | null;
  amount: string | null;
  amount_display: string | null;
  fee: string;
  fee_display: string;
  memo: string;
}

export type Role =
  | { type: "owner" }
  | { type: "session"; limit: string; period_blocks: number; expires_at: number };

export interface KeyView {
  key_id: string;
  label: string;
  role: Role;
  added_at: number;
  spent_this_period: string;
  window_start: number;
}

export interface AccountView {
  account: Label;
  balance: string;
  balance_display: string;
  nonce: number;
  next_nonce: number;
  names: string[];
  keys: KeyView[];
  recent_txs: TxView[];
}

export interface NameView {
  name: string;
  available: boolean;
  price?: string;
  price_display?: string;
  owner?: Label;
}

export interface KeyAccounts {
  key_id: string;
  accounts: { account: Label; role: Role; label: string }[];
  /** The next page's `after` (`null` at the end). */
  next?: string | null;
}

/** A validator in `/api/validators` (active, next_epoch, candidates, …), as node/src/api.rs serves it. */
export interface ValidatorView {
  validator: Label;
  owner: Label;
  /** `candidate`, `active`, `removed` or `tombstoned`. */
  status: string;
  /** The pool's total stake and the owner's self-bond, in drops. */
  stake: string;
  stake_display: string;
  self_bond: string;
  self_bond_display: string;
  commission_bps: number;
  commission_display: string;
  /** Selectable at the next boundary (registered, bonded enough, not jailed or tombstoned). */
  eligible: boolean;
  jailed: { at: number; until: number; reason: string } | null;
  tombstoned_at: number | null;
}

export interface ValidatorsView {
  epoch: number;
  active: ValidatorView[];
  next_epoch: ValidatorView[];
  candidates: ValidatorView[];
  jailed: ValidatorView[];
  max_validators: number;
}
