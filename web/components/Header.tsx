"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { DEFAULT_NETWORK, explorer, isNodeUrl, NETWORK, NETWORKS, RPC, setNetwork, type Network, type NetworkKey } from "@/lib/config";
import { useWallet } from "@/lib/wallet";

/** Which network this is, on every screen. Anything but a known mainnet is stamped as such. */
export function NetworkBadge() {
  const { client, status, chain } = useWallet();
  const kind = NETWORK.key === "custom" ? "custom" : NETWORK.mainnet ? "main" : "test";
  const chainId = chain.state === "mismatch" ? "wrong chain" : (status?.chain_id ?? client.pin.chainId ?? "…");
  return (
    <span className={`net-badge ${kind}`} data-testid="network-badge" title={`Connected to ${RPC}`}>
      <span className="net-name">{NETWORK.label}</span>
      <span className="net-chain">{chainId}</span>
    </span>
  );
}

export function Header() {
  const { wallet, hint, lock } = useWallet();
  const path = usePathname();
  const router = useRouter();
  const open = wallet && hint?.account;
  const known = Object.values(NETWORKS).filter((n): n is Network => !!n);
  return (
    <>
      <header className="top">
        <span className="masthead">
          <Link href="/" className="brand" aria-label="Blockula wallet, home">
            <span className="wordmark">Blockula</span>
            <span className="edition">The Wallet</span>
          </Link>
          <NetworkBadge />
        </span>
        <nav>
          {open && (
            <>
              <Link href="/" className={path === "/" ? "active" : ""}>Home</Link>
              <Link href="/devices" className={path === "/devices" ? "active" : ""}>Devices &amp; agents</Link>
            </>
          )}
          <select
            className="net-select small"
            aria-label="Network"
            title={`Connected to ${RPC}`}
            value={NETWORK.key}
            onChange={(e) => {
              const key = e.target.value as NetworkKey;
              if (key !== "custom") return setNetwork(key);
              const rpc = window.prompt(
                "Custom node URL (https://…)\n\nWarning: only use a node you run or fully trust. A node decides what names resolve to and what balances you see, so a malicious node can trick you into paying the wrong person.",
              );
              if (rpc && isNodeUrl(rpc.trim())) setNetwork("custom", rpc.trim());
              else e.target.value = NETWORK.key;
            }}
          >
            {known.map((n) => (
              <option key={n.key} value={n.key}>{n.label}</option>
            ))}
            <option value="custom">{NETWORK.key === "custom" ? `Custom: ${RPC.replace(/^https?:\/\//, "")}` : "Custom node…"}</option>
          </select>
          <a href={explorer.home()} target="_blank" rel="noreferrer">Explorer ↗</a>
          {open && (
            <button
              className="link caps"
              title="Lock: forget the key in memory until you unlock again"
              onClick={() => {
                lock();
                router.push("/");
              }}
            >
              Lock
            </button>
          )}
        </nav>
      </header>
      {NETWORK.key === "custom" && (
        <div className="banner danger">
          <b>Custom node: {RPC}.</b> It controls what names resolve to and what balances you see. Only send money if you trust whoever runs it.{" "}
          <button className="link" onClick={() => setNetwork(DEFAULT_NETWORK.key)}>Switch back to {DEFAULT_NETWORK.label}</button>
        </div>
      )}
      {hint?.kind === "local" && (
        <div className="banner">
          <b>Browser-only key, for testing.</b> Its secret is stored in this browser, not protected by a passkey. Don&apos;t keep real value here.
        </div>
      )}
    </>
  );
}
