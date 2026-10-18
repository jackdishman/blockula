// Non-secret hints kept in localStorage so we know which passkey/account to unlock on reload.
// The passkey's PRF output, the derived seed and recovery-kit words are NEVER stored. The one
// exception is the explicitly-labeled "browser-only key" fallback, whose seed lives here by
// design (test chains only; disabled on mainnet).

export type WalletKind = "passkey" | "local";

export interface Hint {
  v: 1;
  kind: WalletKind;
  /** base64url WebAuthn credential id, so unlock can target the right passkey. */
  credentialId?: string;
  /** This device's key id (not secret: it's a hash of the public key). */
  keyId: string;
  /** The account this key acts for. Missing while a new device waits to be linked. */
  account?: string;
  name?: string;
  /** Browser-only fallback key. Less secure: anything with access to this browser profile can read it. */
  seed?: string;
  /**
   * A recovery kit the user wrote down that isn't on the account yet (no FANG for the fee): its key
   * id and its grant code (the kit key's public consent to joining the account, which adding it
   * needs). Never the words.
   */
  pendingKit?: { keyId: string; created: string; grant?: string };
}

const HINT = "blockula.wallet";
const TUTORIAL = "blockula.tutorial";

const read = <T,>(k: string): T | null => {
  try {
    const raw = localStorage.getItem(k);
    return raw ? (JSON.parse(raw) as T) : null;
  } catch {
    return null;
  }
};

export const loadHint = () => read<Hint>(HINT);
export const saveHint = (h: Hint) => localStorage.setItem(HINT, JSON.stringify(h));
export const clearHint = () => localStorage.removeItem(HINT);

export interface TutorialState {
  dismissed?: boolean;
  viewedExplorer?: boolean;
  /** Items ticked once stay ticked (e.g. after revoking the agent you set up). */
  done?: string[];
}

/**
 * Checklist progress is kept per chain and account: the same key has the same address on every
 * chain, but what it did on a testnet that has since been relaunched (or with another account)
 * isn't done here. A store from before this (no `v`) can't say which chain it was for, so it's dropped.
 */
interface TutorialStore {
  v: 2;
  scopes: Record<string, TutorialState>;
}

/** Whose checklist: the chain (id and genesis, as the node reported them to the pinned client) and the account. */
export const tutorialScope = (chainId: string, genesisHash: string | undefined, account: string) => `${chainId}/${genesisHash ?? ""}/${account}`;

const tutorials = (): Record<string, TutorialState> => {
  const s = read<TutorialStore>(TUTORIAL);
  return s?.v === 2 && s.scopes ? s.scopes : {};
};
export const loadTutorial = (scope: string): TutorialState => tutorials()[scope] ?? {};
export const saveTutorial = (scope: string, t: TutorialState) => localStorage.setItem(TUTORIAL, JSON.stringify({ v: 2, scopes: { ...tutorials(), [scope]: t } } satisfies TutorialStore));
export const markTutorial = (scope: string, patch: Partial<TutorialState>) => saveTutorial(scope, { ...loadTutorial(scope), ...patch });

export const ONBOARDED = "blockula.onboarded";

export function b64url(bytes: Uint8Array) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function fromB64url(s: string) {
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4));
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}
