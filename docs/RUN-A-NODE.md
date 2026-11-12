# Run a Blockula node

Your own node gives you your own view of a chain: one you [made](CREATE.md), or one someone else made. (Blockula's public testnet, `blockula-testnet-2`, was retired in November 2026; there's no public chain to join.) Your wallet, apps and agents can read through it and send through it, and it's the independent witness that lets them prove names. This page covers a node at home or on a small server, and becoming a validator.

[OPERATIONS.md](OPERATIONS.md) is the reference for professional validator operators: sentries, monitoring, incident response and releases.

> **Before you run a node or validator**
>
> - **It's experimental software, provided as is**, under the MIT license, without warranty. It isn't externally audited.
> - **Validators can lose stake.** Signing two different blocks for the same turn (two copies of one key, a restored disk) slashes 5% of the validator's pool, its delegators' stake included, and bans the validator for good. Missing too many turns jails it and slashes a little.
> - **Stake is slow to leave.** Unbonding takes 21 days by default (a chain's genesis can set less), and FANG that's unbonding can still be slashed for a fault from before it left.
> - **Upgrades can stop your node.** When governance schedules a protocol version your release doesn't run, your node stops at that height until you install one that does. The network can also be halted on purpose to fix a problem.
> - **Whoever runs the validators governs at first.** Until a chain's bootstrap period ends, its validators can pass proposals as a council: on a chain someone else made, that's whoever runs its validators.
> - **Validating isn't paid work.** Rewards are FANG from the chain's issuance, and FANG has no value. There's no service agreement and no compensation for downtime, slashes or halts.
> - **You're responsible for your own compliance:** the law where you are, and your hosting provider's terms (some ban blockchain workloads).
> - **Your validator's addresses are public and permanent.** The chain publishes the p2p and HTTP addresses you register, and peers see your node's IP.

## Which node?

| | Personal full node | Public RPC node | Validator |
|---|---|---|---|
| What it does | Syncs and checks every block; serves your own wallet, apps and agents | A full node others use: the API and explorer over HTTPS | Votes on blocks; needs a self-bond |
| Command | `blockula join` | `blockula join` behind a reverse proxy | `blockula validator init`, then `run` and `register` |
| Exposed to the internet | Nothing | HTTPS (443) through a proxy | p2p only (TCP 30300). Never its HTTP API |
| Where it runs | A laptop, a home server, a small VPS | A VPS with a TLS proxy | A machine that's always on, with a public IP or port forward |

## What it takes

Measured on 2026-10-21 with release 0.5.2: a local chain of 4 validators and a full node, idle, over 5 minutes, and a full node of the public testnet of the time.

| | Personal full node | Validator, set of 4 (measured) | Validator, set of 7 (estimate) | Validator, set of 32 (estimate) |
|---|---|---|---|---|
| Memory | ~30 MB | ~80 MB | about the same | [Reference hardware](OPERATIONS.md#reference-hardware) |
| CPU | less than a validator | 1–2% of a core | a little more | |
| Upload | only what you serve | ~59 KB/s, **~150 GB a month** | ~600 GB a month | ~12 TB a month |
| Disk | ~1 MB after a fast sync, then ~80 MB a day on an idle chain | two days of blocks (it prunes): ~0.2 GB idle | ~0.2 GB idle | ~0.3 GB idle |

- **Memory** grows with the chain's state (accounts, names), not with its history or the size of the set. Give any node at least 1 GB of RAM for headroom; a public RPC node needs more under load.
- **Bandwidth** is what limits a home validator. Certificates are sent to every other validator, so a validator's traffic grows with the size of the set. The 7- and 32-validator figures come from the certificate maths in [OPERATIONS.md, Bandwidth](OPERATIONS.md#bandwidth). A home connection with a 1.2 TB monthly cap fits a validator in a set of up to about 10.
- **Chains you make yourself** (`blockula summon`, [CREATE.md](CREATE.md)) make a block every 5 seconds by default, so an idle validator sends about a fifth of the upload above, which was measured at 1-second blocks. [CREATE.md, What it costs](CREATE.md#what-it-costs) has both, measured side by side.
- **Disk** for a full node grows with use: about 80 MB a day on an idle chain of 4 validators (about 30 GB a year), plus about 17 KB per transfer. A full node keeps every block unless you pass `--prune-blocks N`. A validator keeps about two days. [OPERATIONS.md, Disk sizing](OPERATIONS.md#disk-sizing) has the full tables.

## Install

```bash
curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
```

It checks the download's checksum, and its build provenance when the GitHub CLI is logged in. For a validator, install the way [OPERATIONS.md, A first install](OPERATIONS.md#a-first-install) says: it checks the release's signature before anything runs.

## A personal full node

```bash
blockula join --genesis https://<a node of the chain> --genesis-hash <its genesis hash> --home ~/.blockula/node
```

It fetches the genesis from that node, checks it against the hash (the chain's creator gives it to you with the address, [CREATE.md](CREATE.md)), fast-syncs from a certified snapshot (seconds), then follows the chain. Its API and explorer are at http://localhost:7171, on this machine only.

- **Restart it** with `blockula run --home ~/.blockula/node`. Check it with `blockula status --rpc http://localhost:7171` or `curl -s localhost:7171/api/health`.
- **History.** A fast-synced node holds blocks from its snapshot on (`history_from` in `/api/status`). Pass `--full` to `join` to replay from genesis instead, as long as a peer still has the early blocks.
- **Keep it running** with your system's service manager. A minimal systemd unit:

  ```ini
  # /etc/systemd/system/blockula-node.service
  [Unit]
  Description=Blockula full node
  After=network-online.target
  Wants=network-online.target

  [Service]
  User=blockula
  ExecStart=/usr/local/bin/blockula run --home /var/lib/blockula/node
  Restart=on-failure
  RestartSec=10

  [Install]
  WantedBy=multi-user.target
  ```

- **A chain on a home network or Tailscale** ([CREATE.md](CREATE.md)): its nodes serve plain `http://<address>:7070`, which `join` accepts on a private network.

## Use your node

Point everything at `http://localhost:7171`. Your own node answers for what you see, and another node of the chain, run by someone else, can be a witness to it.

- **The CLI:** add `--rpc http://localhost:7171` to each command. It doesn't read `BLOCKULA_RPC` yet.

  ```bash
  blockula wallet balance --rpc http://localhost:7171
  ```

- **The SDK:** a client for your node, pinned to the chain, with another node as a witness, so every name that ends up in a signature is proven against both.

  ```ts
  import { Client } from "@blockula/sdk";

  const client = new Client("http://localhost:7171", {
    chainId: "<its chain id>",
    genesisHash: "<its genesis hash>",
    witnesses: [new Client("https://<another node of the chain>")],
  });
  ```

- **The MCP** (for coding agents): use your node, pin the chain, and add another node as a witness.

  ```bash
  claude mcp add blockula \
    -e BLOCKULA_RPC=http://localhost:7171 \
    -e BLOCKULA_CHAIN_ID=<its chain id> -e BLOCKULA_GENESIS_HASH=<its genesis hash> \
    -e BLOCKULA_WITNESSES=https://<another node of the chain> \
    -- npx -y @blockula/mcp
  ```

- **The web wallet:** open the network menu and pick **Custom node…**, then enter your node's URL. The wallet then shows a warning, because a custom node isn't checked against the network's pins. Browsers may refuse a plain-`http` node from an `https` page, so serve it over HTTPS, or use the SDK, MCP or CLI. A "my node" option that keeps the network's pins is planned.

## Running at home

- **Forward one port.** On your router, forward TCP 30300 to the machine, and only for a validator: a full node needs no inbound connections at all.
- **Keep HTTP on this machine.** Every node's HTTP API listens on 127.0.0.1 by default. A validator listens for p2p on every interface with `--p2p-bind 0.0.0.0` (or `::`), while its HTTP stays local. Only `--bind 0.0.0.0` would open both.
- **A changing IP needs a name.** Most home connections change IP from time to time. Use a dynamic DNS name (from your router or a DNS provider) as your validator's p2p address, so it follows your IP. If you registered an IP and it changed, publish the new one with `blockula validator set-addr` (below).
- **Carrier-grade NAT doesn't work.** If your router's WAN address is in 100.64.0.0/10, your provider shares one public IP between many customers, and nothing can reach your port. Run a validator on a VPS instead.
- **Keep the clock right.** Validators reject blocks stamped more than a second ahead of their own clock. Use chrony (or systemd-timesyncd) on Linux; macOS and Windows sync automatically. Validators: [OPERATIONS.md, Time](OPERATIONS.md#time).
- **Stay online.** A validator that misses too many turns is jailed until its owner unjails it. Plan for power cuts and router reboots.

## Become a validator

On a chain someone else made (on your own, `summon` made the validators: [CREATE.md](CREATE.md)):

1. **Install a verified release ([OPERATIONS.md, A first install](OPERATIONS.md#a-first-install)).
2. **Create the validator home.** On a public chain both addresses are required, and loopback, private and unspecified hosts are refused:

   ```bash
   blockula validator init --home ~/val --genesis https://<a node of the chain> --genesis-hash <its genesis hash> \
     --public-p2p-addr <your public IP or DNS name>:30300 \
     --public-http-addr https://<your full node's public URL>
   ```

   - `--public-p2p-addr` is what other validators dial.
   - `--public-http-addr` is published as your validator's HTTP address. Point it at a full node you run in front of the validator ([OPERATIONS.md, Sentries](OPERATIONS.md#sentries)), never at the validator itself.
   - `--allow-private-addr` accepts private hosts, for validators that really reach each other over a private network.
3. **Get the self-bond** (below) into the address `init` printed.
4. **Start its node**, with p2p open and HTTP on this machine:

   ```bash
   blockula run --home ~/val --p2p-bind 0.0.0.0
   ```

5. **Register it.** `register` first checks that the node runs on this chain, then signs the registration and prints how this chain lets the validator in: stake ranking at an epoch boundary (with the lowest stake in the set), an admin's approval, or, in a set of one or two, the members' admission.

   ```bash
   blockula validator register --home ~/val --rpc https://<a node of the chain>
   ```

6. **Keep it running** under systemd: [OPERATIONS.md, Running a validator](OPERATIONS.md#running-a-validator). Never run a second copy of `validator.key`.
7. **Watch for upgrades.** When governance schedules a protocol version your release doesn't run, the node logs a warning every hour: "protocol vN activates at #H (in about …): install a release that supports it". Install it before that height, or the node stops there.

### Validate someone else's chain

The chains listed on [blockula.app](https://blockula.app) with **Wants validators** take validators from outside. Each card's **Become a validator** shows these steps with the chain's node, genesis hash and home filled in. New to this? [BECOME-A-VALIDATOR.md](BECOME-A-VALIDATOR.md) walks through it step by step: in the cloud (Fly.io with `deploy/host-validator.sh`, or a $5 VPS), at home, or at home behind a cheap relay that keeps your IP private, with a prompt that lets your coding agent do the setup. Before you start:

- **Your machine stays on and online.** A validator that sleeps or drops off misses its turns and is jailed.
- **Two ports are open to the internet:** TCP 30300 (other validators dial it) and 7171 (your full node, which the chain publishes as your validator's HTTP address). Forward them on your router ([Running at home](#running-at-home)).
- **Only chains on the open internet can take you.** A chain on a tailnet or a home network can't: its validators must reach yours, and yours theirs. Chains listed without **Wants validators** are open for their wallet only.

With `<node>` the chain's listed `https://` node, `<hash>` its genesis hash, `<id>` its chain id and `<your host>` your public IP or DNS name, one command sets up both nodes, runs them in the background (launchd on a Mac, systemd on Linux) and registers the validator:

```bash
curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
blockula validator setup --node <node> --genesis-hash <hash> --host <your host>
```

([BECOME-A-VALIDATOR.md](BECOME-A-VALIDATOR.md#the-quick-way-blockula-validator-setup) says what it does.) The same by hand:

```bash
curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh

# Your own full node of the chain (leave it running): it checks the genesis hash, and serves
# your validator's public HTTP address on port 7171
blockula join --genesis <node> --genesis-hash <hash> --home ~/.blockula/<id> --bind 0.0.0.0

# In another terminal: the validator. init prints its address (boo1…)
blockula validator init --home ~/.blockula/<id>-validator --genesis <node> --genesis-hash <hash> \
  --public-p2p-addr <your host>:30300 --public-http-addr http://<your host>:7171

# The bond (/api/validators' min_bond), from the faucet if the chain has one; else ask the owner
blockula wallet faucet --to <your validator address> --rpc <node>

blockula run --home ~/.blockula/<id>-validator --p2p-bind 0.0.0.0
blockula validator register --home ~/.blockula/<id>-validator --rpc <node>
```

`register` says how the chain lets you in, which depends on the size of its set:

- **One or two validators:** every one is needed for each block, so a newcomer joins only once the members admit it. Send the owner your validator address (the listing's contact), check it with them, and ask them to run `blockula validator admit <your validator address> --key ~/.blockula/<id>/validator.key --rpc http://localhost:7070`. Your node signals it's ready by itself and joins at the end of that epoch.
- **Three:** the chain's admin approves you: `blockula validator approve <your validator address> --key ~/.blockula/<id>/validator.key --rpc http://localhost:7070`.
- **Four or more:** you join by stake at an epoch boundary, a few newcomers an epoch.

**Changing its addresses** (a new IP, a new machine, a loopback address registered by mistake):

```bash
blockula validator set-addr --key ~/val/validator.key --rpc https://<a node of the chain> \
  --p2p <new host>:30300 --http https://<your full node> --home ~/val
```

It signs `update_validator` with your current commission, so only the addresses change. The consensus key can do this once an epoch; the owner's key any time. `--home` also updates `node.json`, which your node advertises: restart it afterwards. [OPERATIONS.md, Moving a validator](OPERATIONS.md#moving-a-validator) covers a move to another machine.

## Getting the self-bond

<!-- D8 (decided 2026-10-21): the self-bond path for outside validators. Keep it in this section. -->

A validator registers with a self-bond of at least the chain's `min_self_bond` (10,000 FANG by default). A chain you make sets its own: `blockula summon` defaults to 50 FANG, and to 10,000 with `--no-faucet` ([CREATE.md](CREATE.md#who-can-join)).

- **A chain you make, or one you join:** its faucet pays the bond (`blockula wallet faucet --to`), or, on a chain without one, a validator's account sends it (`blockula wallet send --key`): [CREATE.md](CREATE.md).
