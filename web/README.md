# Blockula wallet (web)

The passkey wallet, and an explorer, for a chain you [made](../docs/CREATE.md). A passkey unlocks an ML-DSA-65 key that lives only in the tab's memory; the account is a list of owner and session keys on-chain (see the root README).

Every node serves it at `/wallet/` (`http://localhost:7070/wallet/`): the binary embeds a static export of this app, so a friend needs only the link. Its node is the page's origin, its passkey domain the page's host name, its chain the one that node reports (pinned on first use, as for a custom node), and it has no network picker.

```sh
npm run build:wallet                # the served build: SDK, then the static export into web/out
cargo build --release --locked --manifest-path node/Cargo.toml   # embeds web/out (node/build.rs)
npm run wallet                      # or: this app as its own server for the node on this machine, http://localhost:3000
```

The served build (`BLOCKULA_SERVED=1`, `next.config.ts`) is `output: "export"` under `basePath: "/wallet"`: no proxy.ts and no `headers()`, since no Next server runs it; the node sets the headers and a CSP that hashes each page's inline scripts (node/src/wallet.rs). Code that differs reads `SERVED`, `BASE_PATH`, `servedConfig()` and `appPath()` from `lib/config.ts`. A static export can't list every id, so each dynamic route (`app/tx/[id]` and the rest) exports one page under the placeholder `_`, the node serves it for every id, and the page reads the id with `useRouteParam` (`lib/route-param.ts`), never `useParams`. A new dynamic route needs the same split: a server `page.tsx` with `export const generateStaticParams = servedStaticParams("id")` (a function only in the served build: one returning `[]` would make the route static elsewhere) and a client `view.tsx` (and its entry in `scripts/wallet-built.mjs`). `E2E_SERVED=1 npm run test:e2e -w @blockula/web` adds the e2e of a real node serving it (`e2e/served.spec.ts`).

`npm run wallet` binds passkeys to `localhost`, so it works only on the machine that runs it. Friends use the node's `/wallet/` (over HTTPS for passkeys: `tailscale serve`), or see [docs/BUILD-ON-IT.md, "Give friends a link"](../docs/BUILD-ON-IT.md#5-give-friends-a-link) to host this app yourself.

```sh
npm run build -w @blockula/sdk      # the wallet imports the SDK's build
npm run dev -w @blockula/web        # http://localhost:3000, against a local node by default
npm run build -w @blockula/web      # production build
npm run start -w @blockula/web      # serve the production build on port 3000
npm run test:e2e -w @blockula/web   # Playwright (needs `npx playwright install chromium` once)
npm run test:vectors -w @blockula/sdk
```

## Configuration

Everything is a build-time `NEXT_PUBLIC_*` variable: set them in the environment of `npm run build`, or in `web/.env.production.local` (not committed), and rebuild after changing them.

### Your chain

With none of these set, the wallet starts on a node on this machine (`http://localhost:7070`, the **Local node** network). To build it for your chain, set these. The wallet lists your chain as its **Testnet** network (the slot the retired public testnet used) and shows the chain's name beside it.

| Variable | Default | |
|---|---|---|
| `NEXT_PUBLIC_RP_ID` | `testnet.blockula.app` | The passkey relying-party id: the hostname the wallet is served from (no scheme, no port). Passkeys, and so wallets, are bound to it for good. Payment links point at this host. `localhost` for development |
| `NEXT_PUBLIC_BLOCKULA_RPC` | (unset) | Your node's URL, `https://` for a hosted wallet: adds the network and makes it the default |
| `NEXT_PUBLIC_TESTNET_CHAIN_ID` | `blockula-testnet-2` | Your chain id: the node must report it |
| `NEXT_PUBLIC_TESTNET_GENESIS_HASH` | the retired testnet's | Your chain's genesis hash: the node must report it |
| `NEXT_PUBLIC_TESTNET_PREFIX` | `tboo` | The address prefix: `boo` for a chain made with `blockula create` or `summon` |
| `NEXT_PUBLIC_BLACKJACK_TABLE` | (unset) | The blackjack table's program address (`boo1…`) on your chain: `/blackjack` plays at it, and the Programs page links it. Unset, `/blackjack` says there's no table unless a link names one (`/blackjack?table=boo1…`). A value that isn't an address fails the build. A bet there is two transactions, each signed with the player's passkey: the bet with only the hash of a seed the browser made and keeps in `localStorage` (`blockula.blackjack.<chain id>.<table>`), then `reveal_seed`, which the page presents by itself once the bet is in a block, and again after a reload while the hand still waits for its seed. A hand whose seed isn't in the browser (another device, cleared storage) can't be revealed from it, the page says so, and at the seed's deadline the bet goes to the house |
| `NEXT_PUBLIC_BLACKJACK_ORIGIN` | `https://` + `NEXT_PUBLIC_RP_ID` | The public https origin in the blackjack share page's card (`/blackjack/share`: X's player card, whose player is `/blackjack/embed`). The served wallet (`npm run build:wallet`) has no default, so its share page carries no card unless this is set |
| `NEXT_PUBLIC_TESTNET_WITNESSES` | (unset) | Comma-separated `https://` URLs of other nodes of your chain. Names that end up in a signature are proven against them first (audit gap-7-1); without any, a name is the selected node's word and the wallet says so. The blackjack table page also sends each seed reveal (`reveal_seed`) through the selected node and every witness at once, so one node can't keep it out of the chain; without witnesses it goes through the one node, and the page says so. |

