# Blockula wallet (web)

The passkey wallet at `https://<RP_ID>`. A passkey unlocks an ML-DSA-65 key that lives only in the tab's memory; the account is a list of owner and session keys on-chain (see the root README).

```sh
npm run build -w @blockula/sdk      # the wallet imports the SDK's build
npm run dev -w @blockula/web        # http://localhost:3000, against a local node by default
npm run build -w @blockula/web      # production build
npm run test:e2e -w @blockula/web   # Playwright (needs `npx playwright install chromium` once)
npm run test:vectors -w @blockula/sdk
```

## Configuration

Everything is a build-time `NEXT_PUBLIC_*` variable.

| Variable | Default | |
|---|---|---|
| `NEXT_PUBLIC_NETWORK` | (unset) | `mainnet` makes a mainnet deployment: mainnet is the default network, browser-only keys are off, and the build fails unless the three variables below are set and the rpId isn't on a shared hosting domain (`*.vercel.app`, `*.pages.dev`, …). |
| `NEXT_PUBLIC_RP_ID` | `blockula-wallet.vercel.app` | The passkey relying-party id. Passkeys, and so wallets, are bound to it for good. Mainnet needs a domain the project owns (DEC-8). |
| `NEXT_PUBLIC_MAINNET_RPC` | (unset) | Adds the Mainnet network (chain `blockula-1`, prefix `boo`). Hidden until set. |
| `NEXT_PUBLIC_MAINNET_GENESIS_HASH` | (unset) | Mainnet's genesis hash. Hard-code it in `lib/config.ts` once the ceremony has produced it. |
| `NEXT_PUBLIC_BLOCKULA_RPC` | `https://blockula-testnet.fly.dev` | The testnet's RPC. On a mainnet deployment, the testnet is listed only if this is set. |
| `NEXT_PUBLIC_TESTNET_CHAIN_ID` | `blockula-testnet` | The chain id the testnet node must report. |
| `NEXT_PUBLIC_TESTNET_GENESIS_HASH` | (unset) | Pins the testnet's genesis too. |
| `NEXT_PUBLIC_TESTNET_PREFIX` | `boo` | The testnet's address prefix. Protocol v1 testnets use `tboo` (DEC-7). |

## Stake and Governance (protocol v1)

- **Stake** (`/stake`): what the account has staked and unbonding, each pool (value, shares, commission), unstaking an amount or everything, withdrawing what has finished unbonding, and delegating to a validator of the active set. An amount to unstake is signed as an amount, and the chain converts it to pool shares at its own price (audit gap-7-5), so the node's report of the stake never decides how much unbonds.
- **Governance** (`/governance`): open and earlier proposals with their token tallies, quorum, bootstrap-council votes and timelock; voting aye or nay with a conviction (its weight and lock time shown before signing); releasing expired vote locks.
- Both use the SDK helpers (`wallet.delegate`, `wallet.vote`, …) through the pinned client, read the module endpoints tolerantly (`stakeSummary`, `proposalSummary`), and say so plainly on a node that doesn't serve them yet. Text and links from the chain render as text; only `https://` links become links.

## Which chain am I on? (WAL-4)

- Each network in `lib/config.ts` pins a chain id, and optionally a genesis hash and address prefix. Every `/api/status` the wallet reads goes through `PinnedClient` (`lib/chain.ts`), including the one `Wallet.build` signs against, so the wallet can't sign for a chain it isn't showing.
- If the node reports another chain id, genesis or prefix, the wallet drops the key from memory and shows a blocking screen until you switch networks or reload. A node claiming a chain whose genesis is pinned (say `blockula-1`) must match it, whatever network it was picked as.
- Local and custom nodes have no configured chain. The wallet pins the first chain they report for the session, and remembers a custom node's chain across visits (trust on first use), so a later switch is caught.
- The header stamps the network and chain id on every screen: plain for mainnet, a red stamp for anything else, solid red for a custom node.

## Origin security (WAL-3)

