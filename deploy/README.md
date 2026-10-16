# Deploying the Blockula testnet (Fly.io)

`deploy/launch.sh` launches a protocol v1 public testnet, `blockula-testnet-2` by default, and creates:

| App | What | Public address |
|---|---|---|
| `blockula-v0` … `blockula-v3` | One validator each, in iad / sjc / fra / ams | Its own dedicated IPv4, **p2p `:30300` only**. HTTP stays private |
| `blockula-testnet` | Public full node: API, explorer, sync source for joiners | `https://blockula-testnet.fly.dev` (Fly handles HTTPS) |

The public node forwards txs and faucet requests to validators over Fly's private, WireGuard-encrypted network (`http://blockula-vN.internal:7070`), authenticated with a shared forwarder secret.

The chain is built like mainnet, at testnet scale: addresses use the `tboo` prefix (DEC-7), the genesis comes from the genesis builder (`blockula genesis operator` per validator, then `blockula genesis build --spec`, docs/GENESIS_CEREMONY.md) with the DEC-5 allocation, every validator has its own **owner account** (it holds the self-bond, earns the commission and pays the validator's system fees, SF-13), and there are **no admin keys**: the validators pass proposals as the bootstrap council (GOVERNANCE.md). Two things differ from mainnet: a faucet (100 FANG drips, paid from a float on each validator's consensus-key account, the `faucet` allocation) and shorter durations (a day of unbonding, day-long votes, an hour's timelock, spends open from genesis; `spec.toml`'s `[params]`).