The values come from `~/.blockula/<chain id>/README.md` or `curl -s <node>/api/status` (`chain_id`, `genesis_hash`, `address_prefix`).

### The project site

`NEXT_PUBLIC_SITE=project` builds Blockula's own landing page (https://blockula.app) instead of a wallet: `/` is the project's page (`components/site/`), and every other route redirects to it (`proxy.ts`). It has no network picker, no wallet and no chain gate. It shows a live demo chain, opens any node's own wallet (`<node>/wallet/`) from an address a visitor pastes, lists the chains in the repo's chain directory ("Join a chain", `components/site/Chains.tsx`), and has the create-a-chain and contributor prompts. It follows the system's light or dark (`app/site.css`, scoped to `html[data-site]`, so the wallet builds look as before).

| Variable | Default | |
|---|---|---|
| `NEXT_PUBLIC_SITE` | (unset) | `project` makes the project site. Any other value fails the build, and so does combining it with the served build or `NEXT_PUBLIC_NETWORK=mainnet` |
| `BLOCKULA_CHAINS_DIR` | `../chains` | The chain directory the site lists. `next.config.ts` reads its `<chain id>.json` files at build time, checks them with `lib/chains.ts` (the rules `scripts/check-chains.mjs` runs in CI; a bad listing fails the build) and hands them to the page as `BLOCKULA_SITE_CHAINS`. Each listed `rpc` origin joins the CSP's `connect-src`, so a new listing needs a new deploy. The e2e builds point it at `e2e/fixtures/` |
| `NEXT_PUBLIC_DEMO_NODE` | (unset) | The demo chain's node: `https://` (plain `http://` only for this machine or a private network, as for the connect field). The page reads its `/api/status` and `/api/blocks` from the browser (nodes send permissive CORS), shows its name, height, validators and newest blocks, links its wallet (`<node>/wallet/`), explorer and invite page, and gives the agent prompt pinned to it. The CSP's `connect-src` allows this origin and the listed chains' nodes, nothing else. Unset, or not answering, the demo section says the demo is resting and points at creating your own. A malformed value fails the build |

```sh
NEXT_PUBLIC_SITE=project NEXT_PUBLIC_DEMO_NODE=https://demo.example npm run build -w @blockula/web
```

