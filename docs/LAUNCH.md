# Launch your own ledger

Blockula is a kit for running your own quantum-safe ledger: a few computers that agree on who holds what, with post-quantum signatures throughout. Your group runs it on machines it already has. There's no hosting to pay for, no company in the middle, and nothing to buy.

- **Quantum-safe.** Every account, vote and block is signed with ML-DSA-65 (NIST FIPS 204). Validators talk over hybrid X25519 + ML-KEM-768 connections, and hashes are BLAKE3.
- **Light.** With the default 5-second blocks, an idle validator in a set of 4 uses about 80 MB of memory, a quarter of a percent of one CPU core and about 16 KB/s of upload ([what it costs](#what-it-costs)).
- **Complete.** Accounts with several keys and spending limits, readable `.boo` names, an explorer in every node, a passkey wallet, a TypeScript SDK and an MCP server for agents.

**Its unit has no value.** Each chain has a native unit, FANG, that its faucet hands out. It's there so every transaction pays a tiny fee, which keeps spam expensive. Nobody sells it, buys it or gets paid in it, and each chain's FANG is its own.

## 1. Pick how your machines reach each other

Validators connect to each other on TCP port 30300, and people's nodes read from a validator's HTTP API on port 7070. Pick one way for everyone:

| | Best for | What to do |
|---|---|---|
| **Tailscale** (recommended) | Friends in different places | Everyone installs [Tailscale](https://tailscale.com) and joins one tailnet. Use each machine's Tailscale address (`100.x.y.z`, or its MagicDNS name). No port forwarding, and only people on your tailnet can reach the chain |
| **One home network** | Machines in one place | Use each machine's local address (`192.168.x.y`) |
| **The open internet** | A ledger anyone can use | A public IP or DNS name, with TCP 30300 and 7070 forwarded to the machine. Read [Who can join](#who-can-join) first |

Below, `<your address>` is the creator's address and `<their address>` a friend's.

## 2. Start the chain

On your machine:

```bash
curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh
blockula summon mychain --host <your address>
blockula run --home ~/.blockula/mychain --bind 0.0.0.0
```

- `summon` prints the chain's **genesis hash** and the command others join with. Everyone who joins pins that hash, so share it with the address.
- `--bind 0.0.0.0` listens on every network the machine is on. On Tailscale, `--bind <your Tailscale address>` listens on your tailnet only.
- The explorer is at `http://<your address>:7070`.

The defaults: a block every 5 seconds, a validator set that can change every 10 minutes (an epoch), a faucet that gives 100 FANG at a time, a 50 FANG validator bond, and you as the chain's admin. `blockula summon --help` lists the options, such as `--block-time-ms 1000` for faster blocks, `--no-faucet` and `--min-bond`.

## 3. Friends join

A full node keeps its own copy of the chain, checks every block, and gives its owner their own explorer and API:

```bash
blockula join --genesis http://<your address>:7070 --genesis-hash <hash> --home ~/.blockula/mychain
```

Its explorer is at `http://localhost:7171`. The wallet, the SDK and the MCP can all use it ([RUN-A-NODE.md, "Use your node"](RUN-A-NODE.md#use-your-node)).

## 4. Friends become validators

Validators take turns making blocks. On the friend's machine:

```bash
# A validator home: a new key, and the addresses the others reach it at
blockula validator init --home ~/.blockula/mychain-validator \
  --genesis http://<your address>:7070 --genesis-hash <hash> \
  --public-p2p-addr <their address>:30300 --public-http-addr http://<their address>:7070

# Its bond, from the faucet, to the validator address init printed (it waits until the payment is final)
blockula wallet faucet --to <validator address> --rpc http://<your address>:7070

# Start it, then register it
blockula run --home ~/.blockula/mychain-validator --bind 0.0.0.0
blockula validator register --home ~/.blockula/mychain-validator --rpc http://<your address>:7070
```

`register` bonds 50 FANG and prints how the validator gets in. On your machine, approve it:

```bash
blockula validator approve <validator address> --key ~/.blockula/mychain/validator.key --rpc http://localhost:7070
```

It joins at the next epoch boundary, within 10 minutes, and starts taking turns.

## How many validators?

- **With 1 to 3 validators, every one must be online** for the chain to make blocks.
- **With 4, the chain keeps going while one is offline**, and 7 can lose 2. Aim for 4 if laptops will sleep.
- **Full nodes can come and go** as they like.
- **A validator that misses too many turns is jailed:** it stops being asked to make blocks and loses a little of its bond. `blockula validator unjail` brings it back.
- **Never run two copies of one validator,** for example from a copied home or a restored backup: signing twice bans it for good.

## Who can join?

- **While the set has 1 to 3 validators,** only you can add one, with `approve` as above.
- **From 4 validators on,** the chain also takes newcomers by stake: anyone who registers with the bond can join at an epoch boundary, a few an epoch at most. On Tailscale or a home network, only people who can reach your nodes can get the bond from the faucet, so that's your group.
- **On the open internet,** keep the seats for people you know. Summon with `--no-faucet`, which also raises the bond to 10,000 FANG, and give FANG yourself to people you trust. Your validator's account holds the chain's starting FANG:

  ```bash
  blockula wallet send <their account> 10000 --key ~/.blockula/mychain/validator.key --rpc http://localhost:7070
  ```

- **As the admin, you can remove a validator.** It leaves at the next epoch, for good:

  ```bash
  blockula validator remove <validator address> --key ~/.blockula/mychain/validator.key --rpc http://localhost:7070
  ```

## What it costs

Measured on an idle chain of 4 validators on one machine, on Oct 22, 2026 (the same build and method for both columns):

| Per validator | 5-second blocks (the default) | 1-second blocks |
|---|---|---|
| Upload | about 16 KB/s, ~42 GB a month | about 84 KB/s, ~216 GB a month |
| CPU | about 0.25% of one core | about 1% |
| Memory | about 80 MB (a full node: about 30 MB) | the same |

- **Upload grows with the number of validators,** because each one sends its votes and certificates to all the others. Keep sets on home connections small: 4 to 10 validators.
- **Disk:** a full node keeps every block. An idle chain at 5-second blocks stores a fifth as many blocks as at 1 second, so about 16 MB a day instead of about 80 MB. A validator keeps about two days of blocks.

## Keep it running

- **Back up each `validator.key`.** Whoever holds it is that validator.
- **Run nodes as a service** so they come back after a restart: [RUN-A-NODE.md](RUN-A-NODE.md#a-personal-full-node) has a systemd unit.
- **Upgrade** with `blockula release install vX.Y.Z` ([OPERATIONS.md](OPERATIONS.md#installing-and-upgrading)).
- **Check on it:** every node's explorer shows blocks, accounts and validators, and `/api/health` answers 200 while the node is in sync.

## Good to know

- **Try it first** on Blockula's public testnet, which runs the same software: [testnet.blockula.app](https://testnet.blockula.app).
- **Programs** (small programs in WebAssembly that run on the chain) are built into the node but switched off. They're an optional feature for later.
- **Making blocks only when there are transactions** would cut an idle chain's cost further. That's a protocol change, for later.
- **The software comes as is,** with no warranty ([LICENSE](../LICENSE)). If you ever want your chain's FANG to have value, by selling it or paying people in it, that's your project, and a question for your own lawyer.