**Rough cost:** ~$60/month. Each validator is one `shared-cpu-2x` 2 GB machine + a $2/mo dedicated IPv4 + a 10 GB volume, plus the public node (`shared-cpu-1x` 1 GB). Check [fly.io/pricing](https://fly.io/pricing) for current rates. Validators need a dedicated IPv4 because Fly's free shared IPv4 only carries HTTP, and p2p is raw TCP.

## Launch

```bash
brew install flyctl jq
fly auth login
GENESIS_ONLY=1 deploy/launch.sh   # optional rehearsal: keys, operator files, spec and genesis, no Fly calls
deploy/launch.sh                  # idempotent: re-run to upgrade the node everywhere
```

Options (env vars):

| Variable | Default |
|---|---|
| `CHAIN_ID` | `blockula-testnet-2`. Never reuse a chain id: a relaunch takes a new one |
| `ADDRESS_PREFIX` | `tboo` |
| `ORG` | `personal` |
| `PREFIX` | `blockula` (app names) |
| `REGIONS` | `"iad sjc fra ams"` (one validator per region) |
| `RPC_REGION` | `iad` |
| `EPOCH_BLOCKS` | `3600` (1-hour epochs) |
| `FAUCET_FANG` | `2500000` per validator's consensus-key account |
| `SELF_BOND` | `10000` FANG per owner (the default `min_self_bond`) |
| `VALIDATOR_VM`, `VALIDATOR_MEMORY`, `RPC_VM`, `RPC_MEMORY`, `VOLUME_GB` | `shared-cpu-2x`, `2048mb`, `shared-cpu-1x`, `1024mb`, `10` |
| `HALT_HEIGHT` | unset. Set for a coordinated stop: validators run `--halt-height` (OPERATIONS.md "Incidents") |

## Keys and secrets

Everything is generated once in `~/.config/blockula/$CHAIN_ID` (mode 700):

- **Consensus keys** (`v*/validator.key`, `v*/consensus.json`): each one is uploaded only to its own app, as the Fly secret `VALIDATOR_KEY_B64` (through stdin, never argv). On first boot it's copied onto that app's volume, and then **launch.sh deletes the secret from Fly**. A cloned, scaled or replacement machine with an empty volume refuses to start instead of double-signing. The local copies are your backup.
- **Owner accounts** (`owner-*.json`), and the founder, foundation and grants stand-ins (`founder.json`, `foundation.json`, `grants.json`): never leave this laptop. An owner's allocation vests (the operators bucket's 6-month cliff), so it can bond and pay fees but not transfer yet. Back them up.
- **Genesis inputs** (public): `operators/v*.toml` (each validator's entry with its proof of possession), `spec.toml`, `genesis.json`. `blockula genesis build --spec spec.toml` rebuilds `genesis.json` byte for byte.
- **Containers** run as an unprivileged `blockula` user.

## Relaunch (replacing `blockula-testnet` with `blockula-testnet-2`)

The v0.4 testnet (`blockula-testnet`, `boo` addresses) can't run protocol v1, and its chain id is never reused. Order matters: clients pin the chain, so they're released after the new chain exists.

1. **Rehearse locally:** `GENESIS_ONLY=1 deploy/launch.sh` (writes `~/.config/blockula/blockula-testnet-2-rehearsal`, placeholder IPs). Check the printed warnings are only the expected policy ones (fewer than 22 operators, one provider, foundation seats).
2. **Launch:** `RESET=1 deploy/launch.sh`. `RESET=1` destroys the apps' old machines, volumes and secrets (the v0.4 chain's data); it's refused once `blockula-testnet-2` has launched. The script allocates IPs, builds the genesis with the real IPs, stages each consensus key, deploys, removes the keys from Fly, and prints the **genesis hash**. Later upgrades: `deploy/launch.sh` without `RESET`.
3. **Check:** `curl https://blockula-testnet.fly.dev/api/status` shows `chain_id: blockula-testnet-2`, `address_prefix: tboo`, the genesis hash, a rising height and 4 active validators; `/api/validators` lists each with its owner; `/api/fees`, `/api/proposals` and `POST /api/faucet` work.
4. **Monitoring:** Fly scrapes every app's `/metrics` (the `[metrics]` section; Fly's managed Grafana). For the reference stack with its alerts (`deploy/monitoring`), run it on a machine in the org's private network (`fly wireguard create`) with the targets launch.sh wrote to `~/.config/blockula/blockula-testnet-2/prometheus-targets.yml`.
5. **Pin the chain in the clients and release them** (the release notes below), then publish the genesis hash where users look (README, the explorer's status).
6. **Optional:** give validators a GitHub token for contribution claims (`pbpaste | deploy/set-github-token.sh`).

**Release notes for the relaunch:**

- **MCP (`@blockula/mcp`):** the `testnet` network now pins `blockula-testnet-2` and the `tboo` prefix (`mcp/src/network.ts`). Its genesis hash isn't built in until a release adds it to `KNOWN_GENESIS` (set it in that release, from step 2): until then the first status the node reports pins it (trust on first use, remembered in `~/.blockula/known-chains.json`). An entry remembered for the old `blockula-testnet` isn't held against the new chain. Users who pinned the old chain by hand (`BLOCKULA_CHAIN_ID=blockula-testnet` or `BLOCKULA_GENESIS_HASH`) must drop or update those variables; users can pin the new one with `BLOCKULA_GENESIS_HASH=<hash>`. Old testnet wallets and balances don't carry over: agents run `create_wallet` and `faucet` again (addresses now start with `tboo1`).
- **Web wallet (Vercel env):** `NEXT_PUBLIC_TESTNET_CHAIN_ID=blockula-testnet-2`, `NEXT_PUBLIC_TESTNET_PREFIX=tboo`, `NEXT_PUBLIC_TESTNET_GENESIS_HASH=<hash from step 2>` (the code defaults still describe the old chain), then redeploy. Passkeys keep working (the key doesn't depend on the chain), but accounts start empty and display `tboo1…` addresses.
- **SDK (`@blockula/sdk`):** no change needed: `Client` reads the prefix from `/api/status`; apps that pin should pass `{ chainId: "blockula-testnet-2", genesisHash, prefix: "tboo" }`.
- **CLI and docs:** `blockula join --genesis https://blockula-testnet.fly.dev --genesis-hash <hash>` (the binary doesn't know this chain, so `join` needs the pin). Faucet, Hunts and Soul from the old testnet are gone.

## Day 2

| Task | Command |
|---|---|
| Upgrade all nodes | `deploy/launch.sh` |
| Start a brand-new testnet | a new `CHAIN_ID` with `RESET=1 deploy/launch.sh` (then the relaunch steps above) |
| Coordinated stop at a height | `HALT_HEIGHT=<h> deploy/launch.sh`; to resume, run it again without `HALT_HEIGHT` |
| Logs | `fly logs -a blockula-v0` |
| Status | `curl https://blockula-testnet.fly.dev/api/status` |
| Give validators a GitHub token (contribution claims, link checks) | `pbpaste \| deploy/set-github-token.sh` (fine-grained, public-read-only token; restarts apps one at a time) |
| Add one of *our* validators later | `fly apps create`, a volume (`--scheduled-snapshots=false`) and a dedicated IPv4, then the normal "Become a validator" flow (register with a self-bond; it joins by stake at an epoch boundary). No new genesis needed |

## Caveats

- Upgrades redeploy validators in parallel, so block production pauses briefly.
- Each validator app must stay at exactly **one machine** (`--ha=false`). Never `fly scale count`, clone a validator machine, or restore a volume snapshot: two copies of a validator key double-sign. Validator volumes are created with scheduled snapshots off (and launch.sh turns them off on older volumes); the entrypoint and node guard against the common cases, but don't rely on that.
- **Deliberate stops stay down.** A validator that exits with 10 (`--halt-height`), 11 (the chain needs a newer binary) or 12 (slashing protection saw its key sign elsewhere) isn't restarted: the entrypoint logs why and idles, and the p2p service doesn't auto-start machines. Crashes (any other exit) restart under the `on-failure` policy. Deploying a new image or `fly machine restart` starts the node again once you've acted. Exit 12 also holds across those: the node records it in `/data/home/double-sign-halt.json` and exits 12 again on every start, so a redeploy, a restart or a host move can't bring it back next to the other instance. Find the other instance first (OPERATIONS.md), then `fly ssh console -a <app> -C "rm /data/home/double-sign-halt.json"` and `fly machine restart`.
- If a validator's volume is lost, its key is gone from Fly by design. Re-seed it deliberately from your local backup (`printf 'VALIDATOR_KEY_B64=…' | fly secrets import -a blockula-vN`), then re-run launch.sh.

## Monitoring and operations

- [`monitoring/`](monitoring/README.md): the reference Prometheus, Alertmanager and Grafana stack every operator runs, with alert rules and a dashboard.
- [docs/OPERATIONS.md](../docs/OPERATIONS.md): hardware, ports, time, sentries, keys and restores, upgrades, incident response, runbooks and releases. Mainnet validators follow it rather than this testnet setup (one org, keys generated on one laptop).