- `proxy.ts` sets a per-request nonce CSP on every page: `script-src 'self' 'nonce-…' 'strict-dynamic'`, `style-src 'self' 'nonce-…'`, `connect-src 'self'` plus the configured nodes (and a custom node, named by the `blockula_rpc` cookie and accepted only as a plain `https?://host[:port]` origin), `img-src 'self' data:`, `font-src 'self'`, `frame-ancestors 'none'`, `base-uri 'none'`, `form-action 'self'`, `object-src 'none'`. Pages render per request so the nonce is fresh. `next dev` adds `'unsafe-eval'` and inline styles, which React's dev tooling needs.
- `experimental.sri` puts `sha384` integrity hashes on the framework and main chunks. Next.js 16 (Turbopack) still emits route chunks without them; those load only from this origin under the nonce. Fonts are self-hosted (`next/font` downloads them at build time). There are no third-party scripts.
- `next.config.ts` sets HSTS, `nosniff`, `Referrer-Policy: no-referrer`, a `Permissions-Policy` that allows WebAuthn on this origin only, `Cross-Origin-Opener-Policy: same-origin`, `Cross-Origin-Resource-Policy: same-origin` and `X-Frame-Options: DENY` on every route.
- Not covered by code: production deploys should come from a separate, locked-down project that requires review, and the wallet belongs in the audit scope.

## Passkeys and PRF (WAL-1)

- `@blockula/sdk/passkey` creates a discoverable passkey (`residentKey` and `userVerification` required) and evaluates the WebAuthn **PRF** extension with the fixed salt `blockula/wallet/v1`. The 32-byte output goes through BLAKE3 `derive_key("blockula passkey wallet v1")` to make the ML-DSA-65 seed. The passkey's own signature is never used on-chain.
- The same passkey gives the same wallet everywhere it syncs (iCloud Keychain, Google Password Manager, 1Password). So every device holding that passkey holds the wallet key, and losing the passkey provider loses the key: hence recovery kits.
- Some providers return the PRF output at creation, others only report `prf.enabled` and evaluate it on the next `get()`. The SDK handles both: the second kind shows a second prompt.
- **Without PRF**, creating or unlocking fails after one prompt with "This passkey provider doesn't support the PRF extension". The wallet then explains PRF and suggests providers that support it. On test chains it also offers a browser-only key (a random seed in `localStorage`, labeled as such on every screen); on mainnet it never does, and won't open one left in storage.
- Derivation vectors (PRF output → seed → key id → address) are in `sdk-ts/test/passkey-vectors.json`, cross-checked against the node's own crates. The SDK test checks the derivation, the salt and the WebAuthn options against a simulated authenticator; `e2e/passkey.spec.ts` checks the wallet derives the same addresses in a real browser.

## Recovery kits (WAL-2)

- A kit is an offline ML-DSA-65 key made in the browser and added to the account as an owner key labeled `recovery kit`. Onboarding offers one right after the faucet step; Devices & agents → Recovery kit makes, lists and revokes them.
- The 32-byte seed is written as **24 words**: the BIP39 English wordlist (vendored in `lib/bip39-english.ts`, hash-checked in the tests) and BIP39's checksum, so a mistyped or swapped word is caught before anything happens. Only the encoding is BIP39: there's no passphrase, PBKDF2 or BIP32, so the words open nothing in other wallets. We chose words over hex because people copy 24 words onto paper far more reliably than 64 hex digits, and the checksum turns most mistakes into an immediate error. Restore accepts numbering, any case and four-letter abbreviations.
- The kit page shows the words, account, network and chain id, kit key id, date and restore steps, and prints on its own (print stylesheet). It can also be downloaded as text, with a warning that downloads end up in cloud backups. The user confirms two random words before the key is added. Only the key id is used to add it; the seed is wiped as soon as the words exist, and the words are dropped once confirmed. Nothing secret is stored or sent.
- With no FANG for the fee (mainnet has no faucet), the kit's key id waits in the hint and Home and Devices offer to activate it later.
- **Restore** (`/recover`): type the words → the wallet finds the accounts that key owns (`/api/key/{key_id}`) → creates a passkey on this device → the kit key signs one `add_key` for it → the new passkey takes over. It then offers to revoke the lost device and recommends replacing the kit, whose words have now been typed into a browser.

