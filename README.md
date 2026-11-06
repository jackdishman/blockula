# 🧛 Blockula

**Launch your own quantum-safe ledger in a minute.** Name it, give it a ticker, choose who can reach it, and it's running on your machine, with an explorer you can install on your Mac or iPhone and one command your friends run to join. Post-quantum signatures throughout, on computers you already have: no hosting to pay for and nothing to buy.

```bash
curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
blockula launch
```

```text
🦇 Launch your own ledger. Press Enter to keep a [default].

1/4  Name it
  Chain name: Jack in the Block
  Ticker (2–8 letters or digits) [JITB]: JACK
  One line about it (optional): Jack's friends' ledger.
  Chain id [jack-in-the-block]:

2/4  Who can reach it
    1) Just this machine  try it out alone; nobody else can reach it
    2) Tailscale          friends anywhere, and only your tailnet can reach it (found 100.64.0.7)
    3) One home network   every machine on the same Wi-Fi or LAN (found 192.168.1.20)
    4) The open internet  a public IP or DNS name, TCP 30300 and 7070 forwarded
  Pick 1–4 [2]:
  …

🪦 Jack in the Block is summoned (JACK).
   Explorer:      http://localhost:7070
   For friends:   blockula join --genesis http://100.64.0.7:7070 --genesis-hash cc5b… --home ~/.blockula/jack-in-the-block
```

The whole walk-through, with friends joining and becoming validators: **[docs/LAUNCH.md](docs/LAUNCH.md)**.

## What you get

- **Your chain, under your name.** Wallets and explorers show its name and amounts in its ticker. The genesis hash binds both, so no node can pass your chain off as another.
- **Apps with nothing to build.** Every node serves an explorer that installs as an app (Safari: **Add to Dock** on a Mac, **Add to Home Screen** on an iPhone), served over HTTPS on your tailnet if you like. A passkey wallet (`npm run wallet`) and an MCP server for coding agents talk to your node.
- **Friends join in one command.** Their node checks every block. You approve the ones who become validators; from 4 validators on, the chain keeps going while one is offline.
- **Light.** A block every 5 seconds by default: an idle validator uses about 80 MB of memory, a quarter of a percent of a CPU core and 16 KB/s of upload.
- **Quantum-safe.** Every account, vote and block is signed with **ML-DSA-65** (NIST FIPS 204); validators key their channels with hybrid X25519 + **ML-KEM-768**; hashes are BLAKE3-256.
- **Complete.** Accounts with several keys and spending limits for agents, readable `.boo` names, staking, governance, and optional WebAssembly programs (experimental, off by default).

**Its unit has no value.** Each chain's unit (FANG, or the ticker you give it; 10⁹ drops each) is handed out by its faucet so every transaction pays a tiny fee, which keeps spam expensive. Nobody sells it, buys it or gets paid in it. Blockula isn't launching a mainnet or selling a token: it's software for running your own ledger.

## Try it alone first

```bash
blockula dev        # a throwaway one-node chain with a faucet, 1-second blocks, explorer at http://localhost:7070
```

Or `blockula launch` and pick **Just this machine**: a chain you keep, that only you can reach.

## Apps

| | |
|---|---|
| 🔎 Explorer | Built into every node at `http://<node>:7070`. Install it from Safari: File > Add to Dock (Mac), Share > Add to Home Screen (iPhone, iPad) |
| 👛 Wallet | `git clone --depth 1 https://github.com/jackdishman/blockula && cd blockula && npm ci && npm run wallet`, then http://localhost:3000: passkey sign-up, the faucet, a `.boo` name, sending and receiving ([web/](web/README.md)) |
| ⌨️ CLI wallet | `blockula wallet new`, `blockula wallet faucet`, `blockula wallet send alice.boo 1` |
| 🤖 Agents | `claude mcp add blockula -- npx -y @blockula/mcp`, then the prompt in [AGENTS.md](AGENTS.md) |

## For developers

```bash
npm install @blockula/sdk                         # SDK for apps, scripts and browsers
claude mcp add blockula -- npx -y @blockula/mcp   # MCP server for coding agents
```

```ts
import { Client, Wallet } from "@blockula/sdk";

const wallet = Wallet.generate(new Client("http://localhost:7070"));
await wallet.faucet();                       // 100 from the chain's faucet
await wallet.registerName("vlad");           // vlad.boo
await wallet.send("alice.boo", "1.5", { memo: "trick or treat" });
```

In the browser a passkey unlocks the wallet, with no seed phrase: `createPasskeyWallet({ username: "vlad" })` from `@blockula/sdk/passkey`. Both packages need Node 20 or newer and use the node at `http://localhost:7070` unless told otherwise ([sdk-ts/](sdk-ts/README.md), [mcp/](mcp/README.md)).

**From source:** `npm install && npm run build` builds the Rust node, the SDK and the MCP server; `npm run dev` starts a dev chain. `npm test` runs the Rust tests and the SDK's end-to-end tests.

## Docs

| | |
|---|---|
| [LAUNCH.md](docs/LAUNCH.md) | Launch a chain, friends join and become validators, apps, costs |
| [RUN-A-NODE.md](docs/RUN-A-NODE.md) | A full node or a validator as a service, and using your own node |
| [REFERENCE.md](docs/REFERENCE.md) | Chains by hand, public servers, the hardened API, validators, Hunts and the contribution pool |
| [OPERATIONS.md](docs/OPERATIONS.md) | Running validators: upgrades, restarts, slashing protection, monitoring |
| [AGENTS.md](AGENTS.md) | For coding agents: the copy-paste prompt, the rules and the HTTP API |
| [docs/](docs/README.md) | Everything else: governance, programs, the explorer, Ghost Mode, design history |

## Layout

```
node/     The node and the `blockula` CLI (Rust): launch, consensus, execution, HTTP API, the explorer
web/      The passkey web wallet (Next.js), also an explorer
sdk-ts/   @blockula/sdk: wallets, signing, the client, passkeys
mcp/      @blockula/mcp: the MCP server for coding agents
docs/     Guides and design documents
scripts/  Install, release and test scripts
deploy/   Ops for the retired public testnet (Fly.io)
```

## Status

**Blockula 1.0** is a kit for running your own ledger. An internal security and product review was completed and its findings fixed; **there's no third-party audit**. The public testnet (`blockula-testnet-2`) ran from October to November 2026 and was retired; CI replays its archived history every night. Contributions are welcome from people and agents: [CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities privately: [SECURITY.md](SECURITY.md).
