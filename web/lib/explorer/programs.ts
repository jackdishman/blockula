/**
 * The chain's built-in modules, described the way contracts will be: what state they expose,
 * which transactions call them, and the actions anyone can take, with typed parameters. One
 * generic form (components/explorer/ActionForm.tsx) renders any action, previews the exact
 * transaction, and signs it. A contract's ABI maps onto `ActionSpec` unchanged: typed params in,
 * an `Action` out. See docs/EXPLORER.md.
 */
import type { Action } from "@blockula/sdk";
import type { TermKey } from "./glossary";

export type ParamType = "fang" | "account" | "name" | "int" | "text" | "repo" | "days" | "key_id";

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

/** Checked inputs: fang → drops (string), account → address, int/days → number, others trimmed. */
export type Values = Record<string, string | number>;

export interface Ctx {
  height: number;
  /** The chain's block time, to turn days into blocks. */
  blockMs: number;
}

export interface ActionSpec {
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

const blocksOf = (days: number, blockMs: number) => Math.round((days * 86_400_000) / blockMs);
const fang = (v: Values, k: string) => String(v[k]);

export const PROGRAMS: Program[] = [
  {
    id: "accounts",
    title: "Accounts & keys",
    tagline: "Balances, transfers, and the keys allowed to sign for each account.",
    term: "owner_key",
    reads: [
      { path: "/api/account/{name|address}", what: "balance, nonce, names, keys, recent activity" },
      { path: "/api/key/{key_id}", what: "every account a key may sign for" },
    ],
    calls: ["transfer", "add_key", "remove_key", "register_familiar"],
    actions: [
      {
        kind: "transfer",
        title: "Send FANG",
        blurb: "Pay a .boo name or a boo1 address. The name is resolved now and the address is what you sign.",
        who: "any key on the account, agents within their limit",
        ownerOnly: false,
        params: [
          { key: "to", label: "To", type: "account", placeholder: "alice.boo or boo1…" },
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
    elsewhere: "Adding and revoking keys has its own guided flow (check codes, budgets, expiry) on Devices & agents.",
  },
  {
    id: "names",
    title: "Names",
    tagline: "Human-readable .boo names that point to accounts.",
    term: "name",
    reads: [{ path: "/api/name/{name}", what: "owner, or availability and price" }],
    calls: ["register_name"],
    actions: [
      {
        kind: "register_name",
        title: "Register a name",
        blurb: "Claim a .boo name for this account. Shorter names cost more because they're scarcer; the price goes to the treasury.",
        who: "owner keys",
        ownerOnly: true,
        params: [{ key: "name", label: "Name", type: "name", placeholder: "ghoul" }],
        build: (v) => ({ type: "register_name", name: String(v.name) }),
        describe: (r) => `Register ${(r.name || "…").replace(/\.boo$/, "")}.boo`,
      },
    ],
  },
  {
    id: "hunts",
    title: "Hunts",
    tagline: "Bounties on GitHub issues, held in escrow until a merged PR wins them.",
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
        blurb: "Escrow a bounty on an issue. It pays out when you award it for a merged PR, or refunds every funder if cancelled or expired.",
        who: "owner keys",
        ownerOnly: true,
        params: [
          { key: "repo", label: "Repository", type: "repo", placeholder: "owner/name" },
          { key: "issue", label: "Issue number", type: "int", placeholder: "12" },
          { key: "title", label: "Title", type: "text", max: 96, placeholder: "What needs doing" },
          { key: "amount", label: "Bounty", type: "fang", placeholder: "at least 10" },
          { key: "days", label: "Expires after", type: "days", initial: "30", help: "Refunded to funders if nobody wins it by then." },
        ],
        build: (v, ctx) => ({ type: "create_hunt", repo: String(v.repo), issue: Number(v.issue), amount: fang(v, "amount"), expires_at: ctx.height + blocksOf(Number(v.days), ctx.blockMs), title: String(v.title) }),
        describe: (r) => `Put ${r.amount || "…"} FANG on ${r.repo || "…"}#${r.issue || "…"}`,
      },
      {
        kind: "fund_hunt",
        title: "Add to a bounty",
        blurb: "Grow an open hunt's bounty. If it's cancelled or expires, you get back exactly what you put in.",
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
        blurb: "Pay the bounty to whoever's PR fixed it. The PR must be merged into the repo's default branch.",
        who: "the hunt's creator (or a Season 0 admin)",
        ownerOnly: true,
        params: [
          { key: "hunt_id", label: "Hunt", type: "int" },
          { key: "to", label: "Winner", type: "account", placeholder: "their .boo name" },
          { key: "pr", label: "Pull request number", type: "int" },
        ],
        build: (v) => ({ type: "award_hunt", hunt_id: Number(v.hunt_id), to: String(v.to), pr: Number(v.pr) }),
        describe: (r) => `Award hunt #${r.hunt_id || "…"} to ${r.to || "…"} for PR #${r.pr || "…"}`,
      },
      {
        kind: "cancel_hunt",
        title: "Cancel a hunt",
        blurb: "Close it and refund every funder what they put in.",
        who: "the hunt's creator (or a Season 0 admin)",
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
    calls: ["fund_pool", "link_github", "submit_claim", "vouch", "unvouch", "join_jury", "leave_jury", "commit_votes", "reveal_votes", "withdraw_vested"],
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
        params: [{ key: "account", label: "Newcomer", type: "account" }],
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
    elsewhere: "Sealing and revealing Séance votes keeps a secret salt between the two steps, so jurors use the CLI: `blockula contrib vote`, then `blockula contrib reveal`.",
  },
  {
    id: "validators",
    title: "Validators",
    tagline: "Who proposes and finalizes blocks, epoch by epoch.",
    term: "proposer",
    reads: [
      { path: "/api/validators", what: "the active set, next epoch, pending, removed" },
      { path: "/api/epochs", what: "validator-set history and handoff signatures" },
    ],
    calls: ["register_validator", "approve_validator", "remove_validator", "unbond"],
    actions: [
      {
        kind: "unbond",
        title: "Reclaim a bond",
        blurb: "Get back the bond of a validator you own, one epoch after it left the set.",
        who: "the validator's owner",
        ownerOnly: true,
        params: [{ key: "validator", label: "Validator address", type: "account" }],
        build: (v) => ({ type: "unbond", validator: String(v.validator) }),
        describe: (r) => `Reclaim the bond of ${r.validator || "…"}`,
      },
    ],
    elsewhere: "Registering needs the validator node's own consensus key and a proof of possession, so it starts on the node: `blockula validator init`.",
  },
];

export const program = (id: string) => PROGRAMS.find((p) => p.id === id);

/** `fund_hunt(hunt_id: int, amount: fang)`: an action's signature, as an interface listing shows it. */
export const signature = (a: ActionSpec) => `${a.kind}(${a.params.map((p) => `${p.key}${p.optional ? "?" : ""}: ${p.type}`).join(", ")})`;
