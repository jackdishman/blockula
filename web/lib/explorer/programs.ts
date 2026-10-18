/**
 * The chain's built-in modules, described the way contracts will be: what state they expose,
 * which transactions call them, and the actions anyone can take, with typed parameters. One
 * generic form (components/explorer/ActionForm.tsx) renders any action, previews the exact
 * transaction, and signs it. A contract's ABI maps onto `ActionSpec` unchanged: typed params in,
 * an `Action` out. See docs/EXPLORER.md.
 */
import type { Action } from "@blockula/sdk";
import { BLOCKS_PER_DAY } from "../config";
import type { TermKey } from "./glossary";

/**
 * - `account`: a `.boo` name or an address, in a field the chain resolves itself (a payment's
 *   recipient, a hunt's winner, a vouch, a name offer). Signed the way `Client.recipient` says: an
 *   address as given, a name's proven owner with witnesses, else the name itself, so a lying node
 *   can make the transaction fail but not send it elsewhere (audit gap-7-1).
 * - `validator`: a field that needs an address (staking). A name is resolved, and on mainnet an
 *   unproven one isn't signed (`UnprovenNameError`).
 */
export type ParamType = "fang" | "account" | "validator" | "name" | "int" | "years" | "bps" | "text" | "repo" | "days" | "key_id";

export interface Param {
  key: string;
  label: string;
  type: ParamType;
  placeholder?: string;
  /** Prefilled value (as typed). */
  initial?: string;
  optional?: boolean;
  help?: string;
  /** For text: max bytes. */
  max?: number;
}

/** Checked inputs: fang → drops (string), account → as signed, validator → address, int/days/years/bps → number, others trimmed. */
export type Values = Record<string, string | number>;

export interface Ctx {
  /**
   * The height the transaction's `valid_until` is counted from: the same status read signs it
   * (`Wallet.build` with a function), so a node that lies about the height can stretch an expiry
   * by at most `max_tx_validity_blocks` (audit gap-7-3).
   */
  height: number;
  /** The account the transaction acts for. */
  account: string;
}

export interface ActionSpec {
  /** Tells apart actions of one kind (offering a name and accepting one are both `transfer_name`). Defaults to `kind`. */
  id?: string;
  /** The transaction kind it sends. */
  kind: Action["type"];
  title: string;
  blurb: string;
  /** Who the chain lets call it, in words (the chain enforces it, this only explains). */
  who: string;
  /** Session (agent) keys can only send transfers. */
  ownerOnly: boolean;
  params: Param[];
  build(v: Values, ctx: Ctx): Action;
  /** The preview's first line, from the inputs as typed. */
  describe(raw: Record<string, string>): string;
}

export interface Program {
  id: "names" | "hunts" | "pool" | "validators" | "accounts";
  title: string;
  tagline: string;
  term: TermKey;
  /** State it exposes, by node path. */
  reads: { path: string; what: string }[];
  /** Transaction kinds that belong to it. */
  calls: string[];
  actions: ActionSpec[];
  /** Calls that need special keys or tools, explained instead of offered. */
  elsewhere?: string;
}

export const actionId = (a: ActionSpec) => a.id ?? a.kind;

/** Days as blocks, at the chain's fixed block time (config.ts: never read from the node). */
const blocksOf = (days: number) => days * BLOCKS_PER_DAY;
const fang = (v: Values, k: string) => String(v[k]);
const bare = (n: string) => (n || "…").toLowerCase().replace(/\.boo$/, "");

