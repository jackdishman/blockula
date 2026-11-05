"use client";

import { useWallet } from "@/lib/wallet";

/** The connected chain's ticker: FANG unless it was summoned with `--token`. */
export function Unit() {
  const { status } = useWallet();
  return <>{status?.token ?? "FANG"}</>;
}
