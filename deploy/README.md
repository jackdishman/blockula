# Deploying the Blockula testnet (Fly.io)

`deploy/launch.sh` launches a protocol v1 public testnet, `blockula-testnet-2` by default, and creates:

| App | What | Public address |
|---|---|---|
| `blockula-v0` … `blockula-v3` | One validator each, in iad / sjc / fra / ams | Its own dedicated IPv4, **p2p `:30300` only**. HTTP stays private |
| `blockula-testnet` | Public full node: API, explorer, sync source for joiners | `https://blockula-testnet.fly.dev` (Fly handles HTTPS) |

The public node forwards txs and faucet requests to validators over Fly's private, WireGuard-encrypted network (`http://blockula-vN.internal:7070`), authenticated with a shared forwarder secret. Each validator's HTTP peers (`PEER_HTTP`) are the other three validators and the public node over the same network, so syncing, epoch exchange and slashing protection's peer checks never rest on one node. In genesis, each validator publishes the public node as its HTTP endpoint: its sentry, as mainnet operators publish theirs.

The chain is built like mainnet, at testnet scale, so it rehearses mainnet's operations (upgrades, joins, governance, the contribution pool):

- **Genesis:** addresses use the `tboo` prefix (DEC-7), the genesis comes from the genesis builder (`blockula genesis operator` per validator, then `blockula genesis build --spec`, docs/GENESIS_CEREMONY.md) with the DEC-5 allocation, every validator has its own **owner account** (it holds the self-bond, earns the commission and pays the validator's system fees, SF-13), and there are **no admin keys**: the validators pass proposals as the bootstrap council (GOVERNANCE.md).
- **Seats:** the four seats stand in for mainnet's independent genesis operators, so none is flagged as the founder's or the foundation's. The founder's stand-in account is excluded from the bootstrap delegation as on mainnet, which backs all four seats (25M FANG each), follows a seat out of the set and back in, and returns to the treasury over years 2–4. (Flagging the seats would leave the delegation nobody to back, and DEC-3's cap on such seats, fewer than f, which is none in a set of 4, would keep a seat that left the set out for good. The staking test `testnet_seats_leave_rejoin_and_get_the_bootstrap_delegation` covers both.)
- **Proof of Contribution from genesis:** the contribution pool opens with `jackdishman/blockula` (weight 10) and 100,000 FANG out of the endowment bucket (an advance on the endowment's release, which on mainnet starts from zero: the genesis builder warns about it), and founding jurors start with 25 Soul each in the genesis Soul ledger (enough to sit on a jury and to vouch). By default these are three stand-in accounts made on the launcher's laptop; `JURORS_FILE` names real ones.
- **What differs from mainnet:** a faucet (100 FANG drips, paid from a float on each validator's consensus-key account, the `faucet` allocation), and shorter durations: a **week of unbonding and of evidence** (so the weak-subjectivity period is 3.5 days: [Sync trust](#sync-trust)), day-long votes, an hour's timelock, spends open from genesis, and Séances every 3 days with a 3-day revert window and 30-day vesting (`spec.toml`'s `[params]` and `[contribution]`). The evidence window starts below governance's 14-day floor: a vote can raise it, not lower it. The **downtime window** is sized for four validators: jailed past 600 of a validator's last 1,200 leader slots, about 30 minutes away, where the chain's default 50 of 100 would jail after about 3 minutes ([OPERATIONS.md, Jailed or tombstoned](../docs/OPERATIONS.md#jailed-or-tombstoned)). `blockula-testnet-2` predates that and runs 50 of 100: keep its restarts on the [participation gate](#restarting-validators), and every validator's time away under about two minutes. Changing it there takes a token vote (the parameters are the token holders' alone), and until the operator and grant allocations' six-month cliff (block 15,778,800, about April 2027) its vested FANG adds up to about half the 20M FANG quorum.

**Rough cost:** ~$60/month. Each validator is one `shared-cpu-2x` 2 GB machine + a $2/mo dedicated IPv4 + a 10 GB volume, plus the public node (`shared-cpu-1x` 1 GB, 10 GB). Check [fly.io/pricing](https://fly.io/pricing) for current rates. Validators need a dedicated IPv4 because Fly's free shared IPv4 only carries HTTP, and p2p is raw TCP.

**Disk:** validators keep the node's default window of blocks (about two days); the public node keeps 14 days. An idle 4-validator chain writes about 82 MB a day and a transfer about 17 KB more ([OPERATIONS.md, Disk sizing](../docs/OPERATIONS.md#disk-sizing)), so 14 days is ~1.2 GB idle and fills 10 GB at a sustained ~0.4 tx/s. Raise `RPC_VOLUME_GB` (and `fly volumes extend`) or shorten `RPC_PRUNE_BLOCKS` if traffic grows; `RPC_ARCHIVE=1` keeps everything.

## Launch

```bash
brew install flyctl jq
fly auth login
GENESIS_ONLY=1 deploy/launch.sh   # rehearsal: keys, operator files, spec and genesis, no Fly calls
scripts/rehearse-testnet.sh       # rehearsal: that genesis's shape on 4 local validators + public nodes
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
| `UNBONDING_BLOCKS` | `604800` (a week). The evidence window is the same length, `UNBONDING_BLOCKS / EPOCH_BLOCKS` epochs |
| `FAUCET_FANG` | `2500000` per validator's consensus-key account |
| `SELF_BOND` | `10000` FANG per owner (the default `min_self_bond`) |
| `POOL_FANG` | `100000`: the contribution pool's opening balance, out of the endowment |
| `POOL_REPOS` | `"jackdishman/blockula:10"`: the pool's repos at genesis, `owner/name:weight` |
| `JURORS_FILE` | unset: the founding jurors, one per line, `tboo1… <soul> <reason>` (`#` comments). Never the founder (docs/TOKENOMICS.md, section 7) |
| `JURORS`, `JUROR_SOUL` | `3`, `25`: without `JURORS_FILE`, stand-in jurors made on this laptop (`juror-N.json`) with this much Soul |
| `DOWNTIME_MAX_MISSED`, `DOWNTIME_WINDOW` | half an epoch's blocks over one fewer than the validators (`600` for four), and twice that (`1200`): about half an epoch away jails a validator ([OPERATIONS.md, Jailed or tombstoned](../docs/OPERATIONS.md#jailed-or-tombstoned)) |
| `EXTRA_PARAMS` | unset: more `[params]` lines for the spec (`'jail_blocks = 20\n…'`); not the downtime ones, which have their own variables |
| `VALIDATOR_VM`, `VALIDATOR_MEMORY`, `RPC_VM`, `RPC_MEMORY`, `VOLUME_GB` | `shared-cpu-2x`, `2048mb`, `shared-cpu-1x`, `1024mb`, `10` |
| `RPC_VOLUME_GB` | `VOLUME_GB` (applies when the volume is created) |
| `VALIDATOR_PRUNE_BLOCKS` | unset: the node's default (~2 days) |
| `RPC_PRUNE_BLOCKS`, `RPC_ARCHIVE` | `1209600` (14 days), unset. `RPC_ARCHIVE=1` keeps every block |
| `HALT_HEIGHT` | unset. Set for a coordinated stop: validators run `--halt-height` (OPERATIONS.md "Incidents") |
| `NETWORK_HALTED_AT` | unset. Set when the whole network is halted at a height and you've checked: validators run `--network-halted-at` so slashing protection may sign again without a peer confirming it (OPERATIONS.md "How slashing protection works"). Remove it once the chain runs |
| `IPS`, `P2P_ADDRS`, `RPC_URL`, `REHEARSAL_DIR`, `SKIP_BUILD` | Rehearsals (`GENESIS_ONLY=1`): placeholder IPs, explicit `host:port`s, the published HTTP endpoint, the output directory, and skipping `cargo build` |
| `GENESIS_MAX_AGE_HOURS` | `24`: a genesis an earlier, failed attempt built longer ago than this isn't launched (its clock would start that far behind); delete `genesis.json` and `spec.toml` to rebuild it, keys kept |
| `GATE_SECS`, `PROGRESS_SECS` | `600`, `5`: how long the [participation gate](#restarting-validators) waits for the validators before and after each restart, and the seconds between its checks (also for `set-github-token.sh`) |

The node's own settings come through `deploy/entrypoint.sh`'s environment: `ROLE`, `P2P_PRESET`, `CLIENT_IP_HEADER`, `PEER_HTTP`, `BIND`, `HTTP_PORT`, `GENESIS_HASH`, `CHECKPOINT`, `HALT_HEIGHT`, `NETWORK_HALTED_AT`, `PRUNE_BLOCKS`, `ARCHIVE` (its header says what each does). `deploy/fly-config.sh` writes the machine configurations from launch.sh's settings; `bash deploy/test-deploy.sh` tests both offline.

## Keys and secrets

Everything is generated once in `~/.config/blockula/$CHAIN_ID` (mode 700):

- **Consensus keys** (`v*/validator.key`, `v*/consensus.json`): each one is uploaded only to its own app, as the Fly secret `VALIDATOR_KEY_B64` (through stdin, never argv), which the machine gets as an environment variable. On first boot the entrypoint writes it onto that app's volume (mode 600) and drops the variable before the node starts; then **launch.sh deletes the secret from Fly**, restarting the validators without it all together, once: on a chain nobody uses yet, one pause and about one missed slot each, instead of a gated restart per validator. (One at a time, behind a gate that didn't wait for p2p links, is what twice took two of four validators out at once at the `blockula-testnet-2` launch.) It restarts nothing unless the public node and each of those validators answer for the genesis, so the keys stay for a rerun; afterwards it waits at the [participation gate](#restarting-validators) until every validator is back, and fails naming the one that isn't. Nothing in the machines' configuration refers to the secret, so restarts after that (`set-github-token.sh`, `fly machine restart`, a later `launch.sh`) start from the volume alone. A cloned, scaled or replacement machine with an empty volume refuses to start instead of double-signing. The local copies are your backup.
- **Owner accounts** (`owner-*.json`), and the founder, grants and juror stand-ins (`founder.json`, `grants.json`, `juror-*.json`; `blockula-testnet-2` also has `foundation.json`): never leave this laptop. An owner's allocation vests (the operators bucket's 6-month cliff), so it can bond and pay fees but not transfer yet. Back them up.
- **Genesis inputs** (public): `operators/v*.toml` (each validator's entry with its proof of possession), `spec.toml`, `genesis.json`. `blockula genesis build --spec spec.toml` rebuilds `genesis.json` byte for byte. (`blockula-testnet-2`'s spec gives the `foundation` bucket 5%, which DEC-5 has moved to the treasury since: it still rebuilds byte for byte, with a warning about both buckets' shares.) They're committed in the release after the launch (`scripts/pin-network.sh --genesis`), and the keys next to them never are.
- **Containers** run as an unprivileged `blockula` user.

## Restarting validators

A restarted validator is back when it signs, is linked to the others and proposes again, not when its node serves new blocks: it fetches those over HTTP before consensus starts, on its own. Simplex sends proposals and votes over direct p2p links only, so with four validators a second one restarted before the first has relinked leaves no quorum. `launch.sh` (after the key removal) and `set-github-token.sh` (before anything restarts, then after each app) therefore wait at the participation gate, `deploy/restart-gate.sh`, until one round of reads shows:

- the public node answering for the genesis, its newest block at most 10 s old, and its last 20 blocks within 25 s: full pace (19 s at 1 s blocks, so one view timing out still passes; a leader whose proposals can't reach a quorum times out every lap of four views, ~40 s);
- every validator, asked on its own machine (`fly ssh console -a blockula-v0 -C "blockula status --rpc http://127.0.0.1:7070"`: the image has no curl), answering for the genesis, in the active set, signing (slashing protection no longer watching after its start), not catching up, and linked to every other active validator (`p2p_active_peers` at least active − 1: `p2p_peers` also counts the last epoch's set, candidates and bootstrappers);
- the active set being exactly the validators the script runs (one it doesn't run would go unread, and the next restart could leave the rest short of a quorum);
- each validator just restarted running a new process (its `started_at` moved) that has proposed a block since it started.

Anything that doesn't answer counts as not back. The gate waits up to `GATE_SECS` (600) per restart, checking every `PROGRESS_SECS` (5), or stops at once for what waiting can't fix (a jailed validator, another genesis, a deployed `blockula` without `status`, an active set with validators it doesn't run: restart the validators together then); then the script stops, and `set-github-token.sh` leaves the apps it hasn't restarted untouched. With the `public-nat` preset a restart passes in 15–35 s in the local rehearsal; with Commonware's `recommended` reconnection limits (`public-nat`'s until they were shortened) it can take minutes. `blockula status` is also how to look by hand: it prints the node's own state as one line of JSON.

## Sync trust

A node with an empty home refuses to sync from a root older than the weak-subjectivity period: half the evidence window, **3.5 days** here, against ~10.5 on mainnet ([OPERATIONS.md, Weak subjectivity](../docs/OPERATIONS.md#weak-subjectivity)). Past it, keys of validators that have since unbonded could sign a made-up history, and a node that trusts only the genesis can't tell.

- **The public node** is rebuildable at any time: with an empty home, the entrypoint asks `blockula checkpoint --peer <each validator> --genesis-hash <the genesis>` for the checkpoint every validator agrees on, over Fly's private network, and joins with it (`CHECKPOINT` overrides it). That's the operator trusting its own validators for a node it runs itself, which is what "a checkpoint from sources you trust" means for it; it isn't evidence for anyone else. If no validator answers, it joins from the genesis alone, which works only while the chain is younger than the period (otherwise `join` refuses and the machine retries under its restart policy).
- **Everyone else** joins with a recent checkpoint: `blockula checkpoint --peer https://blockula-testnet.fly.dev --genesis-hash <hash>`, or the one compiled into the latest release. The testnet has one operator, so any checkpoint is that operator's word: ask several of its nodes if you like, but the agreement OPERATIONS.md asks for (independent operators) comes with the incentivized testnet. A release's compiled-in checkpoint stays fresh for 3.5 days after it was taken.

## Relaunch (replacing `blockula-testnet` with `blockula-testnet-2`)

The v0.4 testnet (`blockula-testnet`, `boo` addresses) can't run protocol v1, and its chain id is never reused. Order matters: clients pin the chain, so they're released after the new chain exists.

1. **Rehearse locally:** `GENESIS_ONLY=1 deploy/launch.sh` (writes `~/.config/blockula/blockula-testnet-2-rehearsal`, placeholder IPs), then `scripts/rehearse-testnet.sh` (the same spec's shape on this machine, with short windows: blocks finalize, the faucet, the pool, a jailed validator rejoining, then under Fly's p2p preset every validator restarting at once and then one at a time through the [participation gate](#restarting-validators) without a stall or a jailing, a join after the weak-subjectivity period, the SDK e2e). The genesis builder's warnings should be exactly these (a testnet breaks mainnet's independence policy and opens the pool early):
   - `the contribution pool opens with 100000 FANG out of the endowment, which DEC-5 releases linearly from zero`
   - `4 operators; a mainnet genesis needs at least 22 (DEC-3)`
   - `hosting provider fly.io has 4 of the 4 operators; at most 1 may`, and the same for `ASN 40509`
   - `data-center country us has 2 of the 4 operators; at most 1 may` (iad, sjc)
   - `facility fly-<region> has 1 of the 4 operators; at most 0 may`, once per region (15% of 4 rounds down to 0)
   - `legal entity blockula (testnet) has 4 of the 4 operators; at most 1 may`
2. **Launch:** `RESET=1 deploy/launch.sh`. `RESET=1` destroys the apps' old machines, volumes and secrets (the v0.4 chain's data); it's refused once `blockula-testnet-2` has launched, which it has from the moment its validators are deployed. The script allocates IPs, builds the genesis with the real IPs, stages each consensus key, builds the image, deploys, removes the keys from Fly (one restart of the validators together, then the [participation gate](#restarting-validators)), and prints the **genesis hash**. If it fails before the validators are deployed (the image build), fix the cause and run the same command again: `RESET=1` stages the keys again. If it fails after, run `deploy/launch.sh` without `RESET`: it finishes this launch (a stop before the key removal's restart leaves the keys in Fly until then). Later upgrades: `deploy/launch.sh` without `RESET`.
3. **Check:** `curl https://blockula-testnet.fly.dev/api/status` shows `chain_id: blockula-testnet-2`, `address_prefix: tboo`, the genesis hash, `version: 0.5.0`, a rising height and 4 active validators; `/api/validators` lists each with its owner (and, after the first hour's boundary, 25M FANG of bootstrap stake each); `/api/contributions` shows the repo and the pool's balance; `/api/fees`, `/api/proposals` and `POST /api/faucet` work.
4. **Wallet (Vercel env), right away:** `NEXT_PUBLIC_TESTNET_GENESIS_HASH=<hash from step 2>`, then redeploy. (The chain id `blockula-testnet-2` and prefix `tboo` are the code's defaults; leave `NEXT_PUBLIC_TESTNET_WITNESSES` unset: there's one public node.)
   Until the release pins the genesis clock in step 5, the wallet learns it from the node on first use. Step 5 pins both the hash and clock in the wallet and MCP; no extra Vercel clock variable is needed.
5. **Release v0.5.0** (the first release with the protocol v1 testnet; versions are 0.5.0 everywhere `release.yml` and `npm-publish.yml` check):
   1. Once the chain has passed its first epoch boundary (an hour after the launch), build `node/target/release/blockula` from this reviewed protocol-v1 branch (`cargo build --release --locked --manifest-path node/Cargo.toml`), not an older installed release. Then `scripts/pin-network.sh blockula-testnet-2 <hash> --genesis ~/.config/blockula/blockula-testnet-2/genesis.json --checkpoint "$(node/target/release/blockula checkpoint --peer https://blockula-testnet.fly.dev --genesis-hash <hash>)"` writes `node/checkpoints/blockula-testnet-2.json` and its `EMBEDDED` line (`node/src/checkpoints.rs`), the MCP's `KNOWN_GENESIS` and `KNOWN_CLOCKS` (`mcp/src/network.ts`), the wallet's `TESTNET_GENESIS_HASH` and `TESTNET_GENESIS_CLOCK` (`web/lib/config.ts`), and `genesis/blockula-testnet-2/` (`genesis.json`, `spec.toml`, `operators/`: what `release.yml` ships as `genesis-blockula-testnet-2.json`, and what anyone rebuilds it from). Checkpoints have four fields: `HEIGHT:HASH:SET_HASH:TIMESTAMP_MS`. The tests read those tables, so nothing else changes (`checkpoints::tests::shipped_genesis_files_are_the_pinned_ones` checks the committed genesis is the pinned one, validates its embedded trust table and rebuilds from its spec). Run `cargo test --locked --manifest-path node/Cargo.toml`, `npm run build -w @blockula/sdk && npm test -w @blockula/mcp` and `npm test -w web`, and open the release PR. (Rehearsed on a `GENESIS_ONLY` genesis: `GENESIS=<rehearsal>/genesis.json bash scripts/test-pin-network.sh`.)
   2. Tag `v0.5.0` on the merge commit, then follow OPERATIONS.md "Releases" (draft, reproduce, check, publish; before the key ceremony the release is unsigned, OPERATIONS.md "A first install"), which publishes the npm packages (`npm-publish.yml`).
   3. Take the checkpoint within a day of the tag: the release's checkpoint is fresh for 3.5 days.
6. **Redeploy from the release commit:** `deploy/launch.sh` (no `RESET`), so the nodes run the tagged code (`/api/status` `git_sha`).
7. **Optional:** give validators a GitHub token for contribution claims (`pbpaste | deploy/set-github-token.sh`), and publish the genesis hash where users look (README, the explorer's status).
8. **Monitoring:** Fly scrapes every app's `/metrics` (the `[metrics]` section; Fly's managed Grafana). For the reference stack with its alerts (`deploy/monitoring`), run it on a machine in the org's private network (`fly wireguard create`) with the targets launch.sh wrote to `~/.config/blockula/blockula-testnet-2/prometheus-targets.yml`.

**Release notes for the relaunch:**

- **MCP (`@blockula/mcp` 0.5.0):** the `testnet` network pins `blockula-testnet-2`, the `tboo` prefix and its genesis hash (`KNOWN_GENESIS`). An entry remembered for the old `blockula-testnet` isn't held against the new chain. Users who pinned the old chain by hand (`BLOCKULA_CHAIN_ID=blockula-testnet` or `BLOCKULA_GENESIS_HASH`) must drop or update those variables. Old testnet wallets and balances don't carry over: agents run `create_wallet` and `faucet` again (addresses now start with `tboo1`).
- **Web wallet:** testnet defaults `blockula-testnet-2` / `tboo`, and the genesis hash from Vercel's env until the release builds it in. Passkeys keep working (the key doesn't depend on the chain), but accounts start empty and display `tboo1…` addresses.
- **SDK (`@blockula/sdk` 0.5.0):** no change needed: `Client` reads the prefix from `/api/status`; apps that pin should pass `{ chainId: "blockula-testnet-2", genesisHash, prefix: "tboo" }`.
- **CLI and docs:** `blockula join --genesis https://blockula-testnet.fly.dev --checkpoint <recent>` (v0.5.0 knows the chain's genesis; earlier binaries also need `--genesis-hash <hash>`). Faucet, Hunts and Soul from the old testnet are gone.

## Day 2

| Task | Command |
|---|---|
| Upgrade all nodes | `deploy/launch.sh` |
| A protocol upgrade (a new protocol version) | As on mainnet: a `ScheduleUpgrade` proposal the council or token holders pass (OPERATIONS.md "Upgrades"), then `deploy/launch.sh` with a release that supports it, before the activation height. A validator on an older binary stops at the height (exit 11) and stays down until it's redeployed |
| Start a brand-new testnet | a new `CHAIN_ID` with `RESET=1 deploy/launch.sh` (then the relaunch steps above) |
| Coordinated stop at a height | `HALT_HEIGHT=<h> deploy/launch.sh`; to resume, run it again without `HALT_HEIGHT` |
| Resume a network that halted by itself | Once you've checked every validator is stopped at the same height: `NETWORK_HALTED_AT=<h> deploy/launch.sh`; then `deploy/launch.sh` without it once blocks finalize |
| A validator jailed for downtime | It leaves the set at the next boundary (the bootstrap delegation moves off it). Once `/api/validators` shows its jail over: top up its self-bond if the slash took it under 10,000 FANG (`blockula stake delegate <validator> 20 --key ~/.config/blockula/blockula-testnet-2/owner-N.json --rpc https://blockula-testnet.fly.dev`), then `blockula validator unjail <validator> --key …/owner-N.json --rpc …`. It rejoins at the next boundary |
| Rebuild the public node | `fly machine destroy <id> -a blockula-testnet --force` and `fly volumes destroy <volume> -a blockula-testnet` (`fly machines list` / `fly volumes list` show the ids), then `deploy/launch.sh`: it creates a fresh volume (`RPC_VOLUME_GB`) and machine, which joins with a checkpoint its validators agree on, however old the chain is ([Sync trust](#sync-trust)). Never do this to a validator |
| Logs | `fly logs -a blockula-v0` |
| Status | `curl https://blockula-testnet.fly.dev/api/status` |
| Give validators a GitHub token (contribution claims, link checks) | `pbpaste \| deploy/set-github-token.sh` (fine-grained, public-read-only token; restarts apps one at a time, each through the [participation gate](#restarting-validators)) |
| Founding jurors | Each juror account takes the faucet, then `blockula contrib join-jury --key ~/.config/blockula/blockula-testnet-2/juror-N.json --rpc https://blockula-testnet.fly.dev` (a 10 FANG bond) before the Séance that should seat it |
| Add one of *our* validators later | `fly apps create`, a volume (`--scheduled-snapshots=false`) and a dedicated IPv4, then the normal "Become a validator" flow (register with a self-bond; it joins by stake at an epoch boundary). No new genesis needed |

## Caveats

- Upgrades redeploy validators in parallel, so block production pauses until they've restarted, relinked over p2p and seen the network standing still (slashing protection's ~20 s).
- Each validator app must stay at exactly **one machine** (`--ha=false`). Never `fly scale count`, clone a validator machine, or restore a volume snapshot: two copies of a validator key double-sign. Validator volumes are created with scheduled snapshots off (and launch.sh turns them off on older volumes); the entrypoint and node guard against the common cases, but don't rely on that.
- **Deliberate stops stay down.** A validator that exits with 10 (`--halt-height`), 11 (the chain needs a newer binary) or 12 (slashing protection saw its key sign elsewhere) isn't restarted: the entrypoint logs why and idles, and the p2p service doesn't auto-start machines. Crashes (any other exit) restart under the `on-failure` policy. Deploying a new image or `fly machine restart` starts the node again once you've acted. Exit 12 also holds across those: the node records it in `/data/home/double-sign-halt.json` and exits 12 again on every start, so a redeploy, a restart or a host move can't bring it back next to the other instance. Find the other instance first (OPERATIONS.md), then `fly ssh console -a <app> -C "rm /data/home/double-sign-halt.json"` and `fly machine restart`.
- If a validator's volume is lost, its key is gone from Fly by design. Re-seed it deliberately from your local backup (`printf 'VALIDATOR_KEY_B64=%s\n' "$(base64 < ~/.config/blockula/blockula-testnet-2/vN/validator.key | tr -d '\n')" | fly secrets import -a blockula-vN`), then re-run launch.sh, which deletes the secret again once it's on the volume.

## Monitoring and operations

- [`monitoring/`](monitoring/README.md): the reference Prometheus, Alertmanager and Grafana stack every operator runs, with alert rules and a dashboard.
- [docs/OPERATIONS.md](../docs/OPERATIONS.md): hardware, ports, time, sentries, keys and restores, upgrades, incident response, runbooks and releases. Mainnet validators follow it rather than this testnet setup (one org, keys generated on one laptop).

### External watch

[`.github/workflows/testnet-watch.yml`](../.github/workflows/testnet-watch.yml) checks the public node from GitHub's runners, outside Fly, every 5 minutes as GitHub's scheduler allows ([`scripts/testnet-watch.sh`](../scripts/testnet-watch.sh)): `/api/health` answers 200 with `ok: true`, `/api/status` is `blockula-testnet-2`, its finalized height advances over 30 s, its head is at most 60 s old, at least 4 validators are active, and `/api/validators` lists none jailed or tombstoned. A failing check opens an issue labeled `testnet-alert` that @-mentions @jackdishman, so the alert arrives as a GitHub notification (an email, and a push with GitHub Mobile, as the maintainer's notification settings say). While it's open, a failing run adds its report there as a comment when the failing checks change, and otherwise at most hourly (`REMIND_SECS`), since each comment notifies again; the first run that passes every check comments "Recovered" and closes it. The run stays green once the alert is delivered and goes red only when a `gh` call fails, a broken alert path. Its token has `issues: write` and nothing else.

**Test delivery** after setting it up, or after changing notification settings: `gh workflow run testnet-watch.yml -f test_alert=true`, then confirm the email or push arrived. The test issue says it's a test; the next scheduled run that passes every check comments "Recovered" and closes it, which tests the recovery path too. Locally, `DRY_RUN=true scripts/testnet-watch.sh` runs the checks and prints the `gh` calls instead of making them; `bash scripts/test-testnet-watch.sh` (CI, `ops`) tests it offline against a mock node and a stand-in `gh`.

**Limits.** It's a free safety net, not a pager. GitHub's scheduler can delay scheduled runs or drop them under load (and turns a public repository's schedules off after 60 days without activity), so an outage can surface late or not at all. On `*/10`, GitHub started 1 of about 58 scheduled runs in the watch's first ten hours (October 19, 2026); it now runs every 5 minutes and avoids the top of the hour, GitHub's busiest time, but count each run as a bonus. It sees only the public node, not each validator: one validator down behind a working quorum shows only once it's jailed. A notification doesn't escalate until someone acknowledges it. For the testnet, a hosted uptime check on `/api/health`, outside both GitHub and Fly, that emails or pages is the dependable layer; mainnet operators still need the reference Prometheus/Alertmanager stack with a paging receiver and an external heartbeat ([monitoring/](monitoring/README.md), [OPERATIONS.md "Monitoring"](../docs/OPERATIONS.md#monitoring)).
