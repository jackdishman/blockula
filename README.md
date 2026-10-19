# 🧛 Blockula

**The post-quantum blockchain that never dies.** Wallets unlocked by passkeys, human-readable `.boo` names, an explorer that explains itself, and an SDK + MCP server so any coding agent can use it, or run its own copy, from one prompt.

Quantum entanglement is literally "spooky action at a distance". We took that personally.

| | |
|---|---|
| Token | **FANG** (1 FANG = 10⁹ drops 🩸) |
| Signatures | **ML-DSA-65** (NIST FIPS 204), no elliptic curves on-chain |
| Hashing | BLAKE3-256 |
| Network | Validators key their channels with hybrid X25519 + **ML-KEM-768** (FIPS 203) and authenticate with ML-DSA-65 |
| Addresses | `boo1…` = bech32m(BLAKE3(public key)), or just `yourname.boo` |
| Privacy (Phase 2) | 👻 **Ghost Mode** shielded transfers + 🪦 **The Crypt** encrypted storage, built on hash-based STARKs + ML-KEM |

## Try it now (no install)

| | |
|---|---|
| 👛 Wallet | **https://blockula-wallet.vercel.app**: passkey sign-up, free test FANG, claim a `.boo` name |
| 🔎 Explorer + API | **https://blockula-testnet.fly.dev**: the public testnet. 4 validators across the US and Europe, ~1s finality |
| 🤖 Agents | `claude mcp add blockula -- npx -y @blockula/mcp` uses the public testnet automatically |

What it is, on one page: [docs/ONE_PAGER.md](docs/ONE_PAGER.md).

**It's a testnet.** Testnet FANG, Hunts and Soul have no monetary value and come with no promise of any mainnet allocation.

## npm packages

```bash
npm install @blockula/sdk                         # SDK for apps, scripts and browsers
claude mcp add blockula -- npx -y @blockula/mcp   # MCP server for coding agents
```

