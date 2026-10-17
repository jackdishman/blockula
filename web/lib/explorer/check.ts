import type { Tx } from "@blockula/sdk";
import { type Holding, parseDrops } from "./ledger";
import { nodeGet, NodeError } from "./node";
import { preflight, type Preflight } from "./preflight";
import type { AccountDetail, ChainStatus, HuntView, MultisigView, NameDetail, ValidatorsDetail } from "./types";

const holding = (key: string, name: string): Holding => ({ address: `holding:${key}`, name, short: name, holding: true });

/** `/api/fees`, for the account-creation fee on nodes whose `/api/account` doesn't report it. */
interface Fees {
  account_creation_fee?: string;
}

const missing = <T,>(p: Promise<T>) => p.catch((e) => (e instanceof NodeError && e.status === 404 ? null : Promise.reject(e)));

/**
 * A recipient as the chain knows it, whether the chain has an account for it yet (a first
 * payment has a minimum, and opening it burns the account-creation fee from the payer). Protocol
 * v1 nodes say both (`exists`, `creation_fee`). Older ones answer "balance 0, nonce 0" for a
 * missing account, and an account that exists almost never looks like that (only sending, which
 * bumps the nonce, takes FANG out; the contribution pool is the exception).
 */
async function recipientOf(to: string, fees: Promise<Fees | null>) {
  const a = await nodeGet<AccountDetail>(`/account/${encodeURIComponent(to)}`);
  const isNew = a.exists != null ? !a.exists : a.balance === "0" && a.nonce === 0;
  const creationFee = a.creation_fee != null ? parseDrops(a.creation_fee) : isNew ? parseDrops((await fees)?.account_creation_fee ?? "0") : 0n;
  return { label: a.account, isNew, creationFee };
}

/**
 * Preflight a built transaction against the chain as it is now: reads the sender (its locks, and
 * whether it's a multisig), and whatever the action touches (the recipient, the hunt, the name's
 * price, the claim bond), fresh.
 */
export async function checkTx(tx: Tx, signer: string): Promise<{ pf: Preflight; blockMs: number }> {
  const action = tx.action as unknown as { type: string; [k: string]: unknown };
  const pays = action.type === "transfer" || action.type === "award_hunt";
  const huntId = ["fund_hunt", "award_hunt", "cancel_hunt"].includes(action.type) ? Number(action.hunt_id) : null;
  const fees = pays ? nodeGet<Fees>("/fees").catch(() => null) : Promise.resolve(null);
  const [st, mine, multisig, recipient, hunt, name, vals] = await Promise.all([
    nodeGet<ChainStatus>("/status"),
    nodeGet<AccountDetail>(`/account/${tx.from}`),
    missing(nodeGet<MultisigView>(`/multisig/${tx.from}`)).catch(() => null),
    pays ? recipientOf(String(action.to), fees) : null,
    huntId != null ? missing(nodeGet<HuntView>(`/hunt/${huntId}`)).then((h) => h ?? { id: huntId, missing: true as const }, () => null) : null,
    action.type === "register_name" || action.type === "renew_name" ? nodeGet<NameDetail>(`/name/${encodeURIComponent(String(action.name))}`) : null,
    action.type === "cancel_hunt" ? nodeGet<ValidatorsDetail>("/validators").catch(() => null) : null,
  ]);
  if (name && action.type === "register_name" && !name.available) throw new Error(`${name.name} is already taken`);
  const bond = action.type === "submit_claim" ? mine.contribution?.next_claim_bond : undefined;
  const years = BigInt(Number.isSafeInteger(action.years) ? (action.years as number) : 1);
  const extra =
    name && action.type === "register_name"
      ? { drops: parseDrops(name.price), to: holding("treasury", "Treasury"), why: `${name.name}'s price` }
      : name && action.type === "renew_name" && name.renewal_price
        ? { drops: parseDrops(name.renewal_price) * years, to: holding("treasury", "Treasury"), why: `renewing ${name.name} for ${years} year${years === 1n ? "" : "s"}` }
        : bond
          ? { drops: parseDrops(bond), to: holding("claim-bond", "Claim bond"), why: "refundable claim bond" }
          : null;
  const lock = [...(mine.locks ?? [])].sort((a, b) => (parseDrops(b.amount) > parseDrops(a.amount) ? 1 : -1))[0];
  const pf = preflight({
    tx: { from: tx.from, fee: tx.fee, tip: tx.tip, nonce: tx.nonce, action },
    sender: {
      label: mine.account,
      balance: mine.balance,
      transferable: mine.transferable,
      locked: mine.locked,
      lockReason: lock ? `${lock.reason}${lock.vesting ? "" : ` until block #${lock.until.toLocaleString("en-US")}`}` : undefined,
      keys: mine.keys,
      recent: mine.recent_txs,
      familiars: mine.familiars.map((f) => f.key_id),
      multisig: multisig ? { threshold: multisig.threshold, owners: multisig.owners.length } : null,
    },
    signer,
    head: st.height,
    recipient,
    extra,
    hunt: hunt && "missing" in hunt ? hunt : hunt ? { id: hunt.id, status: hunt.status, expires_at: hunt.expires_at, creator: hunt.creator.address } : null,
    admins: vals?.admins?.map((a) => a.address),
  });
  return { pf, blockMs: st.block_time_ms || 1000 };
}