export const PROGRAMS: Program[] = [
  {
    id: "accounts",
    title: "Accounts & keys",
    tagline: "Balances, transfers, and the keys allowed to sign for each account.",
    term: "owner_key",
    reads: [
      { path: "/api/account/{name|address}", what: "balance, what it can move (locks), nonce, names, keys, recent activity" },
      { path: "/api/key/{key_id}", what: "every account a key agreed to join" },
      { path: "/api/multisig/{name|address}", what: "a multisig account's threshold, owners and open proposals" },
    ],
    calls: ["transfer", "add_key", "remove_key", "register_familiar", "multisig_create", "multisig_propose", "multisig_approve"],
    actions: [
      {
        kind: "transfer",
        title: "Send FANG",
        blurb: "Pay a .boo name or an address. A name is signed as the name unless your network's witnesses prove its owner: the chain then pays whoever owns it when the transaction runs, so no node can send it elsewhere.",
        who: "any key on the account, agents within their limit",
        ownerOnly: false,
        params: [
          { key: "to", label: "To", type: "account", placeholder: "alice.boo or an address" },
          { key: "amount", label: "Amount", type: "fang", placeholder: "1.5" },
          { key: "memo", label: "Memo", type: "text", optional: true, max: 256, placeholder: "public, up to 256 bytes" },
        ],
        build: (v) => ({ type: "transfer", to: String(v.to), amount: fang(v, "amount") }),
        describe: (r) => `Send ${r.amount || "…"} FANG to ${r.to || "…"}`,
      },
      {
        kind: "register_familiar",
        title: "Register a Familiar",
        blurb: "Mark one of this account's agent keys as a Familiar, so its contributions credit you as its sponsor.",
        who: "owner keys",
        ownerOnly: true,
        params: [
          { key: "key_id", label: "Agent key id", type: "key_id", placeholder: "64 hex characters" },
          { key: "label", label: "Label", type: "text", initial: "claude", max: 64 },
        ],
        build: (v) => ({ type: "register_familiar", key_id: String(v.key_id), label: String(v.label) }),
        describe: (r) => `Register “${r.label || "…"}” as a Familiar`,
      },
    ],
    elsewhere:
      "Keys join an account only with their own consent, a grant code their holder makes for it, so adding and revoking keys has its own guided flow (grant codes, budgets, expiry) on Devices & agents. Multisig accounts are set up and approved with the CLI (`blockula multisig create`, `propose`, `approve`), which decodes each action before an approval binds it.",
  },
  {
    id: "names",
    title: "Names",
    tagline: "Human-readable .boo names that point to accounts, registered a year at a time.",
    term: "name",
    reads: [{ path: "/api/name/{name}", what: "status (active, grace, reserved, available), owner, expiry, a pending offer, prices" }],
    calls: ["register_name", "renew_name", "transfer_name", "release_name"],
    actions: [
      {
        kind: "register_name",
        title: "Register a name",
        blurb: "Claim a .boo name for this account for a year. Shorter names cost more because they're scarcer; the price goes to the treasury.",
        who: "owner keys",
        ownerOnly: true,
        params: [{ key: "name", label: "Name", type: "name", placeholder: "ghoul" }],
        build: (v) => ({ type: "register_name", name: String(v.name) }),
        describe: (r) => `Register ${bare(r.name)}.boo`,
      },
      {
        kind: "renew_name",
        title: "Renew a name",
        blurb: "Extend a name you own, a year at a time, at the chain's renewal price (paid to the treasury). An expired name can be renewed by its owner until its grace period ends; after that anyone can register it.",
        who: "the name's owner",
        ownerOnly: true,
        params: [
          { key: "name", label: "Name", type: "name", placeholder: "ghoul" },
          { key: "years", label: "Years", type: "years", initial: "1" },
        ],
        build: (v) => ({ type: "renew_name", name: String(v.name), years: Number(v.years) }),
        describe: (r) => `Renew ${bare(r.name)}.boo for ${r.years || "…"} year${r.years === "1" ? "" : "s"}`,
      },
      {
        kind: "transfer_name",
        title: "Offer a name",
        blurb: "Offer a name you own to another account. It stays yours until they accept it, so nobody is given a name without saying yes. Offering it again replaces the offer.",
        who: "the name's owner",
        ownerOnly: true,
        params: [
          { key: "name", label: "Name", type: "name", placeholder: "ghoul" },
          { key: "to", label: "To", type: "account", placeholder: "their .boo name or address" },
        ],
        build: (v) => ({ type: "transfer_name", name: String(v.name), to: String(v.to) }),
        describe: (r) => `Offer ${bare(r.name)}.boo to ${r.to || "…"} (theirs once they accept)`,
      },
      {
        id: "accept_name",
        kind: "transfer_name",
        title: "Accept a name",
        blurb: "Accept a name offered to this account: it becomes yours.",
        who: "the account it's offered to",
        ownerOnly: true,
        params: [{ key: "name", label: "Name", type: "name", placeholder: "ghoul" }],
        build: (v, ctx) => ({ type: "transfer_name", name: String(v.name), to: ctx.account }),
        describe: (r) => `Accept ${bare(r.name)}.boo`,
      },
      {
        id: "withdraw_name_offer",
        kind: "transfer_name",
        title: "Withdraw an offer",
        blurb: "Take back your offer of a name you own (the same transaction as accepting it: the chain tells them apart by who owns it).",
        who: "the name's owner",
        ownerOnly: true,
        params: [{ key: "name", label: "Name", type: "name", placeholder: "ghoul" }],
        build: (v, ctx) => ({ type: "transfer_name", name: String(v.name), to: ctx.account }),
        describe: (r) => `Withdraw your offer of ${bare(r.name)}.boo`,
      },
      {
        kind: "release_name",
        title: "Release a name",
        blurb: "Give up a name you own. Anyone can register it afterwards, and nothing is refunded.",
        who: "the name's owner",
        ownerOnly: true,
        params: [{ key: "name", label: "Name", type: "name", placeholder: "ghoul" }],
        build: (v) => ({ type: "release_name", name: String(v.name) }),
        describe: (r) => `Release ${bare(r.name)}.boo for anyone to register`,
      },
    ],
  },
  {
    id: "hunts",
    title: "Hunts",
    tagline: "Bounties on GitHub issues, held in escrow until the creator awards one for a merged PR.",
    term: "hunt",
    reads: [
      { path: "/api/hunts?status=", what: "every hunt, biggest first" },
      { path: "/api/hunt/{id}", what: "one hunt: funders, winner, PR" },
    ],
    calls: ["create_hunt", "fund_hunt", "award_hunt", "cancel_hunt"],
    actions: [
      {
        kind: "create_hunt",
        title: "Put up a hunt",
        blurb: "Escrow a bounty on an issue. You award it for a merged PR. Once it expires you can cancel it, refunding every funder exactly what they put in; before that, only a governance refund (RefundHunt) can.",
        who: "owner keys",
        ownerOnly: true,
        params: [
          { key: "repo", label: "Repository", type: "repo", placeholder: "owner/name" },
          { key: "issue", label: "Issue number", type: "int", placeholder: "12" },
          { key: "title", label: "Title", type: "text", max: 96, placeholder: "What needs doing" },
          { key: "amount", label: "Bounty", type: "fang", placeholder: "at least 10" },
          { key: "days", label: "Expires after", type: "days", initial: "30", help: "Until then the escrow is held: you can award it, but not cancel it. Afterwards you can still award it, or cancel to refund the funders." },
        ],
        build: (v, ctx) => ({ type: "create_hunt", repo: String(v.repo), issue: Number(v.issue), amount: fang(v, "amount"), expires_at: ctx.height + blocksOf(Number(v.days)), title: String(v.title) }),
        describe: (r) => `Put ${r.amount || "…"} FANG on ${r.repo || "…"}#${r.issue || "…"}, for ${r.days || "…"} days`,
      },
      {
        kind: "fund_hunt",
        title: "Add to a bounty",
        blurb: "Grow an open hunt's bounty. It's held until the creator awards it; if the creator cancels it after it expires (or governance refunds it), you get back exactly what you put in.",
        who: "owner keys",
        ownerOnly: true,
        params: [
          { key: "hunt_id", label: "Hunt", type: "int", placeholder: "hunt id" },
          { key: "amount", label: "Amount", type: "fang", placeholder: "at least 1" },
        ],
        build: (v) => ({ type: "fund_hunt", hunt_id: Number(v.hunt_id), amount: fang(v, "amount") }),
        describe: (r) => `Add ${r.amount || "…"} FANG to hunt #${r.hunt_id || "…"}`,
      },
      {
        kind: "award_hunt",
        title: "Award a hunt",
        blurb: "Pay the bounty to whoever's PR fixed it, before or after it expires. Only the creator can, and the whole escrow goes to the winner.",
        who: "the hunt's creator",
        ownerOnly: true,
        params: [
          { key: "hunt_id", label: "Hunt", type: "int" },
          { key: "to", label: "Winner", type: "account", placeholder: "their .boo name or address" },
          { key: "pr", label: "Pull request number", type: "int" },
        ],
        build: (v) => ({ type: "award_hunt", hunt_id: Number(v.hunt_id), to: String(v.to), pr: Number(v.pr) }),
        describe: (r) => `Award hunt #${r.hunt_id || "…"} to ${r.to || "…"} for PR #${r.pr || "…"}`,
      },
      {
        kind: "cancel_hunt",
        title: "Cancel a hunt",
        blurb: "After it expires, close it and refund every funder exactly what they put in. Before then the chain refuses (a governance RefundHunt can still refund it).",
        who: "the hunt's creator, after it expires (admins of dev and test chains any time)",
        ownerOnly: true,
        params: [{ key: "hunt_id", label: "Hunt", type: "int" }],
        build: (v) => ({ type: "cancel_hunt", hunt_id: Number(v.hunt_id) }),
        describe: (r) => `Cancel hunt #${r.hunt_id || "…"} and refund its funders`,
      },
    ],
  },
  {
    id: "pool",
    title: "Contribution pool",
    tagline: "Pays for merged PRs, judged by Séances of human jurors with Soul.",
    term: "pool",
    reads: [
      { path: "/api/contributions", what: "pool balance, rules, the open Séance" },
      { path: "/api/claims?status=&account=", what: "claims, newest first" },
      { path: "/api/seance/{id}", what: "a jury: pairs, commitments, reveals, results" },
      { path: "/api/souls", what: "the Soul leaderboard" },
    ],
    calls: ["fund_pool", "link_github", "submit_claim", "vouch", "unvouch", "join_jury", "leave_jury", "commit_votes", "reveal_votes", "withdraw_vested", "attest_claim", "attest_revert"],
    actions: [
      {
        kind: "fund_pool",
        title: "Fund the pool",
        blurb: "Donate FANG to reward useful work. Each Séance pays out up to a quarter of the pool.",
        who: "owner keys",
        ownerOnly: true,
        params: [{ key: "amount", label: "Amount", type: "fang" }],
        build: (v) => ({ type: "fund_pool", amount: fang(v, "amount") }),
        describe: (r) => `Give ${r.amount || "…"} FANG to the contribution pool`,
      },
      {
        kind: "link_github",
        title: "Link GitHub",
        blurb: "Claim a GitHub handle. Verify it with a public gist, by that user, containing this account's address.",
        who: "owner keys",
        ownerOnly: true,
        params: [
          { key: "handle", label: "GitHub handle", type: "text", max: 39, placeholder: "octocat" },
          { key: "gist_id", label: "Gist id", type: "text", max: 64, placeholder: "the gist's id" },
        ],
        build: (v) => ({ type: "link_github", handle: String(v.handle).replace(/^@/, ""), gist_id: String(v.gist_id) }),
        describe: (r) => `Link GitHub @${(r.handle || "…").replace(/^@/, "")}`,
      },
      {
        kind: "submit_claim",
        title: "Claim a merged PR",
        blurb: "Submit a PR you authored (with your linked handle) to a pool repo. A refundable bond is held until it's judged.",
        who: "owner keys or a registered Familiar",
        ownerOnly: false,
        params: [
          { key: "repo", label: "Repository", type: "repo", placeholder: "jackdishman/blockula" },
          { key: "pr", label: "PR number", type: "int" },
        ],
        build: (v) => ({ type: "submit_claim", repo: String(v.repo), pr: Number(v.pr) }),
        describe: (r) => `Claim ${r.repo || "…"}#${r.pr || "…"} for the pool`,
      },
      {
        kind: "vouch",
        title: "Vouch for a newcomer",
        blurb: "Stake 10 FANG and your Soul on someone being a real, distinct person. You lose both if they're a sybil.",
        who: "accounts with 10+ Soul",
        ownerOnly: true,
        params: [{ key: "account", label: "Newcomer", type: "account", placeholder: "their .boo name or address" }],
        build: (v) => ({ type: "vouch", account: String(v.account) }),
        describe: (r) => `Vouch for ${r.account || "…"}`,
      },
      {
        kind: "join_jury",
        title: "Join the jury pool",
        blurb: "Sit on Séances. Jurors who agree with the outcome earn Soul and a share of the payout; no-shows lose part of their bond.",
        who: "owner keys with 5+ Soul (humans only)",
        ownerOnly: true,
        params: [{ key: "bond", label: "Bond", type: "fang", initial: "10" }],
        build: (v) => ({ type: "join_jury", bond: fang(v, "bond") }),
        describe: (r) => `Join the jury pool with a ${r.bond || "…"} FANG bond`,
      },
      {
        kind: "withdraw_vested",
        title: "Withdraw vested rewards",
        blurb: "Move every reward that has finished vesting into your balance.",
        who: "owner keys",
        ownerOnly: true,
        params: [],
        build: () => ({ type: "withdraw_vested" }),
        describe: () => "Withdraw your vested contribution rewards",
      },
    ],
    elsewhere: "Sealing and revealing Séance votes keeps a secret salt between the two steps, and a sealed judgment binds the pairs it was made on, so jurors use the CLI: `blockula contrib vote`, then `blockula contrib reveal`.",
  },
  {
    id: "validators",
    title: "Validators",
    tagline: "Who proposes and finalizes blocks, chosen by stake epoch by epoch.",
    term: "proposer",
    reads: [
      { path: "/api/validators", what: "the active set, who joins and leaves at the next boundary, candidates by stake, jailed and tombstoned" },
      { path: "/api/staking/{name|address}", what: "an account's delegations, unbonding entries and when they're free" },
      { path: "/api/epochs", what: "validator-set history and handoff signatures" },
    ],
    calls: ["register_validator", "delegate", "undelegate", "redelegate", "withdraw_unbonded", "update_validator", "unjail", "report_equivocation", "beacon_commit", "beacon_reveal", "validator_ready", "admit_validator", "approve_validator", "remove_validator"],
    actions: [
      {
        kind: "delegate",
        title: "Stake with a validator",
        blurb: "Delegate FANG to a validator's pool. It earns a share of issuance and is slashed with the pool; taking it out takes the unbonding period (21 days on mainnet).",
        who: "owner keys",
        ownerOnly: true,
        params: [
          { key: "validator", label: "Validator", type: "validator", placeholder: "its address (or a .boo name its account holds)" },
          { key: "amount", label: "Amount", type: "fang" },
        ],
        build: (v) => ({ type: "delegate", validator: String(v.validator), amount: fang(v, "amount") }),
        describe: (r) => `Stake ${r.amount || "…"} FANG with ${r.validator || "…"}`,
      },
      {
        kind: "undelegate",
        title: "Unstake",
        blurb: "Start unbonding an amount of your stake in a pool (a validator's owner unbonds its self-bond this way too). It's signed as an amount, and the chain turns it into pool shares at its own price when it runs, so no node's report of your stake decides how much unbonds. It stops earning now; withdraw it once the unbonding period is over. To unstake all of it, use the Stake page (/stake).",
        who: "owner keys of the staking account",
        ownerOnly: true,
        params: [
          { key: "validator", label: "Validator", type: "validator", placeholder: "its address" },
          { key: "amount", label: "Amount", type: "fang" },
        ],
        build: (v) => ({ type: "undelegate", validator: String(v.validator), shares: "0", amount: fang(v, "amount") }),
        describe: (r) => `Start unbonding ${r.amount || "…"} FANG of your stake with ${r.validator || "…"}`,
      },
      {
        kind: "withdraw_unbonded",
        title: "Withdraw unbonded stake",
        blurb: "Pay out every unbonding entry that has finished its unbonding period.",
        who: "owner keys",
        ownerOnly: true,
        params: [],
        build: () => ({ type: "withdraw_unbonded" }),
        describe: () => "Withdraw the stake that has finished unbonding",
      },
      {
        kind: "unjail",
        title: "Unjail a validator",
        blurb: "Bring a jailed validator you own back into contention, after its jail period.",
        who: "the validator's owner",
        ownerOnly: true,
        params: [{ key: "validator", label: "Validator", type: "validator", placeholder: "its address" }],
        build: (v) => ({ type: "unjail", validator: String(v.validator) }),
        describe: (r) => `Unjail ${r.validator || "…"}`,
      },
      {
        kind: "update_validator",
        title: "Set commission",
        blurb: "Set the share of your validator's rewards it keeps before the rest goes to its stakers (it moves a little per epoch at most).",
        who: "the validator's owner",
        ownerOnly: true,
        params: [{ key: "commission_bps", label: "Commission (basis points)", type: "bps", placeholder: "500 = 5%" }],
        build: (v) => ({ type: "update_validator", commission_bps: Number(v.commission_bps), p2p_addr: "", http_addr: "" }),
        describe: (r) => `Set your validator's commission to ${r.commission_bps ? `${Number(r.commission_bps) / 100}%` : "…"}`,
      },
    ],
    elsewhere:
      "Registering needs the validator node's own consensus key and a proof of possession, so it starts on the node: `blockula validator init`. Validator nodes sign their own readiness, admissions, beacon rounds and equivocation reports. Redelegating moves pool shares (`blockula stake redelegate`). Your pools, and unstaking all of a stake, are on Stake.",
  },
];

export const program = (id: string) => PROGRAMS.find((p) => p.id === id);

/** `fund_hunt(hunt_id: int, amount: fang)`: an action's signature, as an interface listing shows it. */
export const signature = (a: ActionSpec) => `${a.kind}(${a.params.map((p) => `${p.key}${p.optional ? "?" : ""}: ${p.type}`).join(", ")})`;
