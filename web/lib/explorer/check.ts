import type { Tx } from "@blockula/sdk";
import { nodeGet, NodeError } from "./node";
import { preflight, preflightInput, type Fees, type Preflight, type Reads } from "./preflight";
import type { AccountDetail, ChainStatus, HuntView, MultisigView, NameDetail, ValidatorsDetail } from "./types";

const missing = <T,>(p: Promise<T>) => p.catch((e) => (e instanceof NodeError && e.status === 404 ? null : Promise.reject(e)));

/**
 * Preflight a built transaction against the chain as it is now: reads the sender (its locks, and
 * whether it's a multisig), and whatever the action touches (the recipient, the hunt, the name's
 * price, the claim bond), fresh. What those reads mean is `preflightInput`'s (pure, and tested on
 * a dev chain's own answers: web/lib/v1.test.mjs).
 */
export async function checkTx(tx: Tx, signer: string, intent?: string): Promise<{ pf: Preflight; blockMs: number }> {
  const action = tx.action as unknown as { type: string; [k: string]: unknown };
  const pays = action.type === "transfer" || action.type === "award_hunt";
  const huntId = ["fund_hunt", "award_hunt", "cancel_hunt"].includes(action.type) ? Number(action.hunt_id) : null;
  const [status, account, multisig, recipient, fees, hunt, name, validators] = await Promise.all([
    nodeGet<ChainStatus>("/status"),
    nodeGet<AccountDetail>(`/account/${tx.from}`),
    missing(nodeGet<MultisigView>(`/multisig/${tx.from}`)).catch(() => null),
    pays || action.type === "transfer_name" ? nodeGet<AccountDetail>(`/account/${encodeURIComponent(String(action.to))}`) : null,
    pays ? nodeGet<Fees>("/fees").catch(() => null) : null,
    huntId != null ? missing(nodeGet<HuntView>(`/hunt/${huntId}`)).then((h) => h ?? { id: huntId, missing: true as const }, () => null) : null,
    action.type === "register_name" || action.type === "renew_name" || action.type === "transfer_name" ? nodeGet<NameDetail>(`/name/${encodeURIComponent(String(action.name))}`) : null,
    action.type === "cancel_hunt" ? nodeGet<ValidatorsDetail>("/validators").catch(() => null) : null,
  ]);
  const reads: Reads = { status, account, multisig, recipient, fees, hunt, name, validators };
  if (name && action.type === "register_name" && !name.available) throw new Error(`${name.name} is already taken`);
  return { pf: preflight(preflightInput({ from: tx.from, fee: tx.fee, tip: tx.tip, nonce: tx.nonce, action }, signer, reads, intent)), blockMs: status.block_time_ms || 1000 };
}
