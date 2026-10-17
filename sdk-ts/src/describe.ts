// What a proposal kind or a multisig action does, in plain English, rendered by the client from
// the exact content a vote or an approval binds (audit gap-7-6). The node's own `summary` text
// isn't covered by any digest, so a lying node can rewrite it at will: show these instead.
//
// They never throw. Content a client can't read exactly (an unknown type, a number past 2^53 that
// JSON rounded, a malformed amount or account) is described as such, so a UI can say "don't vote
// on this" instead of rendering something it guessed.
import { bech32m } from "@scure/base";
import { DROPS_PER_FANG } from "./tx.js";

/** Options of `describeKind` / `describeAction`. */
export interface DescribeOptions {
  /**
   * Quote the text a stranger wrote (reasons, labels, titles, memos) inside the description.
   * Default true. Pass false where it must be shown apart, marked untrusted (an agent's tools do).
   */
  freeText?: boolean;
}

const obj = (v: unknown): Record<string, unknown> => (v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : {});

/** Whether `s` is a canonical address (any known prefix). Kept local so this module has no import cycle. */
function address(s: unknown): s is string {
  if (typeof s !== "string" || s !== s.toLowerCase()) return false;
  try {
    const { prefix, words } = bech32m.decode(s as `${string}1${string}`);
    return (prefix === "boo" || prefix === "tboo") && bech32m.fromWords(words).length === 32;
  } catch {
    return false;
  }
}

/** Groups a string of digits in thousands: 10000000 → 10,000,000. */
const grouped = (digits: string) => digits.replace(/\B(?=(\d{3})+(?!\d))/g, ",");

/** A JSON number the client can read exactly, or a decimal string, as a bigint; `undefined` otherwise. */
function whole(v: unknown): bigint | undefined {
  if (typeof v === "bigint") return v >= 0n ? v : undefined;
  if (typeof v === "number") return Number.isSafeInteger(v) && v >= 0 ? BigInt(v) : undefined;
  if (typeof v === "string" && /^\d{1,20}$/.test(v) && BigInt(v) < 1n << 64n) return BigInt(v);
  return undefined;
}

/** Why a number can't be shown exactly. */
const unreadable = (v: unknown) =>
  typeof v === "number" && Number.isFinite(v) && Number.isInteger(v) && v > Number.MAX_SAFE_INTEGER
    ? "a number too large for this client to read exactly (it may have been rounded)"
    : "an unreadable number";

/** An amount of drops, spelled out in FANG with every digit: "10,000,000 FANG", "0.5 FANG". */
export function fangText(drops: unknown): string {
  const d = whole(drops);
  if (d === undefined) return typeof drops === "string" || typeof drops === "number" ? `${unreadable(drops)} of FANG` : "an unreadable amount of FANG";
  const frac = (d % DROPS_PER_FANG).toString().padStart(9, "0").replace(/0+$/, "");
  return `${grouped((d / DROPS_PER_FANG).toString())}${frac ? "." + frac : ""} FANG`;
}

/** A whole number (ids, heights, counts), or what's wrong with it. */
function n(v: unknown): string {
  const d = whole(v);
  return d === undefined ? unreadable(v) : d.toString();
}

/** milli-Soul as Soul ("2.5 Soul"). */
function soul(v: unknown): string {
  const d = whole(v);
  if (d === undefined) return `${unreadable(v)} of Soul`;
  const frac = (d % 1000n).toString().padStart(3, "0").replace(/0+$/, "");
  return `${grouped((d / 1000n).toString())}${frac ? "." + frac : ""} Soul`;
}

const NAME = /^[a-z0-9]([a-z0-9-]{1,30}[a-z0-9])$/;

/**
 * An account field: an address in full (check it), or a `.boo` name, which the chain resolves to
 * its owner when the action runs.
 */
function account(v: unknown): string {
  if (address(v)) return v;
  if (typeof v === "string") {
    // Exactly as the chain reads it (node/src/types.rs `normalize_name`): ASCII, any case, no spaces.
    const bare = v.toLowerCase().replace(/\.boo$/, "");
    if (NAME.test(bare)) return `${bare}.boo (a name, not an address: the chain resolves it to whoever owns it when this runs)`;
  }
  return "an unreadable account";
}

/** A `.boo` name field (registered, renewed, offered, released). */
function nameField(v: unknown): string {
  if (typeof v !== "string") return "an unreadable name";
  const bare = v.toLowerCase().replace(/\.boo$/, "");
  return NAME.test(bare) ? `${bare}.boo` : "an unreadable name";
}

