// 👻 Ghost Mode (shielded transfers) through this MCP: what the ghost_* tools say and show.
// The tools themselves are in index.ts, next to the caps they're held to.
//
// Ghost Mode is NOT AUDITED and off on every chain but a local test chain (`blockula dev --ghost`).
// The rules these tools keep:
//   - the agent's Ghost Mode key comes from its own wallet key (`@blockula/sdk/ghost`,
//     `GhostWallet.fromWallet`: the key `blockula ghost` derives from the same seed) and never
//     leaves the SDK's WebAssembly: no tool returns or logs a seed, a spending key, a viewing key
//     or a note's secrets, and there is no tool that exports a viewing key;
//   - shield, send and unshield sign only for the agent's own account and spend only its own
//     notes: while it acts for another account (use_account) they're refused, since a session key
//     can't shield and nothing it holds can spend notes; and never on blockula-1;
//   - ghost_send pays an address that is one node's word, and a private transfer can't be undone:
//     without the fingerprint the recipient read out, it sends nothing and answers with the one
//     the node reported, for the user to check with them;
//   - everything counts toward BLOCKULA_MAX_SEND and BLOCKULA_MAX_DAILY like a send, fees included
//     (a shield's amount counts when it's shielded and again when it's sent on or unshielded: the
//     caps err high);
//   - a note's memo is its sender's text: `untrusted_memo`.
import { formatFang } from "@blockula/sdk";
import type { GhostScan } from "@blockula/sdk/ghost";
import { clean } from "./untrusted.js";

/** Said in every ghost tool's description and result. */
export const GHOST_NOTE = "Ghost Mode is NOT AUDITED and for local test chains only (`blockula dev --ghost`): never treat shielded FANG as safe, and never use it for anything that matters.";

/** Why shield, send and unshield are refused while the agent acts for another account. */
export const actingRefusal = (tool: string, account: string, own: string) =>
  `This agent is acting for ${account} (use_account), and ${tool} is refused there: session keys can't shield or spend private funds (a session key only sends transfers, and Ghost Mode has no spending limit a key could be held to). This MCP's Ghost Mode tools sign only for the agent's own account (${own}) and spend only its own notes; a user does this from their own wallet. use_account "self" switches back. Nothing was signed.`;

/** A drops amount from the node as FANG, or undefined when it isn't one. */
export const fangOf = (v: unknown) => (typeof v === "string" && /^\d{1,20}$/.test(v) ? formatFang(BigInt(v)) : undefined);

/** `/api/ghost` for the agent: numbers and amounts only, each held to its shape. */
export function poolView(g: any, proves: number): Record<string, unknown> {
  const int = (v: unknown) => (Number.isSafeInteger(v) && (v as number) >= 0 ? (v as number) : undefined);
  const active = g?.active === true;
  const paused = g?.paused === true;
  return {
    ghost_mode: !active ? "off" : paused ? "paused" : "on",
    audited: false,
    pool: fangOf(g?.pool),
    pool_cap: fangOf(g?.pool_cap),
    shielded_outputs: int(g?.frontier_size),
    nullifiers: int(g?.nullifiers),
    fee_private_transfer_or_unshield: fangOf(g?.canonical_fee),
    fee_unshield_to_new_account: fangOf(g?.canonical_fee_new_account),
    verify_fee_per_shield: fangOf(g?.verify_fee),
    circuit_id: int(g?.circuit_id),
    ...(int(g?.circuit_id) !== undefined && g.circuit_id !== proves ? { cannot_prove: `the node verifies circuit ${g.circuit_id} and this MCP proves circuit ${proves}: update @blockula/mcp` } : {}),
    next: !active
      ? "Ghost Mode is off on this chain, so every ghost_* tool that sends is refused. It ships disabled; only a local test chain made with `blockula dev --ghost` has it on."
      : paused
        ? "Ghost Mode is paused on this chain (its validators stopped the pool; only governance resumes it): nothing can be shielded, sent or unshielded."
        : "ghost_address shows this agent's fingerprint (and publishes its address with publish and confirm), ghost_shield moves FANG in, ghost_balance lists its notes, ghost_send pays privately, ghost_unshield moves FANG out.",
    note: GHOST_NOTE,
  };
}

/** What a scan found: amounts, positions and heights as facts, each memo as its sender's text. */
export function scanView(s: GhostScan): Record<string, unknown> {
  return {
    shielded_balance: s.display,
    as_of_block: s.height,
    unspent_notes: s.notes.map((n) => ({ amount: n.display, leaf: n.position, block: n.height, ...(n.memo ? { untrusted_memo: clean(n.memo, 300) } : {}) })),
    scanned: { new_outputs: s.newLeaves, outputs_in_tree: s.leaves, notes_found: s.found, notes_spent: s.spent },
  };
}