## Tests

`npm run test:e2e -w @blockula/web` builds the wallet twice into `.next-e2e/` (a testnet build and a `NEXT_PUBLIC_NETWORK=mainnet` build), starts both, and runs Chromium with a CDP virtual authenticator (`ctap2`, internal transport, resident keys, user verification, `hasPrf: true`). The node is a mock behind Playwright routes (`e2e/mock-node.ts`) that checks ML-DSA signatures over the SDK's signing bytes, key authorization, nonces and balances. `E2E_DEV=1` runs against `next dev`.

| Spec | Covers |
|---|---|
| `passkey.spec.ts` | Create a wallet, same address after reload, lock/unlock, open from the passkey alone, the SDK vectors in the browser, no-PRF providers |
| `network.spec.ts` | Network badge everywhere, refusing another chain id, genesis or prefix, a node switching chains mid-session |
| `security.spec.ts` | CSP and headers on every page, fresh nonces, nonce'd and SRI'd scripts only, the custom-node cookie, zero CSP violations through onboarding, injected script and exfiltration blocked |
| `recovery.spec.ts` | Kit creation (print, download, confirmation, activation), restore on a second device and revoking the lost one, mistyped kits, pending kits on a chain without a faucet, no secret in requests, storage or the page |
| `recovery-phrase.spec.ts` | BIP39 256-bit vectors, wordlist hash, round trips, abbreviations, error messages |
| `mainnet.spec.ts` | Mainnet default and badge, pinned genesis, no browser-only keys, the build refusing a borrowed rpId or missing pins, a footer that doesn't call mainnet a test chain |
| `stake.spec.ts` | Delegate, unstake an amount and the rest, withdraw once unbonded (each tx checked by the mock node), no CSP violations, a node lying about the stake's value or shares (the amount is signed), a node without staking |
| `paylink.spec.ts` | Receive links carry their chain; an absurd amount is refused without crashing; Send fills a link made for this chain, refuses one made for another chain or instance, and fills an older link with a notice |
| `governance.spec.ts` | Proposals with tallies, quorum and council votes; voting with conviction and changing the vote; releasing locks; hostile on-chain titles and links rendered inert; a node without governance |
| `explorer.spec.ts` | The node's built-in explorer (`node/src/explorer.html`) on a mocked protocol v1 API: validator pools, Governance and proposal pages, parameters, name expiry, escaping |

## Manual release test matrix

A virtual authenticator isn't a real one: PRF support, sync and prompts differ by platform. Before each release, run this on the release build (on its real domain) and record the result here or in the release notes.

For each row: **create** a wallet; **reload and unlock**; **second device** (open the wallet where the passkey syncs, expect the same address); **kit** (make, print, confirm); **restore** (on a device without the passkey); **no-PRF message** where relevant.

| Platform | Passkey provider | Create | Unlock | Second device | Kit | Restore | Notes (version, date, tester) |
|---|---|---|---|---|---|---|---|
| iOS Safari | iCloud Keychain | | | | | | |
| macOS Safari | iCloud Keychain | | | | | | |
| Chrome, macOS | Google Password Manager | | | | | | |
| Chrome, macOS | iCloud Keychain | | | | | | |
| Chrome, Android | Google Password Manager | | | | | | |
| Chrome, Windows | Windows Hello | | | | | | |
| Edge, Windows | Windows Hello | | | | | | |
| Any desktop browser | 1Password | | | | | | |
| Chrome, any desktop | Hardware key (YubiKey 5, USB and NFC) | | | | | | |
| Chrome, desktop → phone | Cross-device (QR / hybrid) | | | | | | |
| Firefox, desktop | Platform or 1Password | | | | | | |

Also check: a provider without PRF shows the PRF explanation (and, on mainnet, no browser-only key); a passkey made on a preview deployment is refused; the header's network stamp is right on each network.
