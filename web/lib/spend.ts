"use client";

// A fresh passkey check before every spend. The wallet stays unlocked for reading, but a
// transaction that moves FANG out of the account (or lets another key move it) is signed only
// right after the passkey verifies the user again, with a key re-derived from that very answer.
// Until then a review shows the exact transaction with a placeholder where the signature goes.

import { SIGNATURE_LEN, type Tx, type UnsignedTx, type Wallet } from "@blockula/sdk";
import { bytesToHex } from "@noble/hashes/utils.js";
import { reverifyPasskey } from "./keys";
import { useWallet } from "./wallet";

/**
 * A stand-in for `wallet` that builds just what it would (the same nonce, fee, height and
 * checks) but signs nothing: the signature is zeros of the real length, so the size, and so the
 * fee, are exactly those of the signed transaction.
 */
export function previewer(wallet: Wallet): Wallet {
  const w = Object.create(wallet) as Wallet;
  const signature = "00".repeat(SIGNATURE_LEN);
  Object.defineProperty(w, "sign", { value: (u: UnsignedTx): Tx => ({ ...u, public_key: bytesToHex(wallet.publicKey), signature }) });
  return w;
}

/** A transaction without its key and signature: what gets signed. */
export function unsignedOf(tx: Tx): UnsignedTx {
  const { public_key: _key, signature: _sig, ...unsigned } = tx;
  return unsigned;
}

/**
 * Returns `confirm()`: the passkey check to run immediately before signing a spend. It resolves
 * to the wallet that signs it (for a passkey wallet, the key re-derived from the fresh answer)
 * and throws `PasskeyCheckError` when the check is cancelled or fails. A browser-only key (test
 * chains only) has no passkey to ask, so it signs as it is.
 */
export function useSpendCheck() {
  const { wallet, hint } = useWallet();
  return async (): Promise<Wallet> => {
    if (!wallet || !hint) throw new Error("Unlock your wallet first.");
    if (hint.kind !== "passkey") return wallet;
    return reverifyPasskey(wallet, hint);
  };
}
