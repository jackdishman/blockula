// The txs this MCP signed, by id, in BLOCKULA_HOME (the newest MAX_SIGNED of them). tx_status
// reads them for what a node's answer can't be trusted with, or doesn't carry: a tx's valid_until,
// the last block it can execute in. A node answers a tx it dropped from its mempool without the
// tx itself, so without this the agent couldn't tell when a dropped tx is dead (audit
// review-fix-clients-2). The record is what this MCP signed, so no node can change it.
//
// It only ever informs: a missing or unreadable file means "valid_until unknown", and tx_status
// then tells the agent not to send the tx again until it's final, which is the conservative
// answer. Spending caps don't depend on it (see spend.ts). The account and nonce let tx_status
// tell a tx a node can't show (pruned, or never held) that ran from one still in flight: once the
// account's nonce is past it, it (or a tx that took its nonce) ran (audit review-r3-wallet-1).
// Every network the MCP runs on shares this file, so each record names the chain it was signed
// for, and tx_status only reads one against the chain it's on (audit review-r4-wallet-3).
import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

/** What tx_status needs about a tx this MCP signed. */
export interface SignedTx {
  /** The last height it can execute at, as signed. */
  valid_until: number;
  /** The account it acts from, and its nonce: at most one tx per (account, nonce) executes. */
  account: string;
  nonce: number;
  /** The chain it was signed for (the tx's chain_id and genesis_hash). Records written before these were kept have neither. */
  chain_id?: string;
  genesis_hash?: string;
}

/** How many signed txs are kept (oldest dropped first). */
export const MAX_SIGNED = 500;

const ID = /^[0-9a-f]{64}$/;
/** An address as tx_status reads and quotes it: lowercase bech32 characters only. */
const ACCOUNT = /^[a-z0-9]{1,128}$/;
/** A chain id and a genesis hash as tx_status quotes them. */
const CHAIN_ID = /^[A-Za-z0-9-]{1,64}$/;
const GENESIS = /^[0-9a-f]{64}$/;
const optional = (v: unknown, re: RegExp) => v === undefined || (typeof v === "string" && re.test(v));

/** Whether a record was signed for this chain: a record that doesn't name its chain never is. */
export function onChain(tx: SignedTx, chain: { chainId?: string; genesisHash?: string }): boolean {
  return tx.chain_id !== undefined && tx.genesis_hash !== undefined && tx.chain_id === chain.chainId && tx.genesis_hash === chain.genesisHash;
}

export class SignedTxs {
  constructor(readonly file: string, readonly max = MAX_SIGNED) {}

  private read(): [string, SignedTx][] {
    try {
      const f = JSON.parse(readFileSync(this.file, "utf8")) as { txs?: unknown };
      if (!Array.isArray(f.txs)) return [];
      return f.txs.filter((e): e is [string, SignedTx] => Array.isArray(e) && typeof e[0] === "string" && ID.test(e[0]) && typeof e[1] === "object" && e[1] !== null && Number.isSafeInteger(e[1].valid_until) && Number.isSafeInteger(e[1].nonce) && typeof e[1].account === "string" && ACCOUNT.test(e[1].account) && optional(e[1].chain_id, CHAIN_ID) && optional(e[1].genesis_hash, GENESIS));
    } catch {
      return [];
    }
  }

  private static copy(t: SignedTx): SignedTx {
    return { valid_until: t.valid_until, account: t.account, nonce: t.nonce, ...(t.chain_id !== undefined ? { chain_id: t.chain_id } : {}), ...(t.genesis_hash !== undefined ? { genesis_hash: t.genesis_hash } : {}) };
  }

  /** The record of a tx this MCP signed, if it's still kept. */
  get(id: string): SignedTx | undefined {
    const hit = this.read().find(([k]) => k === id);
    return hit ? SignedTxs.copy(hit[1]) : undefined;
  }

  /** The other txs this MCP signed for the same chain with the same (account, nonce), oldest first: at most one of them all executes. */
  sameSlot(mine: SignedTx, except: string): string[] {
    return this.read().filter(([k, t]) => k !== except && t.account === mine.account && t.nonce === mine.nonce && onChain(t, { chainId: mine.chain_id, genesisHash: mine.genesis_hash })).map(([k]) => k);
  }

  /** Records a tx about to be submitted. Written whole (temp file, then rename). */
  add(id: string, tx: SignedTx) {
    if (!ID.test(id)) throw new Error("a tx id is 64 lowercase hex characters");
    if (!optional(tx.chain_id, CHAIN_ID) || !optional(tx.genesis_hash, GENESIS)) throw new Error("a chain id and genesis hash as the node reports them");
    const txs = this.read().filter(([k]) => k !== id);
    txs.push([id, SignedTxs.copy(tx)]);
    mkdirSync(dirname(this.file), { recursive: true });
    const tmp = `${this.file}.${process.pid}.tmp`;
    writeFileSync(tmp, JSON.stringify({ txs: txs.slice(-this.max) }), { mode: 0o600 });
    renameSync(tmp, this.file);
  }
}
