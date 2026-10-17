"use client";

import type { ReactNode } from "react";
import { saveCustomPin, type ChainMismatchError } from "@/lib/chain";
import { NETWORK, NETWORKS, RPC, setNetwork, type Network } from "@/lib/config";
import { useWallet } from "@/lib/wallet";
import { Splash } from "./Gate";
import { Apparition } from "./Haunt";
import { Tip } from "./ui";

/** Nothing renders until the node has proved it's on the expected chain; a wrong chain blocks the app. */
export function ChainGate({ children }: { children: ReactNode }) {
  const { chain, refresh } = useWallet();
  if (chain.state === "checking") return <Splash text="Checking which chain this node is on…" />;
  if (chain.state === "mismatch") return <WrongChain error={chain.error} />;
  return (
    <>
      {chain.state === "unreachable" && (
        <div className="banner danger" role="alert">
          <b>Can&apos;t reach the node.</b> {chain.error}{" "}
          <button className="link" onClick={() => refresh()}>Retry</button>
        </div>
      )}
      {children}
    </>
  );
}

function WrongChain({ error }: { error: ChainMismatchError }) {
  const { expected: e, reported: r } = error;
  const others = Object.values(NETWORKS).filter((n): n is Network => !!n && n.key !== NETWORK.key);
  return (
    <main className="narrow stack-lg" data-testid="wrong-chain">
      <div className="center">
        <Apparition kind="candle" />
        <span className="eyebrow">Wrong chain</span>
        <h1>This node isn&apos;t on {NETWORK.label}</h1>
        <p className="lead">{error.message} The wallet won&apos;t show balances or sign anything through it.</p>
      </div>
      <section className="card stack warn-card">
        <dl className="kv">
          <dt>Node</dt>
          <dd className="mono">{RPC}</dd>
          <dt>Expected</dt>
          <dd className="mono">
            {e.chainId ?? "any chain"}
            {e.genesisHash && <span className="block dim">genesis {e.genesisHash}</span>}
          </dd>
          <dt>Node says</dt>
          <dd className="mono">
            {r.chainId}
            {r.genesisHash && <span className="block dim">genesis {r.genesisHash}</span>}
            {r.prefix && <span className="block dim">addresses {r.prefix}1…</span>}
          </dd>
        </dl>
      </section>
      <div className="stack">
        {others.map((n) => (
          <button key={n.key} className="btn" onClick={() => setNetwork(n.key)}>Switch to {n.label}</button>
        ))}
        {NETWORK.key === "custom" && (
          <button
            className="btn danger"
            onClick={() => {
              if (!confirm(`Trust ${RPC} on chain “${r.chainId}” from now on? Only do this if you reset or moved your own node.`)) return;
              saveCustomPin(NETWORK.rpc, null);
              window.location.reload();
            }}
          >
            I reset this node: trust its new chain
          </button>
        )}
        {NETWORK.key === "local" && <button className="btn" onClick={() => window.location.reload()}>Reload (after resetting your local node)</button>}
      </div>
      <Tip>
        Each Blockula network has its own chain id and genesis. A node on a different chain could show balances and names that aren&apos;t real, or get you to sign a transaction meant for another network, so the wallet stops here.
      </Tip>
    </main>
  );
}