| Package | Install | What it is |
|---|---|---|
| [`@blockula/sdk`](https://www.npmjs.com/package/@blockula/sdk) | `npm install @blockula/sdk` | TypeScript SDK: `Wallet` (ML-DSA-65 signing), `Client` (reads), `.boo` names, session keys, Hunts. Passkey wallets for the browser live at `@blockula/sdk/passkey`. |
| [`@blockula/mcp`](https://www.npmjs.com/package/@blockula/mcp) | `npx -y @blockula/mcp`, or `npm install -g @blockula/mcp` for a `blockula-mcp` binary | MCP server so any coding agent can hold a wallet, send FANG, claim names and list Hunts. |

Both need Node 20 or newer. The MCP server uses the public testnet unless its configuration names another network (`--network local` for `blockula dev`; see the [MCP README](mcp/README.md)), and it never switches networks on its own. The SDK's `Client` defaults to `http://localhost:7070`, so pass the testnet explicitly: `new Client("https://blockula-testnet.fly.dev")`.

## Install the node (no Rust needed)

```bash
curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
blockula dev          # local chain + explorer at http://localhost:7070
```

Prebuilt for macOS (Apple Silicon and Intel) and Linux (x64 and arm64). The script checks the version, the SHA-256 checksum, build provenance (with a logged-in `gh`) and the offline release signature (with an installed `blockula`). It's still the release's own script, though, so validators don't use it: they install and upgrade with `blockula release install vX.Y.Z`, which checks the release against the release key id they pinned at their first install and never runs the release's scripts ([OPERATIONS.md "Installing and upgrading"](docs/OPERATIONS.md#installing-and-upgrading)). Agents can call the MCP's `install_node` tool instead.

## Contribute

> Testnet FANG, Hunts and Soul have no monetary value and come with no promise of any mainnet allocation.

Open **Hunts** are [issues labeled `hunt`](https://github.com/jackdishman/blockula/issues?q=is%3Aopen+label%3Ahunt), each with acceptance criteria and a testnet FANG bounty. Agents are welcome as Familiars. See [CONTRIBUTING.md](CONTRIBUTING.md).

Bounties live on-chain: the FANG is escrowed in a Hunt, anyone can add to it, and only its creator awards it for the merged PR (nobody else can move the escrow; governance can only refund the funders). Browse them in the explorer's Hunts page or `GET /api/hunts`.

```ts
await wallet.linkGithub("vlad", "<gist id>");  // a public gist by @vlad containing your boo1 address verifies it
const hunt = await wallet.createHunt({ repo: "jackdishman/blockula", issue: 12, amount: "50", title: "Add snapshot sync", expiresInBlocks: 100_000 });
await other.fundHunt(huntId, "10");            // crowdfund; refunded pro-rata if cancelled
await wallet.awardHunt(huntId, "vlad.boo", 34, familiarKeyId);  // optional: credit vlad's agent Familiar
```

Awarding from the CLI checks GitHub first: the PR must be merged into the hunt's repo's default branch, and it pays the account the PR's author linked.

```bash
blockula hunt award 3 --pr 34 --rpc https://blockula-testnet.fly.dev            # signs with ~/.blockula/wallet.json
blockula hunt award 3 --pr 34 --key creator.key --rpc …                         # or the creator's key file
blockula hunt list | show | create | fund | cancel
```

**Beyond Hunts, the contribution pool pays testnet FANG for any merged PR to a pool repo** (Proof of Contribution, [docs/PLAN.md](docs/PLAN.md#-proof-of-contribution-rewarding-useful-work)):
- Claim a merged PR you authored. Validators check it on GitHub.
- A **Séance** judges it: a random jury of people with **Soul** (reputation you can't buy) compares claims in pairs, sealing their votes before revealing them.
- Payouts rate a person's claims in a Séance together (one win rate over all their comparisons) and count each after the first a quarter of the one before (so in expectation volume adds at most a third), and vest over ~90 days with clawback.

```bash
blockula contrib link-github vlad <gist id>       # once
blockula contrib claim jackdishman/blockula 34    # after the PR merges
blockula contrib status                           # the pool, its repos, the open Séance
blockula contrib join-jury                        # jurors need 5 Soul and bond 10 FANG
blockula contrib seance                           # your assigned pairs, with PR links and their digest
blockula contrib vote 2 --votes ab=a --spam 17 --digest <pairs digest>   # seal exactly those pairs, then later:
blockula contrib reveal 2
blockula contrib withdraw                         # vested rewards
```

## Quickstart (from source)

```bash
npm install && npm run build
npm run dev
```

That builds the Rust node, SDK and MCP server, then starts a local chain with a faucet. Open http://localhost:7070 for the explorer. No Rust? Use the public testnet: `new Client("https://blockula-testnet.fly.dev")`.

The node binary also includes a CLI wallet: `blockula wallet new`, then `blockula wallet faucet --rpc https://blockula-testnet.fly.dev`, then `blockula wallet send alice.boo 1 --rpc https://blockula-testnet.fly.dev`.

```ts
import { Client, Wallet } from "@blockula/sdk";

const wallet = Wallet.generate(new Client("https://blockula-testnet.fly.dev"));
await wallet.faucet();                       // 100 test FANG
await wallet.registerName("vlad");           // vlad.boo
await wallet.send("alice.boo", "1.5", { memo: "trick or treat" });
```

In the browser, a passkey unlocks the wallet (no seed phrase):

```ts
import { createPasskeyWallet, unlockPasskeyWallet } from "@blockula/sdk/passkey";
const wallet = await createPasskeyWallet({ username: "vlad" });
```

## For agents

```bash
claude mcp add blockula -- npx -y @blockula/mcp
```

Then paste the prompt from [AGENTS.md](AGENTS.md). With no local node running, it connects to the public testnet, so there's nothing else to install. MCP tools: `status`, `tx_status`, `create_wallet`, `use_account`, `wallet_info`, `faucet`, `send`, `check_name`, `register_name`, `lookup`, `latest_blocks`, `list_hunts`, `fund_hunt`, `link_github`, `contribution_pool`, `submit_contribution`, `my_contributions`, plus `install_node`, `start_dev_node` and `summon_chain` (the last two need the `blockula` binary).

## Run your own chain

`summon` creates a genesis plus validator keys; `run` starts a validator. Consensus is
[Commonware](https://commonware.xyz) **simplex BFT** with ML-DSA-65 votes and certificates, so the
same binary runs a 1-validator chain or a multi-validator network. Validators can be added and
removed on-chain without a new genesis; the set changes at **epoch** boundaries.

```bash
# one validator (HTTP on 7070)
blockula summon mychain
blockula run --home ~/.blockula/mychain

# a local 4-validator testnet: ~/.blockula/mynet/v0 … v3, ports picked automatically
blockula summon mynet --validators 4
blockula run --home ~/.blockula/mynet/v0     # repeat for v1, v2, v3 (each prints its URL)

# a full node: serves the API + explorer right away, fast-syncs from the newest certified
# snapshot (add --full to replay from genesis), then follows finalized blocks. It pins the
# genesis hash `summon` printed (required for chains this binary doesn't know)
blockula join --genesis http://127.0.0.1:<v0 http port> --genesis-hash <hash> --home ~/.blockula/mynet-full --port 7171
```

`summon` is for chains you run for yourself: it gives the first validator's account (or each `--admin`) the validator-admin levers of dev and test chains (pin or remove a validator), which mainnet doesn't have.

**On public servers**, build the genesis the way mainnet's is built, with no admin keys (the validators pass proposals as the bootstrap council):
1. Each operator writes its public entry offline: `blockula genesis operator --chain-id pubnet --key validator.key --owner <owner account> --p2p-addr 203.0.113.10:30300 --http-addr https://… --out v0.toml` (the addresses peers will dial).
2. `blockula genesis build --spec spec.toml --out genesis.json` turns a reviewed spec (allocations by bucket with their vesting, the operator files, parameters, reserved names, the contribution pool) into the same `genesis.json` and hash on every machine. `deploy/launch.sh` does both for the public testnet ([deploy/README.md](deploy/README.md)); `deploy/mainnet/genesis-spec.example.toml` is mainnet's shape.
3. Each server runs `blockula run --home <its home, holding genesis.json and its validator.key> --bind 0.0.0.0` (or `--bind ::`).
   - The node listens on the bind address and advertises its genesis addresses.
   - Public addresses use the `recommended` p2p preset; private ones use `local` (override with `--p2p-preset`).

Full nodes: `blockula join --genesis genesis.json --genesis-hash <hash> --home full --bind 0.0.0.0 --port 7171` (`blockula checkpoint --genesis genesis.json` prints the hash). On a chain older than its weak-subjectivity period (half the evidence window: ~10.5 days on mainnet, 3.5 days on the public testnet), also pass a recent `--checkpoint` ([OPERATIONS.md, Weak subjectivity](docs/OPERATIONS.md#weak-subjectivity)).

Mainnet's genesis is built the same way, by independent operators in a ceremony: [docs/GENESIS_CEREMONY.md](docs/GENESIS_CEREMONY.md).

Any node accepts txs, which are gossiped to the leader. On chains with a faucet, any node also accepts faucet requests: genesis validators pay from their own accounts, and other nodes forward the request. A forwarded request is never forwarded again.

Exposed HTTP is hardened:
- 128 KB body limit.
- Every tx is size- and format-checked: 64 KB max, lowercase hex and addresses only, capped string fields.
- Txs: 120 per minute per client.
- Faucet: 1 drip per address per hour, and 1 per client per 10 minutes. IPv6 clients are counted per /64, and quota is only consumed when a drip actually goes out. While 32 drips are already pending, the faucet answers `503 faucet busy`.
- How the client is identified: by default the socket address. Proxy headers are ignored unless you name one with `--client-ip-header` (e.g. `fly-client-ip` behind Fly). A forwarding full node's `X-Blockula-Client-IP` is honored only from `--trusted-forwarder` IPs, or with the shared `BLOCKULA_FORWARDER_SECRET`.
- Mempool: capped at 5,000 txs and 64 per sender. When full, the lowest fee-per-byte tx is evicted. Txs not included within 600 blocks expire. `/api/tx/{id}` reports `failed` with the reason for txs that failed or were dropped.

Operations:
- `/api/health` returns 503 when the head is stale or the node is syncing.
- `/metrics` serves Prometheus metrics.
- `/api/status` shows the version, git sha and genesis hash.
- Every node, full nodes included, serves `/api/sync`, so validators' HTTP needn't be public.
- `--peer-http <url>` (repeatable) replaces the genesis list of HTTP peers used for sync and forwarding.
- `join --genesis-hash <hex>` pins the chain. `summon` prints the hash.
- A validator refuses to start if its home has history but no consensus journal, or if `machine.lock` names another instance (`BLOCKULA_INSTANCE_ID`, or `FLY_MACHINE_ID` on Fly). Pass `--i-know-this-key-is-not-running-elsewhere` to override.
- `--p2p-preset public-nat` is for NATed or proxied hosts such as Fly, where every peer arrives from one private address: recommended limits, except that it accepts private inbound addresses, reconnects to a peer after 2 s instead of 60 s and accepts 16 inbound handshakes a second per address instead of one per 5 s (docs/OPERATIONS.md "Restarts").

Nodes restart where they left off. A validator that was down catches up over HTTP first, then via consensus backfill.
`scripts/local-testnet.sh` runs 4 validators, a full node, a CLI-onboarded 5th validator, the SDK e2e and the CLI wallet against them.

### CLI wallet

```bash
blockula wallet new                         # key in ~/.blockula/wallet.json (mode 600)
blockula wallet faucet --rpc https://<node>
blockula wallet send alice.boo 1.5 --rpc https://<node>
blockula wallet name vlad --rpc https://<node>
blockula wallet balance --rpc https://<node>
```

Every command that signs (`wallet send|name`, `stake`, `gov`, `multisig`, `name`, `hunt`, `contrib`, `validator`) first checks which chain the node at `--rpc` is on, as the MCP does: `--network testnet` (or the public testnet's URL) and `--network mainnet` pin their chain id and prefix, `--chain-id` and `--genesis-hash` (or `BLOCKULA_NETWORK`, `BLOCKULA_CHAIN_ID`, `BLOCKULA_GENESIS_HASH`) pin any node, and whatever isn't pinned is pinned on first use and remembered in `~/.blockula/known-chains.json` (not for a node on this machine). A node on another chain is refused before anything is signed, and mainnet is never assumed: a node that reports `blockula-1` is refused unless you passed `--network mainnet`. Plain `http://` only works for this machine or a private network. Names you send to are resolved by the node, and the CLI prints the address it got.

## Become a validator

Validators are permissionless: anyone can register one with a self-bond, anyone can delegate to it, and at each epoch boundary the top `max_validators` by stake make the set (newcomers at most a third of it). Double-signing is slashed and banned for good; missing too many turns gets a validator jailed until it unjails. Everything is on-chain and visible in the explorer's Validators page.

```bash
# 1. Create a validator home: fetches genesis, generates the ML-DSA-65 key, records your public addresses
blockula validator init --home ~/val --genesis https://<any node> --genesis-hash <the chain's genesis hash> \
  --public-p2p-addr <your-ip>:30300 --public-http-addr <your-ip>:7070
# 2. Fund the printed address with the minimum self-bond plus fees (testnets: the faucet, POST /api/faucet)
# 3. Start the node: it syncs now and votes once it's in the active set
blockula run --home ~/val --bind 0.0.0.0
# 4. Bond and register, signed by the validator's own account. The consensus key also signs a
#    proof of possession, so nobody can register a key they don't hold.
#    (Registering from another owner account? `blockula validator pop --home ~/val --owner boo1…`
#    prints the `pop` field for the SDK's `registerValidator`.)
blockula validator register --home ~/val --rpc https://<any node>
# Stake: delegate, start unbonding, withdraw what's free, see an account's stake
blockula stake delegate boo1… 1000 --key wallet.json --rpc https://<any node>
blockula stake undelegate boo1… --amount 250 --key wallet.json --rpc https://<any node>   # or --all, or --shares N
blockula stake withdraw --key wallet.json --rpc https://<any node>
blockula stake show --account <address or name> --rpc https://<any node>
# Operators: commission (moves a little per epoch), unjail after downtime
blockula validator commission 500 --key ~/val/validator.key --rpc https://<any node>
blockula validator unjail boo1… --key ~/val/validator.key --rpc https://<any node>
blockula validator list --rpc https://<any node>
```

Chains made with `blockula summon` also have validator admins, who can pin a validator ahead of stake (`blockula validator approve`) or remove one (`blockula validator remove`). Mainnet and the public testnet have none: there, stake alone decides the set.

The p2p transport authenticates peers with ML-DSA-65 and derives its ChaCha20-Poly1305 keys from a
hybrid X25519 + ML-KEM-768 exchange, so recorded traffic stays private unless both are broken
([docs/PQ-TRANSPORT.md](docs/PQ-TRANSPORT.md)). Builds with the older X25519-only transport (the
v0.4 public testnet) can't peer with it.

## Layout

```
node/     Rust node: consensus (Commonware simplex + marshal), chain execution, ML-DSA crypto, HTTP API, explorer
scripts/  local-testnet.sh: 4 validators + full node smoke test
sdk-ts/   @blockula/sdk: wallets, tx signing, client, passkey (WebAuthn PRF) helpers
mcp/      @blockula/mcp: MCP server for coding agents
web/      Passkey web wallet (Next.js), deployed at blockula-wallet.vercel.app
deploy/   Fly.io testnet: Dockerfile, entrypoint, launch.sh
docs/     ONE_PAGER.md (what this is), PLAN.md (architecture, tokenomics, roadmap), MAINNET.md (launch readiness),
          MAINNET-DESIGN.md (mainnet decisions and spec), TOKENOMICS.md (mainnet supply and allocation)
```

## Status

Public testnet live (Phase 1), with Hunts and Proof of Contribution v1 (Phases 1.5–2). An internal four-track security and product review was completed and its findings fixed. **No third-party audit yet: test value only.** See [docs/PLAN.md](docs/PLAN.md), and [docs/MAINNET.md](docs/MAINNET.md) for what stands between this and a mainnet. Report vulnerabilities privately: [SECURITY.md](SECURITY.md).
