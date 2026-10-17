"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { explorer, NETWORK, NETWORKS, RPC, setNetwork, type NetworkKey } from "@/lib/config";
import { isExplorerPath } from "@/lib/explorer/routes";
import { useWallet } from "@/lib/wallet";
import { CommandPalette, openPalette } from "./CommandPalette";
import { Alerts } from "./explorer/Alerts";

export function Header() {
  const { wallet, hint, lock } = useWallet();
  const path = usePathname();
  const router = useRouter();
  const open = wallet && hint?.account;
  // A program's records (a hunt, a claim, a Séance) belong to Programs; every other chain page to Explore.
  const programs = /^\/(programs|hunt|claim|seance)(\/|$)/.test(path);
  const explore = isExplorerPath(path) && !programs;
  return (
    <>
      <header className="top">
        <Link href="/" className="brand" aria-label="Blockula wallet, home">
          <span className="wordmark">Blockula</span>
          <span className="edition">The Wallet</span>
        </Link>
        <nav>
          <button type="button" className="search-btn" onClick={openPalette} aria-label="Search the chain (⌘K)">
            <span className="label">Search the chain…</span>
            <span className="label-short">Search</span>
            <span className="kbd">⌘K</span>
          </button>
          <Link href="/" className={path === "/" ? "active" : ""}>{hint?.account ? "Wallet" : "Open wallet"}</Link>
          <Link href={explorer.home()} className={explore ? "active" : ""}>Explore</Link>
          <Link href="/programs" className={programs ? "active" : ""}>Programs</Link>
          {open && (
            <>
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
      <CommandPalette />
      <Alerts />
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
