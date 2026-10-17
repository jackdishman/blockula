import type { Tx } from "@blockula/sdk";
import { type Holding, parseDrops } from "./ledger";
import { nodeGet, NodeError } from "./node";
import { preflight, type Preflight } from "./preflight";
import type { AccountDetail, ChainStatus, HuntView, NameDetail } from "./types";

const holding = (key: string, name: string): Holding => ({ address: `holding:${key}`, name, short: name, holding: true });

/**
 * A recipient as the chain knows it, and whether the chain has an account for it yet (a first
 * payment has a minimum). The node answers "balance 0, nonce 0" for a missing account, and an
 * account that exists almost never looks like that (only sending, which bumps the nonce, takes
 * FANG out; the contribution pool is the exception). Having a history isn't enough: being
 * vouched for or granted Soul doesn't create an account.
 */
async function recipientOf(address: string) {
  const a = await nodeGet<AccountDetail>(`/account/${encodeURIComponent(address)}`);
  return { label: a.account, isNew: a.balance === "0" && a.nonce === 0 };
}

/**
 * Preflight a built transaction against the chain as it is now: reads the sender, and whatever
 * the action touches (the recipient, the hunt, the name's price, the claim bond), fresh.
 */
export async function checkTx(tx: Tx, signer: string): Promise<{ pf: Preflight; blockMs: number }> {
  const action = tx.action as unknown as { type: string; [k: string]: unknown };
  const [st, mine, recipient, hunt, name] = await Promise.all([
    nodeGet<ChainStatus>("/status"),
    nodeGet<AccountDetail>(`/account/${tx.from}`),
    action.type === "transfer" ? recipientOf(String(action.to)) : null,
    action.type === "fund_hunt"
      ? nodeGet<HuntView>(`/hunt/${Number(action.hunt_id)}`).catch((e) => (e instanceof NodeError && e.status === 404 ? { id: Number(action.hunt_id), missing: true as const } : null))
      : null,
    action.type === "register_name" ? nodeGet<NameDetail>(`/name/${encodeURIComponent(String(action.name))}`) : null,
  ]);
  if (name && !name.available) throw new Error(`${name.name} is already taken`);
  const bond = action.type === "submit_claim" ? mine.contribution?.next_claim_bond : undefined;
  const extra = name ? { drops: parseDrops(name.price), to: holding("treasury", "Treasury"), why: `${name.name}'s price` } : bond ? { drops: parseDrops(bond), to: holding("claim-bond", "Claim bond"), why: "refundable claim bond" } : null;
  const pf = preflight({
    tx: { from: tx.from, fee: tx.fee, nonce: tx.nonce, action },
    sender: { label: mine.account, balance: mine.balance, keys: mine.keys, recent: mine.recent_txs, familiars: mine.familiars.map((f) => f.key_id) },
    signer,
    head: st.height,
    recipient,
    extra,
    hunt: hunt && "missing" in hunt ? hunt : hunt ? { id: hunt.id, status: hunt.status, expires_at: hunt.expires_at } : null,
  });
  return { pf, blockMs: st.block_time_ms || 1000 };
}
