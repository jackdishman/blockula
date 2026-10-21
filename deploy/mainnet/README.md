# Launching mainnet (`blockula-1`) on Fly

The launch-day runbook for an **experimental, upgradable mainnet** run by a small founder-run validator set: the founder's four validators on Fly, maybe one or two outside operators, and two public RPC nodes. It's the compressed version of [docs/GENESIS_CEREMONY.md](../../docs/GENESIS_CEREMONY.md) and [docs/OPERATIONS.md](../../docs/OPERATIONS.md), with the policy gates this launch waives listed in [What's waived](#whats-waived).

| What | Where |
|---|---|
| The launcher | [`launch-mainnet.sh`](launch-mainnet.sh): `prepare`, `image`, `launch`, `upgrade`. Its header lists every setting; `bash deploy/mainnet/test-launch-mainnet.sh` tests it offline (CI, `ops`) |
| The image | [`Dockerfile`](Dockerfile): the signed release's own Linux binary on a digest-pinned base, never a source build |
| The genesis | `genesis/blockula-1/` (genesis.json, spec.toml, operators/), committed by the pin PR; [`genesis-spec.example.toml`](genesis-spec.example.toml) shows the spec format |
| The pins | `CONFIRM_MAINNET=<hash> scripts/pin-network.sh blockula-1 <hash> --mainnet …` ([GENESIS_CEREMONY.md, section 6](../../docs/GENESIS_CEREMONY.md#6-publish)) |
| The external watch | `.github/workflows/mainnet-watch.yml` ([Monitoring](#monitoring)) |

## What this mainnet is

- **Chain id `blockula-1`, addresses `boo1…`**, a 1B FANG supply in the DEC-5 buckets, no faucet, no admin keys. Every binary refuses `blockula-1` until the release that pins its genesis hash.
- **Born at protocol v2** (`protocol_version = 2` in the spec: the genesis schedules v2 from block 1). Programs v0 (third-party WebAssembly programs) ship dormant in the launch binary if the [October 28 go/no-go](#calendar-october-20-to-31) passes (option A), else with the release that brings v3 (option B: the launch binary then carries only the activation rule below). The `programs` feature is **off at genesis** (`features = []`: this release refuses a `blockula-1` genesis that lists it), and on `blockula-1` governance switches it on **only from protocol v3** (`upgrade::check_may_activate`), the version that ships what the commissioned audit asks for: see "Turn programs on" below. Until it's active, v2 changes nothing in state or behaviour, and nodes refuse program transactions (tags 62+). Training wheels: a value cap (10M FANG by default, which the council can tune) and the validators' pause (`ProgramsPause`, tag 69). Ghost Mode stays off: the builder never lists it, and `blockula-1` refuses to switch it on.
- **Governance works from block 1.** Each genesis validator's consensus-key account gets a small liquid **operator float** out of the operators bucket (the spec's `operator_float`), and the spec sets a low `proposal_deposit`, so the bootstrap council can propose and vote at once. Every other genesis allocation vests (DEC-5).
  - A validator's council vote is a `Vote` from its consensus-key account, which holds the float and which nothing else spends from (the node's own system transactions are paid from its owner's account): `blockula gov vote <id> aye 1 --conviction 0 --digest <digest from gov show> --key <that seat's validator.key> --network mainnet --rpc https://$RPC_DOMAIN`. Proposals go the same way (`blockula gov propose … --key …`, the 10 FANG deposit from the float).
  - That key comes from its encrypted backup onto the launch laptop for the vote and is wiped after. A copy that only signs transactions can't double-sign; only a second node running it can.
  - **On `blockula-1` the owner outranks the key.** A seat holds its owner's vote over its consensus key's: the consensus-key account's `Vote` fills the seat only while the owner hasn't voted on that proposal, and the owner's `Vote` replaces either (`governance::owner_outranks_key`; every other chain keeps the latest vote from either account). So the key votes, and the owner (cold, offline) votes only to overrule it: if a consensus key may have leaked, its owner votes on every open proposal, and nothing the key sends after that takes the seat back (its own FANG still counts as its token vote). An owner's `Vote` races the node's system transactions for the owner's nonces: check it landed (`gov show`) and send it again if it didn't.
- **Founder seats** aren't flagged `founder_or_foundation`. Each is owned by its own operators-bucket owner account, commission 0, and disclosed publicly as founder-run. Flagged seats couldn't rejoin a set this small after leaving it (DEC-3's cap is min(2, f − 1) = 0 for six or fewer).
- **The spec records the waivers.** The genesis builder is always strict for `blockula-1`: every rule this set breaks needs a `[[waiver]]` with its reason (`rule = "operator-minimum"`, `provider-cap`, `asn-cap`, `country-cap`, `facility-cap`, `one-seat-per-entity`: [below](#genesis-parameters-to-get-right)). Every build prints `WAIVED: …` for each, and a waiver that matches nothing is an error, so what the spec waives is exactly what the set breaks.
- **Validators:** 4 is the right size, or 7 if three outside operators are ready. Five or six tolerate no more faults than four (f = 1), and with six, two down halts the chain.

| Active validators | Quorum | Down before it halts |
|---|---|---|
| 4 | 3 | 1 |
| 5 | 4 | 1 |
| 6 | 5 | 1 |
| 7 | 5 | 2 |

## Genesis parameters to get right

These can't be fixed quickly later. The downtime, evidence and bootstrap-commission parameters are the token holders' alone (a token supermajority, never the council), and DEC-5's locks keep a token quorum out of reach for months. [`genesis-spec.example.toml`](genesis-spec.example.toml) sets each of them as below.

- **The spec's top level:** `chain_id = "blockula-1"`, `address_prefix = "boo"`, a fixed `genesis_time_ms` (never "now"), `strict = true`, `faucet = false`, `validator_admins = []`, `protocol_version = 2`, `features = []` (whatever the audit says: `Genesis::check` refuses `programs` in `blockula-1`'s genesis on this release), and `operator_float = "100"` (liquid FANG on each genesis validator's consensus-key account, out of the operators bucket; at most 1,000).
- **`[params]`**, from here to the evidence window: **`proposal_deposit = "10000000000"`** (10 FANG, in drops; the floor is 1 FANG), so the floats pay deposits.
- **Governance at its floors** for faster upgrades: `voting_blocks = 259200` (3 days), `timelock_blocks = 86400` (1 day). A `ScheduleUpgrade` then activates at least about 7 days after it's proposed. At the defaults (7 days and 2 days) it's about 16.
- **Downtime:** `downtime_max_missed = 600`, `downtime_window = 1200` for 4 validators (about half an epoch away jails one), `450`/`900` for 5, `360`/`720` for 6, `300`/`600` for 7. The default 50 of 100 jails one of four after about 3 minutes away ([OPERATIONS.md, Jailed or tombstoned](../../docs/OPERATIONS.md#jailed-or-tombstoned)).
- **`bootstrap_commission_cap_bps = 0`.** The bootstrap delegation (100M FANG, DEC-5) backs the founder's four unflagged seats, and an owner may raise its validator's commission without any vote (`UpdateValidator`, 1% an epoch). Above 0, the founder seats' owners would hold a lever nobody votes on over the delegation's rewards, which DEC-5 gives the treasury: at the default 10%, about 4M FANG in year 1. At 0 the delegation pays no commission whatever a validator charges. It's a validators' parameter, so only a token supermajority changes it later (outside operators get no commission on the delegation either until then). The builder refuses a `blockula-1` set with several seats of one entity that the delegation backs above 0 unless the spec waives `bootstrap-commission`: don't waive it.
- **`bootstrap_end_height`:** the council ends there (365 days by default; the example spec's 63,072,000 is 730 days, and a vote can only shorten it). After that, without a token quorum, no upgrade can pass. Pick it knowing that.
- **Evidence window and unbonding:** the defaults (21 days, so the weak-subjectivity period is about 10.5 days). Never below 14 days.
- **The `[[waiver]]`s**, each a `rule` and a one-line `reason`. The builder prints each as `WAIVED: <rule>: <finding> (<reason>)`. The founder's four seats on Fly break `operator-minimum`, `provider-cap`, `asn-cap`, `country-cap` (iad and sjc are both in the US), `facility-cap` and `one-seat-per-entity`, and the spec waives exactly those. The other waivable rules mean the spec is wrong, so fix it instead: `founder-seats` and `rejoin-trap` (a seat is flagged `founder_or_foundation`: unflag it), `fee-headroom` (an owner keeps too little unstaked to pay its validator's system transactions) and `bootstrap-commission` (above). `independence-fields`, `bucket-shares` and `opening-balance` can't be waived, and `faucet`, `admin-keys`, `total-supply`, `prefix`, `governance-floors` and `ghost` are refused outright.
- **Each operator file's addresses** are signed into its proof of possession: `p2p_addr` is the app's dedicated IPv4 from `prepare`, and `http_addr` is the public RPC's **custom domain** (`https://rpc.blockula.app`), not `*.fly.dev`. Changing either later takes an owner-signed `update_validator` per validator.

## Calendar (October 20 to 31)

Genesis time **T** is October 31. The ceremony's calendar is ten weeks ([GENESIS_CEREMONY.md](../../docs/GENESIS_CEREMONY.md#calendar)); this is eleven days, with the gaps in [What's waived](#whats-waived).

| When | What | Done when |
|---|---|---|
| Oct 20–22 | Land the code: the builder's waivers, floats and v2 genesis (with the rule that switches programs on for `blockula-1` only from v3), the programs engine crate, this launcher. Programs v0's node integration continues on `automation/programs-v0` toward the October 28 go/no-go. Commission the audit (its scope: the release candidate the code freeze tags). Create the Fly org, then `launch-mainnet.sh prepare` (apps, volumes, IPs, certificates) and set the RPC domains' DNS. | CI green on main; `prepare` printed each validator's p2p address |
| Oct 22 | **Release-key ceremony** on the offline machine ([OPERATIONS.md, The release key](../../docs/OPERATIONS.md#the-release-key)), then the key-id PR: the three built-in copies (`node/src/release_cli.rs`, `scripts/install.sh`, `mcp/src/release.ts`), SECURITY.md and OPERATIONS.md. Publish the key id on independent channels. The release's tests don't depend on it ([below](#release-signing)). | The key id published in at least three places that agree |
| Oct 23 | **Code freeze**, for everything but programs v0's integration (`node/src/programs/` and its hooks), which freezes at the October 28 go/no-go. Tag the release candidate (a plain `vX.Y.Z`: release tags have no `-rc`). Release it as in [OPERATIONS.md, Releases](../../docs/OPERATIONS.md#releases): reproduced twice, checked, signed offline. From here to the launch release nothing may change what the genesis builder writes: the launch release's tests and the launcher rebuild the genesis byte for byte. | The signed release candidate |
| Oct 23–24 | **Operator files.** For each founder seat, on the offline machine: `blockula keygen` twice (consensus and owner) and `blockula genesis operator --chain-id blockula-1 --p2p-addr <its IPv4>:30300 --http-addr https://<RPC domain> --commission-bps 0 …` ([GENESIS_CEREMONY.md, section 2](../../docs/GENESIS_CEREMONY.md#2-offline-key-generation-each-operator-alone)); encrypted backups of both keys in two places. Outside operators send theirs. | Every operator file, PoPs verified by the builder |
| Oct 24–26 | **Dry run** on `blockula-dryrun-1` (prefix `tboo`), with dry-run keys made the same offline way, in a separate org and prefix, for at least 24 h: `launch` started inside its window before genesis time, as on the day (note how the RPC nodes' deploys and the gate behave before T: that start is untested live), a gated `upgrade`, a coordinated stop at a height, an RPC rebuild and the archive's restore from a volume snapshot, a validator re-seeded onto a new volume, and a council proposal (its spec may shorten voting and the timelock to minutes: governance's floors bind `blockula-1` only). The release candidate doesn't carry programs' integration, so `ActivateFeature programs` runs on a second dry-run chain from the launch release (October 29, option A only). Use `UNSIGNED_RELEASE=1` only if it runs before the release key exists. | 24 h of finality, every step done |
| Oct 25, evening | **Programs checkpoint** (option A): stages S3 and S4a merged on `automation/programs-v0` with their acceptance. A miss takes option B now: the launch release carries only the activation rule, and the integration ships with v3. | The checkpoint and its evidence in the decisions log |
| Oct 26 | **Spec freeze.** `features = []` whatever the audit's verdict (it decides when programs switch on, under v3). Two maintainer approvals on the spec PR. | `genesis/blockula-1/spec.toml` frozen; its SHA-256 recorded |
| Oct 27 | **Genesis build on two machines**, each from the signed release candidate ([GENESIS_CEREMONY.md, section 5](../../docs/GENESIS_CEREMONY.md#5-build-it-several-times)): the same genesis.json SHA-256 and genesis hash on both. Publish the hash. | Two signed `builds/<builder>.txt` that match |
| Oct 28 | **Pin PR:** `CONFIRM_MAINNET=<hash> scripts/pin-network.sh blockula-1 <hash> --mainnet --genesis <genesis.json> --rpc https://<RPC domain>`. In the same PR, the node tests it names: delete the placeholder assertion (`checkpoints::tests::embedded_files_parse_and_mainnet_waits_for_its_ceremony`), and have the stand-in mainnet nodes of `cli_net::tests::mainnet_is_never_trusted_on_first_use` and `validator_commands_sign_only_addresses_they_were_given` report the pinned genesis. The MCP's and the wallet's tests read the new pins and pass as they are, before genesis time too. | CI green, merged |
| Oct 28, evening | **Programs go/no-go** (option A), all of: stages S1 to S4b, S5, S6a and S7a merged on `automation/programs-v0` with their acceptance and reviewed; the v1 replay gate green on the branch rebased onto main; simulator scenarios 1, 2 and 4 green; the codec fuzzers 24 hours clean on the final codec; no open high finding on the dormant surface. Go: the branch merges for the launch release (programs stay off at genesis, and nothing the genesis builder writes changes). Any miss: option B, by not merging it. | The decision and its evidence in the decisions log |
| Oct 29 | **Launch release:** tag, `release.yml`, two reproductions, `check-release-files.sh`, sign offline, publish, announce (tag, commit, key id, archive hashes, genesis hash) on channels GitHub doesn't control. **npm:** `npm-publish.yml` from the tag, a dry run first. Install it on the launch laptop: `blockula release install vX.Y.Z --key-id <K>` (that's `VERIFIER`). `launch-mainnet.sh image` to prove the image builds. Under option A, start a second dry-run chain from it (born at v2, programs off) and pass `ActivateFeature programs` there by council vote. | The release public; `image` built |
| Oct 30 | **The plan, keys still encrypted:** `DRY_RUN=1 deploy/mainnet/launch-mainnet.sh launch` with `KEYS_DIR` unset. It reads no key (it says so), runs only fly's reads, and prints every fly command with each deploy's configuration under it. Review: each app's IPv4 matched to its genesis validator, `OUTSIDE_VALIDATORS` accepted, the six deploys (the two RPC nodes, then the four validators), `size = "performance-4x"` and `memory = "16gb"`, `PRUNE_BLOCKS = "43200"` on the validators and `ARCHIVE = "1"` on the archive, no `HALT_HEIGHT`, and the "launch itself refuses until then" line naming when the window opens. Configure the external monitors (paused) and the paging receivers. The mainnet Vercel project's environment (`NEXT_PUBLIC_NETWORK=mainnet`, `NEXT_PUBLIC_RP_ID=wallet.blockula.app`, `NEXT_PUBLIC_MAINNET_RPC`, `NEXT_PUBLIC_MAINNET_GENESIS_HASH`). Merge freeze from here to T + 1 day. | The dry run's plan reviewed |
| **Oct 31** | [Launch day](#launch-day). | The chain finalizes with every validator |
| Nov 1–3 | **Release N+1** with the first embedded checkpoint (taken after the first epoch boundaries; well inside the ~10.5-day weak-subjectivity period), then a signed checkpoint every week. | The checkpoint release published |

## Launch day

Keep the settings in a file outside the repo and source it in every shell:

```bash
# ~/.config/blockula/blockula-1-fly/env  (no secrets in it)
export CHAIN_ID=blockula-1 ORG=<mainnet org> PREFIX=blockula-main RPC_APP=blockula-rpc ARCHIVE_APP=blockula-archive
export REGIONS="iad sjc fra ams" RPC_DOMAIN=rpc.blockula.app ARCHIVE_DOMAIN=archive.blockula.app
export RELEASE=vX.Y.Z VERIFIER=$HOME/.blockula/bin/blockula
export OUTSIDE_VALIDATORS=""   # boo1… of each genesis validator an outside operator runs
# KEYS_DIR only from T − 2 h, in that shell alone.
```

The launcher refuses names that are, or could become, the testnet's apps (`blockula`, `blockula-testnet…`, `blockula-v…`), the org `personal`, a dirty checkout, a checkout that isn't `RELEASE`'s tag, a release that doesn't verify against the pinned key, and any leftover `RESET`, `HALT_HEIGHT` or `NETWORK_HALTED_AT`.

1. **T − 2 h. Checkout and keys.**
   - `git fetch --tags && git checkout $RELEASE && git status` (clean).
   - Decrypt each founder seat's consensus key from its backup into `KEYS_DIR`: `v0/validator.key` … `v3/validator.key`, mode 600, directories 700, on an encrypted disk, nothing else in it. Seat `vN` is the app `$PREFIX-vN`, whose IPv4 its operator file names; the launcher checks each key against the genesis validator at its app's address and refuses a key on the wrong app.
   - `export KEYS_DIR=…; DRY_RUN=1 deploy/mainnet/launch-mainnet.sh launch` once more: now it also checks each key against its app's genesis validator (still reading no key into Fly).
   - Outside operators, on their own hosts: the launch release (`blockula release install vX.Y.Z --key-id <K>`), its `genesis-blockula-1.json` checked against the published SHA-256 and genesis hash ([GENESIS_CEREMONY.md, section 6](../../docs/GENESIS_CEREMONY.md#6-publish)), and their validator started in the same window as yours ([OPERATIONS.md, Running a validator](../../docs/OPERATIONS.md#running-a-validator)). The gate waits for each of them to propose.
2. **T − 45 min. Launch:** `deploy/mainnet/launch-mainnet.sh launch`. It refuses to start earlier than `LAUNCH_WINDOW_MINS` (60) before T. Starting validators hours early is untested: consensus proposes no block before its parent's time plus a block time, so until T views time out (a stall that charges each validator one missed slot, never more). Start inside the window. It:
   1. checks the checkout, the genesis (pinned hash, rebuilt byte for byte from its spec with `VERIFIER`), `KEYS_DIR`, and each key against its app's genesis validator, and that every other genesis validator is in `OUTSIDE_VALIDATORS`;
   2. verifies the release with `VERIFIER release fetch` (signature, tag line, SHA-256; the release's `genesis-blockula-1.json` must be the checkout's) and builds the image from that binary;
   3. deploys both RPC nodes, before any key is staged. Their Fly health check (`/api/health`) answers 503 until they serve a fresh head, so not before T (and the archive, which replays the chain from genesis, not until it catches up): their deploys don't wait for it (`--detach`), and the launcher only checks each has its machine;
   4. stages each consensus key as the Fly secret `VALIDATOR_KEY_B64` (through stdin) and deploys the validators (`performance-4x`, 16 GB, `--prune-blocks 43200`, one machine each);
   5. removes each key from Fly once its volume shows it seeded, with one restart of the validators together (before T, there's nothing to pause), waits at the participation gate until T has passed and every validator signs, links and proposes, the outside ones included (`deploy/restart-gate.sh`), and marks the launch finished (`~/.config/blockula/blockula-1-fly/finished`).

   If it stops, fix the cause and run the same command again. It deploys only apps that have no machine yet and never redeploys a running one; a failed release check or a failed RPC deploy leaves no key staged. Until the launch finishes, every validator gets the release it started with.
3. **T to T + 30 min. Check.**
   - `curl https://$RPC_DOMAIN/api/status`: `chain_id` `blockula-1`, `address_prefix` `boo`, the published `genesis_hash`, `version`, a rising `height`, and every genesis validator in `validators` (`active_validators` counts them).
   - `curl https://$ARCHIVE_DOMAIN/api/status`: the same chain, `pruning` and `fast_synced_from` both `null` (every block, replayed from genesis).
   - `/api/upgrade`: `protocol_version` 2, its `schedule` v2 from block 1. `/api/genesis`: no `features` (the spec's `[]`), `upgrades` v2 at block 1. `/api/validators`: none jailed. `/api/params`: the spec's parameters (`bootstrap_commission_cap_bps` 0, `downtime_window` 1200, …).
   - Each validator on its machine: `fly ssh console -a $PREFIX-v0 -C "blockula status --rpc http://127.0.0.1:7070"` (`signing`, `p2p_active_peers`).
   - No `VALIDATOR_KEY_B64` left: `fly secrets list -a $PREFIX-v0` for each.
   - Wipe `KEYS_DIR` (the encrypted backups stay offline).
   - Unpause the external monitors, set the repository variable `MAINNET_RPC`, and send a test alert through each path ([Monitoring](#monitoring)).
4. **T + 1 h (after the first epoch boundary).** `/api/validators` and `/api/beacon` show the new epoch; take a checkpoint: `blockula checkpoint --peer https://$RPC_DOMAIN --peer https://$ARCHIVE_DOMAIN`. Start the off-Fly copy of the chain's history on a machine outside Fly, hourly: `node scripts/archive-chain.mjs https://$ARCHIVE_DOMAIN <a directory of its own>` (it resumes where it stopped). Deploy the mainnet wallet from its locked-down Vercel project, test a passkey on real devices, then announce.
5. **T + 1 to 3 days.** The checkpoint release: `CONFIRM_MAINNET=<hash> scripts/pin-network.sh blockula-1 <hash> --mainnet --checkpoint "$(blockula checkpoint …)"`, released as usual, then `launch-mainnet.sh upgrade` with it.

### Day 2

| Task | How |
|---|---|
| A new release (a patch, a checkpoint) | `RELEASE=vX.Y.Z deploy/mainnet/launch-mainnet.sh upgrade` from that tag: the validators one at a time through the participation gate, then each RPC node. It stops at the first node that isn't back and leaves the rest untouched |
| A protocol upgrade | A council `ScheduleUpgrade` ([OPERATIONS.md, Upgrades](../../docs/OPERATIONS.md#upgrades)), then `upgrade` with a release that supports it, well before the activation height. A validator on an older binary stops at that height (exit 11) |
| Turn programs on (once the audit clears them) | Only from protocol v3 on `blockula-1` (`upgrade::check_may_activate`): a release that supports v3 (the audit's fixes, or nothing else changed when it cleared v2's rules), the council's `ScheduleUpgrade` to v3 and `upgrade` with that release before its height (about 7 days at the floors), then a council `ActivateFeature` proposal for `programs` that runs at or after v3's height (it may be proposed once v3 is scheduled; nodes refuse one that would run earlier). Each vote binds the proposal's content (`gov show`, `gov vote --digest`) |
| Pause programs | The validators' `ProgramsPause` (tag 69) |
| Outside operators join later | They register with a self-bond and join by stake at an epoch boundary. Add each one's address to `OUTSIDE_VALIDATORS`, or the gate refuses to restart anything while it's in the set |
| A jailed validator | Once its jail is over: `blockula validator unjail <validator> --key <owner key> --network mainnet --rpc https://$RPC_DOMAIN` (signing commands refuse mainnet without `--network mainnet`) |
| A coordinated stop at a height | Not in this script yet: follow [OPERATIONS.md, Coordinated restart at a height](../../docs/OPERATIONS.md#coordinated-restart-at-a-height). The launcher refuses `HALT_HEIGHT` and `NETWORK_HALTED_AT` so neither can ride along with a deploy |
| Rebuild the public RPC node | `RPC_APP` only: destroy its machine and volume by hand (`fly machine destroy`, `fly volumes destroy`; the launcher never destroys anything), then `prepare` (a new volume) and `launch` with `RELEASE` the nodes run now (a new machine, which fast-syncs from a snapshot at a checkpoint its validators agree on). With every validator deployed, `launch` needs no `KEYS_DIR` |
| Rebuild the archive | Never from an empty volume after T + 14 days: the archive replays the chain from genesis (`ARCHIVE=1` runs `--archive --full`), syncing from the validators (12 hours of blocks) and the public node (14 days), so once the public node has pruned block 1 no node on Fly can serve it. Restore its volume from its newest Fly snapshot instead (Fly snapshots volumes daily unless told not to; the launcher turns that off on validators only): `fly volumes snapshots list <its volume id>`, `fly volumes create data -a $ARCHIVE_APP -r ord -s 500 --snapshot-id <id> --yes` (before destroying anything: the snapshots may go with their volume), then destroy the broken machine and the old volume, and `launch` with `RELEASE` the nodes run now; it resumes from the snapshot's height. Before T + 14 days an empty volume works too. The off-Fly copy (`archive-chain.mjs`) keeps the history either way |
| A validator's machine is gone, its volume intact | Make sure nothing else runs that seat's key, then `launch` with `RELEASE` the nodes run now: it deploys a machine onto the volume, which still holds the key. No `KEYS_DIR` is read for it |
| A validator's volume is gone | **Never clone, fork or restore a validator volume**, and only after [OPERATIONS.md, Restoring after losing a validator host](../../docs/OPERATIONS.md#keys-backups-and-restores) step 1 (the old copy provably can't sign): destroy what's left of the app's machine and volume by hand, `prepare` (a new volume, snapshots off), then `launch` with `RELEASE` the nodes run now. It refuses a validator whose volume is newer than its key's staging (`staged-<app>` under `~/.config/blockula/blockula-1-fly`: the key was seeded onto the old volume and is off Fly) until you delete that marker; then it stages that seat's key again from `KEYS_DIR`, which needs only that seat's key (`vN/validator.key`), and removes it from Fly once seeded. The empty home syncs from a snapshot and its slashing protection watches the network before it signs. Wipe `KEYS_DIR` after |
| A node stays down (exit 10–13) | `fly logs -a <app>` says `blockula exited N: staying down, …`: a validator's entrypoint idles instead of restarting after 10 (`HALT_HEIGHT`), 11 (the chain needs a newer binary: `upgrade`), 12 (slashing protection saw its key sign elsewhere: [OPERATIONS.md, Slashing protection](../../docs/OPERATIONS.md#slashing-protection)) and 13. **Exit 13** (only once `programs` is on): a program faulted while this node executed a certified block, which the quorum that certified it executed without a fault, so this node differs from them (binary, build, hardware) and a restart would replay the block and fault again. The rest of the set goes on while a quorum is left (with 4, only one may be down). Compare its `version` and `git_sha` with the others' and its image with the release's, find the difference ([OPERATIONS.md, Incident response](../../docs/OPERATIONS.md#incident-response)), then deploy the fix or `fly machine restart`. Exit 14 (a thread the OS wouldn't start for a program) restarts on its own. RPC nodes don't idle: Fly restarts them up to 10 times, then leaves them stopped, and the external monitors page |

## Release signing

Mainnet installs only signed releases: `blockula release install` and `fetch` refuse a release without `checksums.txt.sig`, and the launcher verifies with `fetch`. The key doesn't exist until the ceremony ([OPERATIONS.md, The release key](../../docs/OPERATIONS.md#the-release-key)).

- The ceremony PR changes three lines, the built-in key id in `node/src/release_cli.rs`, `scripts/install.sh` and `mcp/src/release.ts`, plus SECURITY.md and OPERATIONS.md. CI stays green: `built_in_key_id_matches_install_sh` checks the three agree, `scripts/test-install.sh` runs on copies with the key ids it needs, and `scripts/test-release-runbook.sh` runs a `blockula` built with `--features test-release-key`, which trusts a published test key instead. No release, image or reproduction enables that feature (`scripts/check-pins.sh`).
- The key is backed up like a validator key: encrypted, offline, two places. Losing it means operators can only re-pin by hand, since a rotation needs the old key's signature.
- `UNSIGNED_RELEASE=1` exists only for a dry run before the key exists, and the launcher refuses it for `blockula-1`.

## Fly sizing and cost

Prices are for `iad` (docs.fly.io/about/pricing, October 2026); other regions cost more.

| App | Machine | Volume | Window | $/month |
|---|---|---|---|---|
| 4 validators (`$PREFIX-v0…3`) | `performance-4x`, 16 GB, a dedicated IPv4 (p2p only) | 500 GB each, scheduled snapshots off | `--prune-blocks 43200` (12 h: a flood of full blocks fits) | ~261 each: 180 machine, 75 volume, 2 IPv4, ~4 egress |
| Public RPC (`RPC_APP`) | `performance-4x`, 16 GB, shared IPv4 + IPv6 | 250 GB | 14 days (`RPC_PRUNE_BLOCKS`) | ~220 |
| Archive RPC (`ARCHIVE_APP`) | `performance-4x`, 16 GB | 500 GB | everything | ~255 |
| Monitoring host | `shared-cpu-2x`, 2 GB | 20 GB | | ~16 |
| **Total** | | | | **~$1,540/month** (~$1,110 with reserved CPU and RAM) |

- Sizes are settings (`VALIDATOR_VM`, `VOLUME_GB`, `RPC_VOLUME_GB`, …). On `blockula-1` the launcher refuses shared CPUs.
- **Disk.** An idle 4-validator chain writes ~82 MB a day, and a transfer adds ~17 KB ([OPERATIONS.md, Disk sizing](../../docs/OPERATIONS.md#disk-sizing)). Fly volumes stop at 500 GB. The archive fills it in about 11 months at a sustained 1 tx/s, or in under a day of full blocks (~0.57 TB a day); the public node's 14-day window fills its 250 GB in about ten hours of full blocks. Validators' 12-hour window holds ~0.29 TB of full blocks. Watch disk usage, and keep the off-Fly `/api/sync` archive.
- **Egress.** Certificates are q × 3,309 B, twice per view to n − 1 peers: about 80 KB/s (~210 GB/month) per validator at n = 4, ~240 KB/s at n = 7.
- **Seven validators** cost ~$2,350/month.

## Monitoring

None of this pages anyone by default. Set up all of it before T, and test each path.

- **External uptime checks (the dependable layer).** A hosted monitor outside both GitHub and Fly (Better Stack, UptimeRobot, Pingdom or similar), on `https://<RPC domain>/api/health` and `https://<archive domain>/api/health` every minute, expecting HTTP 200. `/api/health` answers 503 when the head is stale or the node is syncing or halted. Alert by push or SMS and email, to the founder and a second person. This needs an account the maintainer creates; nothing in the repo sets it up.
- **The external watch (a free safety net).** `.github/workflows/mainnet-watch.yml` runs `scripts/testnet-watch.sh` with `NETWORK=mainnet` every 5 minutes, as GitHub's scheduler allows, once the repository variable `MAINNET_RPC` is set. It's skipped until then. Set `MAINNET_EXPECT_VALIDATORS` to the active set's size (default 4). It checks health, the chain id `blockula-1`, finality advancing, the head's age, the active validators, and that none is jailed or tombstoned. A failure opens a `mainnet-alert` issue that @-mentions the maintainer. Test it with `gh workflow run mainnet-watch.yml -f test_alert=true`. GitHub drops scheduled runs under load, so count each run as a bonus.
- **Prometheus and Alertmanager** ([deploy/monitoring](../monitoring/README.md)) on a host in the org's private network (`fly wireguard create`), scraping the targets the launcher writes to `~/.config/blockula/blockula-1-fly/prometheus-targets.yml`. Give Alertmanager a real paging receiver (the placeholders are `replace-me`) and route its `Watchdog` alert to a dead-man's-switch service that pages when the heartbeat stops. Fly machines can't run node_exporter, so the `Host*` alerts never fire for Fly nodes. Watch volumes and memory in Fly's own metrics (its managed Grafana); whether those can page is unverified.
- **Fly itself** scrapes every app's `/metrics` (`[metrics]` in each configuration).

## Single mistakes that lose funds or halt the chain

The worst first, each with what guards against it.

1. **Two copies of a validator key.** That's a 5% slash of the pool and a permanent tombstone. In a set of six or fewer, the next failure after a tombstoned seat halts the chain.
   - **How it happens:** scaling, cloning or forking a validator's machine or volume; restoring a volume snapshot; a second machine starting while `VALIDATOR_KEY_B64` is still a Fly secret (the entrypoint seeds an empty volume from it); leaving `NETWORK_HALTED_AT` set, which overrides slashing protection's peer check.
   - **Guards:** the launcher deploys with `--ha=false`, refuses an app with more than one machine or volume, creates validator volumes with scheduled snapshots off and turns them off on every run, removes each key from Fly as soon as its volume holds it, never stages a key again by itself once it's off Fly (that takes deleting its `staged-<app>` marker, [Day 2](#day-2)), and refuses `NETWORK_HALTED_AT`. The entrypoint refuses a volume that held a key and lost it, and slashing protection refuses to sign while its key signs elsewhere (exit 12).
   - **Never** run `fly scale count`, `fly machine clone` or `fly volumes fork` on a validator, or restore its snapshot.
2. **A compromised Fly org or token.** Any org member can SSH in and read `validator.key`.
   - **Guards:** mainnet has its own org (the launcher refuses `personal`) with only the founder in it, 2FA on, and no deploy tokens in CI or anywhere else.
   - **The same for the wallet:** a Vercel or `blockula.app` takeover lets any script on that origin derive users' passkey keys. Use a separate, locked-down Vercel project that promotes only reviewed tags, and lock the domain.
3. **Destroying the wrong apps.** On the testnet, `RESET=1` or a wrong `PREFIX` or `ORG` destroys machines and volumes.
   - **Guards:** the launcher has no reset and never destroys anything, refuses the testnet's names and `RESET` itself, and requires `ORG`, `PREFIX`, `RPC_APP` and `ARCHIVE_APP` explicitly.
   - Only `prepare` creates apps. `launch` and `upgrade` refuse apps that don't exist in `ORG`.
4. **A parallel redeploy of a bad image halts the chain**, and so does a leftover `HALT_HEIGHT`.
   - **Guards:** `upgrade` is one node at a time through the participation gate, and stops at the first that isn't back. `launch` never redeploys a running app, and the launcher refuses `HALT_HEIGHT`.
   - Every image is the verified release binary. The build checks its SHA-256 and version again.
5. **Genesis mistakes stay for months.** Downtime and evidence parameters are the token holders' alone; a flagged founder seat can't rejoin; IPs and the RPC domain are signed into the proofs of possession.
   - **Guards:** [the parameters above](#genesis-parameters-to-get-right), unflagged founder seats, `prepare` before the operator files, a custom RPC domain from day 1, the spec reviewed and frozen, and the genesis rebuilt on two machines and again by the launcher, which also checks each key against its app's address.
6. **Losing the laptop or the release key.** Losing two of four validator keys halts the chain for good: there's no consensus-key rotation.
   - **Guards:** keys are made offline and backed up encrypted in two places before anything else. `KEYS_DIR` holds only consensus keys (the launcher refuses anything else in it), only from T − 2 h (the day-before dry run needs none), and is wiped afterwards; a seat re-seeded later needs only its own key. The release key lives only on the offline machine and its backups.
7. **Losing the chain's early history.** Validators keep 12 hours of blocks and the public node 14 days; an archive that fast-synced from a snapshot would hold nothing before it.
   - **Guards:** the archive replays from genesis (`--archive --full`, also syncing from the public node), is restored from a volume snapshot rather than rebuilt empty, and the off-Fly copy (`archive-chain.mjs`) runs from launch day.
8. **All validators on Fly** are one failure domain, and so is one founder: waived, below.
   - **Mitigations:** four regions, outside operators on other providers, and the off-Fly archive.

## What's waived

Recorded in the genesis spec (`[[waiver]]`s, printed by every build) and published with it.

| Gate | Waived how | What it costs |
|---|---|---|
| Independence: 22+ operators (the builder's minimum), provider/ASN/country/facility caps, one seat per entity, 2 founder seats (DEC-3, DEC-14) | A founder-run set of 4, all on Fly, plus 1–2 outside operators | The founder holds ⅔: it passes every council proposal and can halt or censor alone. Fly is one failure domain |
| External audit before launch | Commissioned before go-live; programs ship off, and switch on only from v3 | The audit's fixes (or only the record that it cleared v2's rules) arrive as v3 by `ScheduleUpgrade` (about 7 days at the floors), before programs can switch on; other fixes the same way, or by a halt and patch |
| 3 months of public testing, a 72 h dry run with every operator | A 24 h dry run on `blockula-dryrun-1` | Less exercised; testnet-2 is days old |
| The full ceremony (10 weeks, 3+ builders, a recorder) | 11 days, 2 builders, the founder's own offline keys | Weaker trust in the genesis |
| 3+ independent RPC operators and witnesses; checkpoints from several operators | Two founder RPC nodes; checkpoints are the founder's word | Clients trust one operator |
| Counsel's sign-off, a funded bounty | Not before launch | Legal and disclosure risk |
| Liquid FANG for users | Only the operator floats are liquid | Users can't transact until a distribution or treasury spend |