/** A plain token (parameter keys, feature names, repos, handles), or nothing it can show safely. */
function token(v: unknown, re: RegExp, what: string): string {
  return typeof v === "string" && re.test(v) ? v : `an unreadable ${what}`;
}

/** Hex of an exact length, shortened to its ends. */
function hex(v: unknown, bytes: number, what: string): string {
  return typeof v === "string" && new RegExp(`^[0-9a-f]{${bytes * 2}}$`).test(v) ? `${v.slice(0, 12)}…${v.slice(-8)}` : `an unreadable ${what}`;
}

/** Text a stranger wrote, quoted (one line, at most 200 characters), or left out. */
function quoted(v: unknown, label: string, opts: DescribeOptions): string {
  if (opts.freeText === false || typeof v !== "string" || !v.trim()) return "";
  // One line, nothing invisible, and no curly quotes of its own, so it can't close the quote early.
  const t = [...v.replace(/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]+/gu, " ").replace(/[“”]/g, '"').replace(/ {2,}/g, " ").trim()];
  return ` (${label}, as written: “${t.length > 200 ? t.slice(0, 200).join("") + "…" : t.join("")}”)`;
}

const REPO = /^[A-Za-z0-9-]{1,39}\/[A-Za-z0-9._-]{1,100}$/;
const HANDLE = /^[A-Za-z0-9-]{1,39}$/;
const KEY = /^[a-z0-9_]{1,64}$/;
const FEATURE = /^[a-z0-9_-]{1,64}$/;

/**
 * What a governance proposal's `kind` does, in plain English, from the kind itself (as stored on
 * chain: accounts are addresses there). Every amount is spelled out in FANG and every recipient
 * in full. Show this to a voter, not the node's `summary`: `proposalDigest(kind, title, url)` is
 * what the vote binds, and this is rendered from the same `kind`.
 */
export function describeKind(kind: unknown, opts: DescribeOptions = {}): string {
  const k = obj(kind);
  switch (k.type) {
    case "param_change":
      return `Set the chain parameter ${token(k.key, KEY, "parameter")} to ${n(k.value)} (in that parameter's own units: drops, blocks, basis points…)`;
    case "treasury_spend":
      return `Pay ${fangText(k.amount)} from the treasury to ${account(k.to)}`;
    case "distribution":
      return `Distribute ${fangText(k.total)} from the community account as distribution #${n(k.distribution_id)}, to the accounts in the Merkle tree with root ${hex(k.merkle_root, 32, "root")} (check it against the published tree), claimable until block #${n(k.claim_until)}`;
    case "set_repo":
      return whole(k.weight) === 0n ? `Remove the repo ${token(k.repo, REPO, "repo")} from the contribution pool` : `Put the repo ${token(k.repo, REPO, "repo")} in the contribution pool with weight ${n(k.weight)}`;
    case "grant_soul":
      return `Grant ${soul(k.amount)} to ${account(k.to)}${quoted(k.reason, "reason", opts)}`;
    case "flag_sybil":
      return `Flag ${account(k.account)} as a sybil (its voucher is slashed, its unvested rewards clawed back)${quoted(k.reason, "reason", opts)}`;
    case "unflag_sybil":
      return `Lift the sybil flag on ${account(k.account)} (nothing the flag took comes back)${quoted(k.reason, "reason", opts)}`;
    case "reject_claim":
      return `Reject contribution claim #${n(k.claim_id)}${quoted(k.reason, "reason", opts)}`;
    case "clawback":
      return `Claw back the unvested contribution rewards of ${account(k.account)}${quoted(k.reason, "reason", opts)}`;
    case "unlink_github":
      return `Unlink the GitHub handle @${token(k.handle, HANDLE, "handle")} and hold it for the proposer to link for one voting period`;
    case "refund_hunt":
      return `Refund every funder of hunt #${n(k.hunt_id)} what they put in`;
    case "schedule_upgrade":
      return `Switch the chain to protocol version ${n(k.version)} at block #${n(k.activation_height)} (nodes that can't run it stop there)`;
    case "cancel_upgrade":
      return `Cancel the scheduled upgrade to protocol version ${n(k.version)} at block #${n(k.activation_height)}, and any upgrade scheduled after it`;
    case "activate_feature":
      return `Turn on the feature ${token(k.feature, FEATURE, "feature")}`;
    case "deactivate_feature":
      return `Turn off the feature ${token(k.feature, FEATURE, "feature")}`;
    case "text":
      return "Nothing on chain: a text proposal (a signal; read its title and link)";
    default:
      return `Something this client can't read (a proposal of type ${typeof k.type === "string" ? token(k.type, /^[a-z_]{1,40}$/, "type") : "unknown"}): don't vote on it from here`;
  }
}

