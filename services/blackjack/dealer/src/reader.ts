/**
 * Where the daemon's table reads come from, by its configuration: the node's storage route as it
 * is (one node's word), or, with `witnesses`, Merkle proofs whose state roots those nodes confirm
 * (`ProvenTable`). docs/proposals/blackjack-card-privacy.md says why it matters: the dealer
 * reveals whatever links a hand's record says it is owed.
 */
import { Table } from "@blockula/blackjack";
import { ProofError, ProvenTable } from "@blockula/blackjack/proven";
import { Client } from "@blockula/sdk";
import { isLocalRpc, type DealerConfig } from "./config.js";
import type { LogFields } from "./log.js";

/** The table reader the configuration asks for. `node` is the client for `cfg.rpc`. */
export function tableReader(cfg: DealerConfig, node: Client): Table {
  if (cfg.witnesses.length === 0) return Table.at(cfg.rpc, cfg.table);
  // Each witness is pinned to the same chain: one that reports another is no confirmation.
  const witnesses = cfg.witnesses.map((rpc) => new Client(rpc, { chainId: cfg.chainId, genesisHash: cfg.genesisHash }));
  return new ProvenTable(cfg.table, node, witnesses, { quorum: cfg.witnessQuorum, maxLagBlocks: cfg.proofMaxLagBlocks });
}

/** How the reads are trusted, for `status` and the log. */
export function readsText(cfg: DealerConfig): string {
  const n = cfg.witnesses.length;
  if (n === 0) return isLocalRpc(cfg.rpc) ? "the node's word (a node on this machine)" : "the node's word";
  return `proven: each record's Merkle proof, its state root confirmed by ${cfg.witnessQuorum ?? n} of ${n} witness${n === 1 ? "" : "es"}`;
}

/**
 * What the daemon says once at its start about whom it trusts. Warnings, never refusals: a
 * configuration that ran before runs as it did.
 */
export function trustNotes(cfg: DealerConfig): { level: "info" | "warn"; action: string; fields: LogFields }[] {
  const out: { level: "info" | "warn"; action: string; fields: LogFields }[] = [];
  const remote = !isLocalRpc(cfg.rpc);
  if (cfg.witnesses.length) out.push({ level: "info", action: "proven_reads", fields: { witnesses: cfg.witnesses.length, quorum: cfg.witnessQuorum ?? cfg.witnesses.length, max_lag_blocks: cfg.proofMaxLagBlocks } });
  else if (remote) {
    out.push({
      level: "warn",
      action: "unproven_remote_node",
      fields: { advice: "the dealer reveals the cards a hand's record says it is owed, and takes that record on this node's word: a node that lies about a hand is sent cards nobody is owed yet. Run a node on this machine, or name other nodes of the chain as witnesses (BJ_DEALER_WITNESSES) so that every record is proven" },
    });
  }
  if (remote && cfg.rpc.startsWith("http://")) out.push({ level: "warn", action: "insecure_rpc", fields: { advice: "the node is another machine's, over plain http: anyone on the path can answer for it and read what the dealer sends. Use https" } });
  return out;
}

/** Whether `e` says the node's answer was wrong (not merely late or unconfirmed): something a person should look at. */
export function isNodeFault(e: unknown): e is ProofError {
  return e instanceof ProofError && (e.kind === "disagreement" || e.kind === "bad_proof");
}
