"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { explorer, NETWORK, NETWORKS, RPC, setNetwork, type NetworkKey } from "@/lib/config";
import { useWallet } from "@/lib/wallet";

export function Header() {
  const { wallet, hint, lock } = useWallet();
  const path = usePathname();
  const router = useRouter();
  const open = wallet && hint?.account;
  return (
    <>
      <header className="top">
        <Link href="/" className="brand" aria-label="Blockula wallet, home">
          <span className="wordmark">Blockula</span>
          <span className="edition">The Wallet</span>
        </Link>
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
              if (rpc && /^https?:\/\/[^\s]+$/.test(rpc.trim())) setNetwork("custom", rpc.trim());
              else e.target.value = NETWORK.key;
            }}
          >
            {Object.entries(NETWORKS).map(([k, n]) => (
              <option key={k} value={k}>{n.label}</option>
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
          <button className="link" onClick={() => setNetwork("testnet")}>Switch back to Testnet</button>
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
