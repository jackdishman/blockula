"use client";

import { isAddress, Wallet } from "@blockula/sdk";
import { createPasskeyWallet, unlockPasskeyWallet } from "@blockula/sdk/passkey";
import type { PinnedClient } from "./chain";
import { localKeysAllowed, RP_ID, rpIdProblem } from "./config";
import { CHECK_CANCELLED } from "./errors";
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

/**
 * Why passkeys can't work on this page, or null if they can. Browsers offer passkeys only in a
 * secure context (https, or localhost), and only for a domain name, never an IP address: a node
 * reached at `http://100.x.y.z:7070` serves the wallet, but its passkey button would only fail.
 */
export function passkeyProblem(): "insecure" | "ip" | "unsupported" | null {
  if (typeof window === "undefined") return null;
  const host = window.location.hostname;
  if (!window.isSecureContext) return "insecure";
  if (!DEV_HOSTS.includes(host) && (/^\d+(\.\d+){3}$/.test(host) || host.includes(":") || host.startsWith("["))) return "ip";
  if (typeof window.PublicKeyCredential === "undefined") return "unsupported";
  return null;
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

/** The PRF salt the SDK's passkey wallets use (sdk-ts/src/passkey.ts `PRF_SALT`): the same salt derives the same key. */
const PRF_SALT = new TextEncoder().encode("blockula/wallet/v1");

/** Why the passkey check before a spend didn't pass. Nothing was signed. */
export class PasskeyCheckError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PasskeyCheckError";
  }
}

const sameBytes = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((x, i) => x === b[i]);

/**
 * Ask the passkey again right before a spend, even though the wallet is unlocked: a fresh
 * assertion with user verification required (Face ID, Touch ID or the device PIN), for this
 * wallet's credential only, over a fresh random challenge. It passes only if the answer comes from
 * that credential, says the user was verified, and carries a PRF output that derives this very key
 * again. The last check is the one that can't be faked: only the passkey that unlocked the wallet
 * produces that output. Returns the re-derived wallet, which signs the spend.
 */
export async function reverifyPasskey(wallet: Wallet, hint: Hint): Promise<Wallet> {
  const id = wallet.credentialId ?? (hint.credentialId ? fromB64url(hint.credentialId) : undefined);
  let cred: PublicKeyCredential | null;
  try {
    cred = (await navigator.credentials.get({
      publicKey: {
        rpId: walletRpId(),
        challenge: crypto.getRandomValues(new Uint8Array(32)),
        userVerification: "required",
        allowCredentials: id ? [{ type: "public-key", id: id as BufferSource }] : [],
        extensions: { prf: { eval: { first: PRF_SALT } } } as AuthenticationExtensionsClientInputs,
      },
    })) as PublicKeyCredential | null;
  } catch (e) {
    if (e instanceof Error && (e.name === "NotAllowedError" || e.name === "AbortError")) throw new PasskeyCheckError(CHECK_CANCELLED);
    throw e;
  }
  if (!cred) throw new PasskeyCheckError(CHECK_CANCELLED);
  if (id && !sameBytes(new Uint8Array(cred.rawId), id)) throw new PasskeyCheckError("That's a different passkey from the one this wallet opened with, so nothing was signed.");
  // authenticatorData: the rpId hash (32 bytes), then the flags byte, whose bit 2 is "user verified".
  const data = new Uint8Array((cred.response as AuthenticatorAssertionResponse).authenticatorData);
  if (data.length < 33 || !(data[32] & 0x04)) throw new PasskeyCheckError("Your device didn't confirm it's you (Face ID, fingerprint or PIN), so nothing was signed.");
  const prf = (cred.getClientExtensionResults() as { prf?: { results?: { first?: ArrayBuffer } } }).prf?.results?.first;
  if (!prf) throw new PasskeyCheckError("Your passkey didn't answer the way it did when you unlocked, so nothing was signed.");
  const fresh = Wallet.fromPasskeyPrf(new Uint8Array(prf), wallet.client);
  if (fresh.keyId !== wallet.keyId) throw new PasskeyCheckError("That passkey doesn't open this wallet, so nothing was signed.");
  fresh.credentialId = new Uint8Array(cred.rawId);
  return fresh;
}

export function createLocalKey(client: PinnedClient) {
  if (!localKeysAllowed(client.pin.chainId)) throw new Error("Browser-only keys are disabled on mainnet.");
  const wallet = Wallet.generate(client);
  return { wallet, hint: { v: 1, kind: "local", keyId: wallet.keyId, seed: wallet.seedHex } satisfies Hint };
}

/**
 * A browser-only key from its stored hint. Its seed sits in localStorage, where any script on
 * this origin can read it, so it never opens on mainnet: the rule every unlock path shares (the
 * wallet's Unlock, and the explorer's inline one; audit main13-signing-2).
 */
export function openLocalKey(client: PinnedClient, hint: Hint) {
  if (!localKeysAllowed(client.pin.chainId)) throw new Error("Browser-only keys are disabled on mainnet. Switch to a test network to open this one.");
  if (hint.kind !== "local" || !hint.seed) throw new Error("This isn't a browser-only key.");
  return Wallet.fromSeed(hint.seed, client);
}

/**
 * The account `hint` acts for, if it's an address on this chain. The hint isn't kept per chain, so
 * one saved on another (a testnet relaunched with a new prefix: `boo1…` before `tboo1…`) names an
 * account this chain refuses, down to the faucet: `undefined`, and the key's accounts are looked up
 * here again.
 */
export async function hintAccount(client: PinnedClient, hint: Hint): Promise<string | undefined> {
  return hint.account && isAddress(hint.account, await client.addressPrefix()) ? hint.account : undefined;
}

export type AccountChoice ={ address: string; name: string | null; short: string; role: string; label: string; balance: string };

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