/** A key role (`add_key`). */
function role(r: unknown): string {
  const x = obj(r);
  if (x.type === "owner") return "an owner key (full control of the account)";
  if (x.type === "session") return `a session key that may send up to ${fangText(x.limit)} (fees included) every ${n(x.period_blocks)} blocks, until block #${n(x.expires_at)}, and nothing else`;
  return "an unreadable role";
}

/**
 * What a transaction action does (a multisig's pending action, before an owner approves it), in
 * plain English, from the action itself. Show this, not the node's summary:
 * `actionDigest(action)` is what the approval binds, and this is rendered from the same action.
 */
export function describeAction(action: unknown, opts: DescribeOptions = {}): string {
  const a = obj(action);
  switch (a.type) {
    case "transfer":
      return `Send ${fangText(a.amount)} to ${account(a.to)}`;
    case "register_name":
      return `Register the name ${nameField(a.name)}`;
    case "add_key":
      return `Add key ${hex(a.key_id, 32, "key id")} to the account as ${role(a.role)}${quoted(a.label, "label", opts)}`;
    case "remove_key":
      return `Remove key ${hex(a.key_id, 32, "key id")} from the account`;
    case "register_validator":
      return `Register a validator, bonding ${fangText(a.bond)} from the account`;
    case "approve_validator":
      return `Approve validator ${account(a.validator)} (admin chains only)`;
    case "remove_validator":
      return `Remove validator ${account(a.validator)} (admin chains only)`;
    case "unbond":
      return `Reclaim the bond of removed validator ${account(a.validator)}`;
    case "link_github":
      return `Link the GitHub handle @${token(a.handle, HANDLE, "handle")} to the account`;
    case "unlink_github":
      return `Unlink the GitHub handle @${token(a.handle, HANDLE, "handle")}`;
    case "create_hunt":
      return `Escrow ${fangText(a.amount)} as a bounty on ${token(a.repo, REPO, "repo")} issue #${n(a.issue)}, until block #${n(a.expires_at)}${quoted(a.title, "title", opts)}`;
    case "fund_hunt":
      return `Add ${fangText(a.amount)} to hunt #${n(a.hunt_id)}`;
    case "award_hunt":
      return `Pay hunt #${n(a.hunt_id)}'s bounty to ${account(a.to)} for PR #${n(a.pr)}`;
    case "cancel_hunt":
      return `Cancel hunt #${n(a.hunt_id)} and refund its funders`;
    case "register_familiar":
      return `Register key ${hex(a.key_id, 32, "key id")} as a Familiar${quoted(a.label, "label", opts)}`;
    case "set_repo":
      return `Set the repo ${token(a.repo, REPO, "repo")}'s weight in the contribution pool to ${n(a.weight)} (admin chains only)`;
    case "grant_soul":
      return `Grant ${soul(a.amount)} to ${account(a.to)} (admin chains only)${quoted(a.reason, "reason", opts)}`;
    case "vouch":
      return `Stake 10 FANG vouching for ${account(a.account)}`;
    case "unvouch":
      return `Withdraw the vouch for ${account(a.account)}`;
    case "flag_sybil":
      return `Flag ${account(a.account)} as a sybil (admin chains only)${quoted(a.reason, "reason", opts)}`;
    case "submit_claim":
      return `Claim PR ${token(a.repo, REPO, "repo")}#${n(a.pr)} for the contribution pool`;
    case "attest_claim":
      return `Attest contribution claim #${n(a.claim_id)} as ${a.ok === true ? "merged" : "not merged"}`;
    case "reject_claim":
      return `Reject contribution claim #${n(a.claim_id)} (admin chains only)${quoted(a.reason, "reason", opts)}`;
    case "join_jury":
      return `Join the Séance juror pool, bonding ${fangText(a.bond)}`;
    case "leave_jury":
      return "Leave the Séance juror pool";
    case "commit_votes":
      return `Seal a judgment in Séance #${n(a.seance_id)}`;
    case "reveal_votes":
      return `Reveal a judgment in Séance #${n(a.seance_id)}`;
    case "fund_pool":
      return `Add ${fangText(a.amount)} to the contribution pool`;
    case "withdraw_vested":
      return "Withdraw the contribution rewards that have vested";
    case "clawback":
      return `Claw back the unvested rewards of ${account(a.account)} (admin chains only)${quoted(a.reason, "reason", opts)}`;
    case "delegate":
      return `Stake ${fangText(a.amount)} with validator ${account(a.validator)}`;
    case "undelegate": {
      // Amount mode (audit gap-7-5): `shares` 0 and an amount the chain converts at its own price.
      const amount = whole(a.amount);
      if (amount !== undefined && amount > 0n) return `Start unbonding ${fangText(a.amount)} of stake from validator ${account(a.validator)} (the chain converts it to pool shares at its own price when this runs)`;
      if (a.amount !== undefined && amount === undefined) return `Start unbonding ${fangText(a.amount)} of stake from validator ${account(a.validator)}`;
      return `Start unbonding ${n(a.shares)} pool shares from validator ${account(a.validator)}`;
    }
    case "redelegate":
      return `Move ${n(a.shares)} pool shares from validator ${account(a.from_validator)} to validator ${account(a.to_validator)}`;
    case "withdraw_unbonded":
      return "Withdraw the stake that has finished unbonding";
    case "update_validator":
      return `Set the account's validator commission to ${n(a.commission_bps)} basis points${typeof a.p2p_addr === "string" && a.p2p_addr ? " and change its addresses" : ""}`;
    case "unjail":
      return `Unjail validator ${account(a.validator)}`;
    case "report_equivocation":
      return "Report a validator's equivocation (two conflicting signed votes)";
    case "beacon_commit":
      return `Commit to a randomness-beacon secret for epoch ${n(a.epoch)}`;
    case "beacon_reveal":
      return `Reveal the randomness-beacon secret for epoch ${n(a.epoch)}`;
    case "propose":
      return `Propose, bonding ${fangText(a.deposit)}: ${describeKind(a.kind, opts)}${quoted(a.title, "title", opts)}`;
    case "vote":
      return `Vote ${a.aye === true ? "aye" : a.aye === false ? "nay" : "(unreadable side)"} on proposal #${n(a.proposal_id)} with ${fangText(a.amount)} at conviction ${n(a.conviction)}`;
    case "unlock":
      return "Release the account's expired vote locks";
    case "multisig_create": {
      const owners = Array.isArray(a.owners) ? a.owners : [];
      return `Make the account a ${n(a.threshold)}-of-${owners.length} multisig of the owner keys ${owners.map((o) => hex(o, 32, "key id")).join(", ") || "(none)"}`;
    }
    case "multisig_propose":
      return `Propose a multisig action: ${describeAction(a.action, opts)}`;
    case "multisig_approve":
      return `Approve multisig action #${n(a.id)}`;
    case "renew_name":
      return `Renew the name ${nameField(a.name)} for ${n(a.years)} year(s)`;
    case "transfer_name":
      return `Offer the name ${nameField(a.name)} to ${account(a.to)} (theirs once they accept it; offered to the account itself, it accepts or withdraws an offer)`;
    case "release_name":
      return `Release the name ${nameField(a.name)}: anyone can register it afterwards`;
    case "claim_distribution":
      return `Claim ${fangText(a.amount)} from community distribution #${n(a.distribution_id)}`;
    case "attest_revert":
      return `Attest that contribution claim #${n(a.claim_id)}'s PR was reverted`;
    case "shield":
      return `Move ${fangText(a.amount)} into the Ghost Mode pool`;
    case "unshield":
      return `Pay ${fangText(a.amount)} out of the Ghost Mode pool to ${account(a.to)}`;
    case "private_transfer":
      return "A private Ghost Mode transfer";
    case "set_ghost_address":
      return "Publish the account's Ghost Mode address";
    case "ghost_pause":
      return "Vote to pause Ghost Mode (validators)";
    case "validator_ready":
      return `Signal that the account's candidate validator node is running, to join a set of one or two at the end of epoch ${n(a.epoch)} (validators)`;
    case "admit_validator":
      return `Admit candidate validator ${account(a.validator)} into a set of one or two at the end of epoch ${n(a.epoch)} (validators)`;
    default:
      return `Something this client can't read (an action of type ${typeof a.type === "string" ? token(a.type, /^[a-z_]{1,40}$/, "type") : "unknown"}): don't approve it from here`;
  }
}
