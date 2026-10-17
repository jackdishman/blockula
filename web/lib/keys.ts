"use client";

import { Wallet } from "@blockula/sdk";
import { createPasskeyWallet, unlockPasskeyWallet } from "@blockula/sdk/passkey";
import type { PinnedClient } from "./chain";
import { localKeysAllowed, RP_ID, rpIdProblem } from "./config";
import { b64url, fromB64url, type Hint } from "./session";
import type { AccountView, KeyAccounts } from "./types";

const DEV_HOSTS = ["localhost", "127.0.0.1"];

/**
 * Passkey wallets are bound to one hostname for good. Only allow them on the production domain
 * (or localhost for development), never on preview deployments that will disappear.
 */
function walletRpId(): string {
  const problem = rpIdProblem();
  if (problem) throw new Error(`This mainnet wallet is misconfigured (${problem}), so it won't create or open passkeys. Mainnet passkeys must belong to a domain the project owns.`);
  const host = window.location.hostname;
  if (DEV_HOSTS.includes(host)) return host;
  if (host !== RP_ID) {
    throw new Error(`Passkey wallets live at https://${RP_ID}. A wallet made here (${host}) couldn't be opened anywhere else, so open the wallet at ${RP_ID} instead.`);
  }
  return RP_ID;
}

export async function createPasskey(client: PinnedClient, username: string) {
  const wallet = await createPasskeyWallet({ username, client, rpName: "Blockula Wallet", rpId: walletRpId() });
  const credentialId = wallet.credentialId ? b64url(wallet.credentialId) : undefined;
  return { wallet, hint: { v: 1, kind: "passkey", credentialId, keyId: wallet.keyId } satisfies Hint };
}

export async function unlockPasskey(client: PinnedClient, credentialId?: string) {
  const wallet = await unlockPasskeyWallet({ client, rpId: walletRpId(), credentialId: credentialId ? fromB64url(credentialId) : undefined });
  return { wallet, credentialId: wallet.credentialId ? b64url(wallet.credentialId) : credentialId };
}

export function createLocalKey(client: PinnedClient) {
  if (!localKeysAllowed(client.pin.chainId)) throw new Error("Browser-only keys are disabled on mainnet.");
  const wallet = Wallet.generate(client);
  return { wallet, hint: { v: 1, kind: "local", keyId: wallet.keyId, seed: wallet.seedHex } satisfies Hint };
}

export type AccountChoice = { address: string; name: string | null; short: string; role: string; label: string; balance: string };

/**
 * Accounts this key may act for. The node always lists the key's own address (implicitly
 * authorized) even if it was never used, so hide it when it's empty and there's somewhere better.
 */
export async function findAccounts(client: PinnedClient, wallet: Wallet): Promise<AccountChoice[]> {
  // Only accounts this key agreed to join list it (AddKey carries the key's consent, audit
  // gap-7-2), a page at a time; a few pages is more than any real key has.
  const listed: KeyAccounts["accounts"] = [];
  let after: string | undefined;
  for (let page = 0; page < 5; page++) {
    const res = (await client.keyAccounts(wallet.keyId, after ? { after } : {})) as KeyAccounts;
    listed.push(...res.accounts);
    if (!res.next) break;
    after = res.next;
  }
  const out: AccountChoice[] = [];
  // Node quirk: once an account has added any key, /api/key lists the key's own account twice.
  const seen = new Set<string>();
  for (const a of listed) {
    if (seen.has(a.account.address)) continue;
    seen.add(a.account.address);
    const acct = (await client.account(a.account.address).catch(() => null)) as AccountView | null;
    out.push({ address: a.account.address, name: acct?.names[0] ?? a.account.name, short: a.account.short, role: a.role.type, label: a.label, balance: acct?.balance_display ?? "0 FANG" });
  }
  const others = out.filter((a) => a.address !== wallet.address);
  const own = out.find((a) => a.address === wallet.address);
  const ownUsed = own && (own.balance !== "0 FANG" || own.name);
  if (others.length && !ownUsed) return others;
  return out;
}