The demo node must serve the wallet over HTTPS on a hostname of its own (passkeys are bound to it): `tailscale funnel 7070`, or a VPS behind Caddy ([docs/BUILD-ON-IT.md, "Put it online"](../docs/BUILD-ON-IT.md#4-put-it-online)). `e2e/site.spec.ts` and `e2e/site-nodemo.spec.ts` test both builds against a mocked demo node.

**The chain directory.** Each card reads its node's `/api/status` and `/api/validators` from the browser every half minute: live, resting (not answering), or refused when the node reports another chain id or genesis hash than its listing (no wallet button then). A chain with `validators_wanted` gets **Become a validator**: the steps of [docs/RUN-A-NODE.md, "Validate someone else's chain"](../docs/RUN-A-NODE.md#validate-someone-elses-chain) with its node, genesis hash and home filled in, and the way in its set needs (admission, approval or stake). With no listings it shows how to create and list one. Listing rules: [chains/README.md](../chains/README.md). `e2e/site-chains.spec.ts` tests it against the fixture listings in `e2e/fixtures/chains/`.

### Mainnet build mode (unused)

Left from the mainnet that was planned and dropped in October 2026 ([docs/README.md, "Design history"](../docs/README.md#design-history)). No chain uses it.

| Variable | Default | |
|---|---|---|
| `NEXT_PUBLIC_NETWORK` | (unset) | `mainnet` makes a mainnet deployment: mainnet is the default network, browser-only keys are off, and the build fails unless `NEXT_PUBLIC_RP_ID`, `NEXT_PUBLIC_MAINNET_RPC` and `NEXT_PUBLIC_MAINNET_GENESIS_HASH` are set and the rpId isn't on a shared hosting domain (`*.vercel.app`, `*.pages.dev`, …). |
| `NEXT_PUBLIC_MAINNET_RPC` | (unset) | Adds the Mainnet network (chain `blockula-1`, prefix `boo`). Hidden until set. |
| `NEXT_PUBLIC_MAINNET_GENESIS_HASH` | (unset) | Mainnet's genesis hash (`scripts/pin-network.sh blockula-1 … --mainnet` would build it into `lib/config.ts`; a mainnet build then refuses any other value here). |
| `NEXT_PUBLIC_MAINNET_WITNESSES` | (unset) | Comma-separated `https://` URLs of other mainnet nodes, as above. |

Witness URLs must be `https://` (or `http://localhost` / `http://127.0.0.1` for development), are deduplicated, and the selected RPC itself is dropped from the list; a malformed one fails the build. The CSP's `connect-src` allows every configured node and witness (`lib/csp.ts`).

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
- With no FANG for the fee (on a chain without a faucet), the kit's key id waits in the hint and Home and Devices offer to activate it later.
- **Restore** (`/recover`): type the words → the wallet finds the accounts that key owns (`/api/key/{key_id}`) → creates a passkey on this device → the kit key signs one `add_key` for it → the new passkey takes over. It then offers to revoke the lost device and recommends replacing the kit, whose words have now been typed into a browser.

## Shared accounts (multisig)

`/shared` (the header's **Shared**) makes the account this wallet acts for an M-of-N of its owner keys, the chain's native multisig ([docs/GOVERNANCE.md](../docs/GOVERNANCE.md#multisig-accounts)), so people and AI agents can hold one account together. Rules and wording are in `lib/multisig.ts`, the screens in `components/shared.tsx`.

- **Owners are the account's owner keys**, exactly: every device, every recovery kit, every co-owner. A co-owner is added from a grant code, their key's consent to this account (**Join a shared account** on their own wallet's Shared page, or an agent's `use_account`). A key id alone can't be added, and a name is never resolved to keys. Until the account is shared, an added owner key has full control, and the page says so.
- **Making it shared** asks for the threshold (1 to the number of owner keys; at least two owner keys), shows what the chain's rules mean (it can't be undone, owner keys then only propose and approve, what losing keys does, how long a proposal waits, that agents with a budget keep sending), shows what will be signed (`describeAction`) and signs `multisig_create` after a tick and a fresh passkey check.
- **Proposing** reuses the explorer's action forms (`lib/explorer/programs.ts`), wrapped in `multisig_propose` (`asProposal`): a payment, adding an owner (from a grant code), removing a key, a new threshold, names, staking and the rest, each with the usual preview and preflight.
- **Reviewing and approving** is `/shared/review?account=<address>&id=<n>&digest=<digest>`, the co-sign link each open proposal offers. The page renders the stored action with the SDK's `reviewAction`, never the node's `summary`; counts approvals itself from the owner keys; shows the digest an approval binds; refuses to offer an approval when the action can't be read, expired, or isn't the one the link's digest names; and signs `multisig_approve` for `actionDigest(action)` with the passkey. A link is anyone's text: only an address, a number and a digest are read from it.
- **One key, several accounts.** The page lists the other accounts this device's key is on and switches the wallet to one (as the unlock screen's account picker does), so a person keeps their own account and co-owns another.
- Home says when the account is shared (its owner keys can't send directly any more), the explorer's account page links each open action to its review page, and a multisig transaction's receipt links back to the account.

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
| `paylink.spec.ts` | Receive links carry their chain and the canonical amount (a zero-padded one still fits a QR code); an absurd amount is refused without crashing; Send fills a link made for this chain, refuses one made for another chain or instance, refuses a malformed chain or genesis with fixed words, names only networks it knows, and fills an older link with a notice |
| `governance.spec.ts` | Proposals with tallies, quorum and council votes; voting with conviction and changing the vote; releasing locks; hostile on-chain titles and links rendered inert; a node without governance |
| `site.spec.ts`, `site-chains.spec.ts`, `site-nodemo.spec.ts` | The project site (`NEXT_PUBLIC_SITE=project`, two more builds): the demo chain's live stats and links, a demo that's unreachable, answers garbage or isn't configured, the connect field's checks, wallet routes redirected home, the chain directory's cards (live, resting, another chain), the validator steps and their copy buttons, its empty state, `connect-src` holding the demo node and the listed nodes only, phone layout in light and dark |
| `shared.spec.ts` | Shared accounts (multisig): adding a co-owner's key from its grant code, the warning and threshold before `multisig_create`; proposing a payment (`multisig_propose`) and its co-sign link; reviewing a co-owner's proposal from the action (a lying node summary never shows) and approving it with the digest bound, which runs it at the threshold; a link whose digest isn't the stored action's, a malformed link, another network's; switching to another account the key co-owns; a key that isn't an owner; this key's grant code for someone else's account |
| `explorer.spec.ts` | The node's built-in explorer (`node/src/explorer.html`) on a mocked protocol v1 API: validator pools, Governance and proposal pages, parameters, name expiry, escaping, a dropped tx (not final, nothing charged) apart from one that failed in a block (its fee charged) |

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
