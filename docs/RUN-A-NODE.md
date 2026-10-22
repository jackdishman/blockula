# Run a Blockula node

Your own node gives you your own view of the chain. Your wallet, apps and agents can read through it and send through it, and it's the independent witness that lets them prove names. This page covers a node at home or on a small server, and becoming a validator.

[OPERATIONS.md](OPERATIONS.md) is the reference for professional validator operators: sentries, monitoring, incident response and releases.

> **Before you run a node or validator**
>
> - **It's experimental software, provided as is**, under the MIT license, without warranty. It isn't externally audited.
> - **Validators can lose stake.** Signing two different blocks for the same turn (two copies of one key, a restored disk) slashes 5% of the validator's pool, its delegators' stake included, and bans the validator for good. Missing too many turns jails it and slashes a little.
> - **Stake is slow to leave.** Unbonding takes 21 days by default (a week on the public testnet), and FANG that's unbonding can still be slashed for a fault from before it left.
> - **Upgrades can stop your node.** When governance schedules a protocol version your release doesn't run, your node stops at that height until you install one that does. The network can also be halted on purpose to fix a problem.
> - **Whoever runs the validators governs at first.** Until a chain's bootstrap period ends, its validators can pass proposals as a council. The project runs all of the public testnet's.
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

Measured on 2026-10-21 with release 0.5.2: a local chain of 4 validators and a full node, idle, over 5 minutes, and a full node of the public testnet.

| | Personal full node | Validator, set of 4 (measured) | Validator, set of 7 (estimate) | Validator, set of 32 (estimate) |
|---|---|---|---|---|
| Memory | ~30 MB | ~80 MB | about the same | [Reference hardware](OPERATIONS.md#reference-hardware) |
| CPU | less than a validator | 1–2% of a core | a little more | |
| Upload | only what you serve | ~59 KB/s, **~150 GB a month** | ~600 GB a month | ~12 TB a month |
| Disk | ~1 MB after a fast sync, then ~80 MB a day on an idle chain | two days of blocks (it prunes): ~0.2 GB idle | ~0.2 GB idle | ~0.3 GB idle |

- **Memory** grows with the chain's state (accounts, names), not with its history or the size of the set. Give any node at least 1 GB of RAM for headroom; a public RPC node needs more under load.
- **Bandwidth** is what limits a home validator. Certificates are sent to every other validator, so a validator's traffic grows with the size of the set. The 7- and 32-validator figures come from the certificate maths in [OPERATIONS.md, Bandwidth](OPERATIONS.md#bandwidth). A home connection with a 1.2 TB monthly cap fits a validator in a set of up to about 10.
- **Chains you launch yourself** (`blockula summon`, [LAUNCH.md](LAUNCH.md)) make a block every 5 seconds by default, so an idle validator sends about a fifth of the upload above, which was measured at the public testnet's 1-second blocks. [LAUNCH.md, What it costs](LAUNCH.md#what-it-costs) has both, measured side by side.
- **Disk** for a full node grows with use: about 80 MB a day on an idle chain of 4 validators (about 30 GB a year), plus about 17 KB per transfer. A full node keeps every block unless you pass `--prune-blocks N`. A validator keeps about two days. [OPERATIONS.md, Disk sizing](OPERATIONS.md#disk-sizing) has the full tables.

## Install

```bash
curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
```

It checks the download's checksum, and its build provenance when the GitHub CLI is logged in. For a validator, install the way [OPERATIONS.md, A first install](OPERATIONS.md#a-first-install) says: it checks the release's signature before anything runs.

## A personal full node

```bash
blockula join --genesis https://blockula-testnet.fly.dev --home ~/.blockula/testnet-node
```

It fetches the genesis from that node, checks it against the one this release knows for `blockula-testnet-2`, fast-syncs from a certified snapshot (seconds), then follows the chain. Its API and explorer are at http://localhost:7171, on this machine only.

- **Restart it** with `blockula run --home ~/.blockula/testnet-node`. Check it with `blockula status --rpc http://localhost:7171` or `curl -s localhost:7171/api/health`.
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

- **A chain someone launched** ([LAUNCH.md](LAUNCH.md)): the same command with one of its nodes as `--genesis`, and the genesis hash its creator gives you as `--genesis-hash`.

## Use your node

Point everything at `http://localhost:7171`. Your own node answers for what you see, and the network's public RPC can be a witness to it.

- **The CLI:** add `--rpc http://localhost:7171` to each command. It doesn't read `BLOCKULA_RPC` yet.

  ```bash
  blockula wallet balance --rpc http://localhost:7171
  ```

- **The SDK:** a client for your node, with the public RPC as a witness, so every name that ends up in a signature is proven against both.

  ```ts
  import { Client } from "@blockula/sdk";

  const client = new Client("http://localhost:7171", {
    chainId: "blockula-testnet-2",
    prefix: "tboo",
    witnesses: [new Client("https://blockula-testnet.fly.dev")],
  });
  ```

- **The MCP** (for coding agents): name the network, use your node, and add the public RPC as a witness.

  ```bash
  claude mcp add blockula \
    -e BLOCKULA_RPC=http://localhost:7171 \
    -e BLOCKULA_WITNESSES=https://blockula-testnet.fly.dev \
    -- npx -y @blockula/mcp --network testnet
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

On the public testnet:

1. **Install** a verified release ([OPERATIONS.md, A first install](OPERATIONS.md#a-first-install)).
2. **Create the validator home.** On a public chain both addresses are required, and loopback, private and unspecified hosts are refused:

   ```bash
   blockula validator init --home ~/val --genesis https://blockula-testnet.fly.dev \
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
   blockula validator register --home ~/val --rpc https://blockula-testnet.fly.dev
   ```

6. **Keep it running** under systemd: [OPERATIONS.md, Running a validator](OPERATIONS.md#running-a-validator). Never run a second copy of `validator.key`.
7. **Watch for upgrades.** When governance schedules a protocol version your release doesn't run, the node logs a warning every hour: "protocol vN activates at #H (in about …): install a release that supports it". Install it before that height, or the node stops there.

**Changing its addresses** (a new IP, a new machine, a loopback address registered by mistake):

```bash
blockula validator set-addr --key ~/val/validator.key --rpc https://blockula-testnet.fly.dev \
  --p2p <new host>:30300 --http https://<your full node> --home ~/val
```

It signs `update_validator` with your current commission, so only the addresses change. The consensus key can do this once an epoch; the owner's key any time. `--home` also updates `node.json`, which your node advertises: restart it afterwards. [OPERATIONS.md, Moving a validator](OPERATIONS.md#moving-a-validator) covers a move to another machine.

## Getting the self-bond

<!-- D8 (decided 2026-10-21): the self-bond path for outside validators. Keep it in this section. -->

A validator registers with a self-bond of at least the chain's `min_self_bond`: 10,000 FANG on the public testnet. A chain you launch sets its own: `blockula summon` defaults to 50 FANG, and to 10,000 with `--no-faucet` ([LAUNCH.md](LAUNCH.md#who-can-join)).

- **Public testnet:** ask for a 10,000 test-FANG self-bond with the [node operator form](https://github.com/jackdishman/blockula/issues/new?template=node-operator.yml). The faucet gives 100 test FANG at a time (once an hour per address, once every 10 minutes per IP), so collecting the bond from it takes 17 hours at best. Test FANG has no value.
- **A chain you launch:** its faucet pays the bond (`blockula wallet faucet --to`), or, on a chain without one, a validator's account sends it (`blockula wallet send --key`): [LAUNCH.md](LAUNCH.md).
