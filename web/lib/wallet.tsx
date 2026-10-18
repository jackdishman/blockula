"use client";

import { Wallet } from "@blockula/sdk";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { ChainMismatchError, networkClient, type PinnedClient } from "./chain";
import { ensureRpcCookie } from "./config";
import { friendly } from "./errors";
import { clearHint, loadHint, saveHint, tutorialScope, type Hint } from "./session";
import type { AccountView, Status } from "./types";

/** Is the node on the chain this wallet is pinned to? A mismatch is final until reload. */
export type ChainState =
  | { state: "checking" }
  | { state: "ok" }
  | { state: "unreachable"; error: string }
  | { state: "mismatch"; error: ChainMismatchError };

interface Ctx {
  client: PinnedClient;
  /** Has localStorage been read yet? (false during SSR/first paint) */
  ready: boolean;
  hint: Hint | null;
  wallet: Wallet | null;
  account: AccountView | null;
  status: Status | null;
  chain: ChainState;
  refresh: () => Promise<void>;
  /** Store the unlocked wallet in memory and its non-secret hints on disk. */
  unlocked: (wallet: Wallet, hint: Hint) => void;
  updateHint: (patch: Partial<Hint>) => void;
  lock: () => void;
  forget: () => void;
}

const WalletCtx = createContext<Ctx | null>(null);

export function WalletProvider({ children }: { children: ReactNode }) {
  const client = useMemo(() => networkClient(), []);
  const [ready, setReady] = useState(false);
  const [chain, setChain] = useState<ChainState>({ state: "checking" });
  const mismatch = useRef(false);
  const [hint, setHint] = useState<Hint | null>(null);
  const [wallet, setWallet] = useState<Wallet | null>(null);
  const [account, setAccount] = useState<AccountView | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const walletRef = useRef<Wallet | null>(null);
  walletRef.current = wallet;

  useEffect(() => {
    ensureRpcCookie();
    setHint(loadHint());
    setReady(true);
  }, []);

  const refresh = useCallback(async () => {
    if (mismatch.current) return;
    const st = await client.status().catch((e) => {
      if (e instanceof ChainMismatchError) {
        // Wrong chain: drop the key from memory and stop talking to this node.
        mismatch.current = true;
        walletRef.current = null;
        setWallet(null);
        setAccount(null);
        setChain({ state: "mismatch", error: e });
      } else setChain((c) => (c.state === "ok" ? c : { state: "unreachable", error: friendly(e) }));
      return null;
    });
    if (!st) return;
    setStatus(st);
    setChain((c) => (c.state === "ok" ? c : { state: "ok" }));
    const w = walletRef.current;
    if (!w || !hintRef.current?.account) return;
    const a = (await client.account(w.account).catch(() => null)) as AccountView | null;
    if (a && walletRef.current === w) {
      setAccount(a);
      // The name on this chain, or none: a name kept from another chain (a relaunched testnet) isn't this account's here.
      const name = a.names[0];
      if (hintRef.current && hintRef.current.name !== name) {
        const h = { ...hintRef.current, name };
        saveHint(h);
        setHint(h);
      }
    }
  }, [client]);

  const hintRef = useRef<Hint | null>(null);
  hintRef.current = hint;

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 3000);
    return () => clearInterval(t);
  }, [refresh, wallet]);

  const unlocked = useCallback((w: Wallet, h: Hint) => {
    saveHint(h);
    hintRef.current = h;
    walletRef.current = w;
    setHint(h);
    setWallet(w);
    setAccount(null);
    refresh();
  }, [refresh]);

  const updateHint = useCallback((patch: Partial<Hint>) => {
    const h = { ...(hintRef.current as Hint), ...patch };
    saveHint(h);
    hintRef.current = h;
    setHint(h);
    refresh();
  }, [refresh]);

  const lock = useCallback(() => {
    setWallet(null);
    setAccount(null);
  }, []);

  const forget = useCallback(() => {
    clearHint();
    setHint(null);
    setWallet(null);
    setAccount(null);
  }, []);

  const value = { client, ready, hint, wallet, account, status, chain, refresh, unlocked, updateHint, lock, forget };
  // Render client-side only: the chosen network and hints live in this browser, so server HTML
  // would disagree with the client on first paint.
  return <WalletCtx.Provider value={value}>{ready ? children : null}</WalletCtx.Provider>;
}

export function useWallet() {
  const c = useContext(WalletCtx);
  if (!c) throw new Error("useWallet outside WalletProvider");
  return c;
}

/** Hook for the wallet-required pages: the gate guarantees these are set. */
export function useUnlocked() {
  const c = useWallet();
  return { ...c, wallet: c.wallet as Wallet, hint: c.hint as Hint };
}

/** The checklist's scope (lib/session `tutorialScope`): this chain and the account, once the node has reported its chain. */
export function useTutorialScope(): string | null {
  const { status, hint } = useWallet();
  return status && hint?.account ? tutorialScope(status.chain_id, status.genesis_hash, hint.account) : null;
}
