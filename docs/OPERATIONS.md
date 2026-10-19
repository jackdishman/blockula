# Operating Blockula

For validator operators, sentry and RPC operators, and the release and incident teams. It closes the documentation side of OPS-1, OPS-4, OPS-5, OPS-6, OPS-7, SF-14 and CRY-2 (weak subjectivity) in [MAINNET.md](MAINNET.md), and follows the decisions in [MAINNET-DESIGN.md](MAINNET-DESIGN.md) (DEC-3, DEC-14, section 3.9).

Some of what's below needs protocol v1 code that is still landing. Those parts are marked **(planned: P-…)** with the package that delivers them; everything else works with today's binary.

## Contents

1. [Roles and topology](#roles-and-topology)
2. [Validator set and hosting policy](#validator-set-and-hosting-policy)
3. [Reference hardware](#reference-hardware), [Bandwidth](#bandwidth), [Disk sizing](#disk-sizing)
4. [Ports and firewall](#ports-and-firewall), [Time](#time), [Sentries](#sentries)
5. [Running a validator](#running-a-validator)
6. [Keys, backups and restores](#keys-backups-and-restores)
7. [Upgrades](#upgrades)
8. [Incident response](#incident-response)
9. [Runbooks](#runbooks) (one per alert)
10. [Weak subjectivity](#weak-subjectivity): what a joining node trusts, and checkpoints in releases
11. [Releases](#releases) (and [installing and upgrading](#installing-and-upgrading) one), [npm packages](#npm-packages) and [repository settings](#repository-settings-github)
12. [Monitoring](#monitoring)
13. [Checklists](#checklists)

## Roles and topology

| Role | Runs | Exposed to the internet |
|---|---|---|
| **Validator** | `blockula run` with a `validator.key`; votes in simplex | p2p only (TCP, default 30300). Never HTTP |
| **Sentry** | A full node registered to its operator's validator; serves the public and relays txs to the validator | HTTPS (reverse proxy) and p2p |
| **RPC / full node** | `blockula join`; API, explorer, `/api/sync`, snapshots | HTTPS (reverse proxy) |
| **Monitoring host** | `deploy/monitoring` | Nothing (SSH only) |

```
            internet (wallets, explorers, joiners)
                     │ HTTPS
           ┌─────────┴─────────┐
           │ sentry / RPC node │  (one or more per operator)
           └─────────┬─────────┘
   private link      │ HTTP :7070 (sync, tx forwarding)
   (WireGuard/VPC)   │
           ┌─────────┴─────────┐   p2p :30300 (ML-DSA-authenticated)
           │     validator     │◀──────────────▶ other validators
           └───────────────────┘
```

A validator's HTTP API locks the same state the consensus engine uses, so a public request flood against it delays its votes (SEC-4). That's why validators don't expose HTTP and why sentries exist. See [Sentries](#sentries).

## Validator set and hosting policy

- **Active-set cap: 32 at genesis** (DEC-3). Votes are equal-weight; the set is the top 32 candidates by pool stake (planned: P-STAKE). The cap rises by governance once certificates shrink: a ⅔ certificate is ~73 KB at n = 32 and every validator re-broadcasts two per view (see [Bandwidth](#bandwidth)).
- **Genesis target: 21–32 independent operators.** Halting the chain takes ⌊(n−1)/3⌋+1 validators (11 of 32), so independence is the security budget.
- **No hosting provider, ASN or jurisdiction above ¼ of the active set** (8 of 32), always below ⅓ (DEC-14). Operators declare provider, ASN and jurisdiction at registration and whenever they move; the founder publishes the distribution monthly and asks operators in an over-represented provider, ASN or jurisdiction to move. Count by the ASN that actually routes the validator's IP (`whois -h whois.radb.net <ip>` or `curl ipinfo.io/<ip>/org`), not the reseller's brand.
- **The founder's nodes stay small.** The founder runs **at most 3 validators**, on Fly.io: `performance-8x` machines (8 dedicated vCPUs, 32 GB; `performance-4x` with 16 GB is the floor), each with a dedicated IPv4 for p2p and a volume sized per [Disk sizing](#disk-sizing). Fly volumes top out at 500 GB at the time of writing: plenty for an empty chain for years, not for a sustained flood, so the founder's validators must run with block pruning (the validator default) and watch `HostDiskFillsWithinAWeek`. The public RPC nodes and the founder's sentries also run on Fly. The founder's share of the set shrinks as independent operators join.
- **Recommended providers, mixed.** Bare metal and dedicated servers give unmetered or generously included bandwidth and predictable disks:
  - Latitude.sh, OVHcloud, Cherry Servers, Hetzner (dedicated). **Hetzner:** its terms have restricted crypto workloads before (mining is banned, and some blockchain node operators have been asked to leave); every operator must check the current terms and get written confirmation before running a validator there.
  - Regional and colocation providers spread ASNs and jurisdictions better than any big brand. They are welcome.
  - AWS and GCP work, but **internet egress is billed per GB**: ~12 TB/month of certificates alone costs on the order of $1,000 per validator per month at list prices. Use them only with committed-use egress pricing, or for sentries in front of a bare-metal validator.

## Reference hardware

| | Minimum | Recommended | Why |
|---|---|---|---|
| CPU | 4 dedicated cores, x86-64 (AVX2) or arm64 | 8 cores / 16 threads, recent EPYC, Xeon or Ryzen | Signatures are cheap: ML-DSA-65 verifies in ~25–50 µs on a modern core, so a full block (~400 txs) plus a view's votes and certificates is 10–25 ms of one core. Cores buy headroom for the rest: restarts load the state database (under a second per million records) instead of replaying history, verifiers will replay up to 64 unexecuted ancestors (CV-5, planned), p2p encryption runs over ~10 MB/s, and sentries pull sync batches. Execution is sequential, so single-thread speed matters more than core count. No shared or burstable vCPUs. |
| Memory | 16 GB | 32 GB | The state (accounts, names, Hunts, claims) is held in memory as well as in the state database, so memory grows with the number of records, not with history (the tx index and account history are on disk). Plus redb's 256 MB page cache, marshal's caches and a 5,000-tx mempool. |
| Disk | 1 TB NVMe SSD | 2 TB NVMe SSD, expandable online | The node fsyncs its block logs and consensus journals on every block and commits the state database before acknowledging consensus: low-latency local NVMe, not network storage with burst credits. Size: [Disk sizing](#disk-sizing). |
| Network | 1 Gbit/s, ≥ 15 TB/month included | 1 Gbit/s unmetered (or 10 Gbit/s) | [Bandwidth](#bandwidth). |
| Other | Dedicated public IPv4 | Plus IPv6 | p2p is raw TCP; NAT and shared IPs break dialing. |

Sentries and RPC nodes: 4 cores, 16 GB, 1 TB NVMe, and the same bandwidth (each sentry costs every validator it peers with ~150 KB/s of certificates at n = 32). The monitoring host: 2 cores, 4 GB, 50 GB.

### Bandwidth

Certificates dominate. Simplex sends every vote and certificate to every connected peer, and each node re-broadcasts each certificate it holds once (MAINNET-DESIGN DEC-3; in commonware-consensus 2026.9.0, `simplex/actors/voter/actor.rs` and `voter/round.rs`):

- **Certificates:** a ⅔ certificate at n = 32 is ~73 KB (22 signatures × 3,309 B). Two per view (notarization, finalization) to 31 peers at one view per second: **~4.5 MB/s of egress per validator, ~12 TB/month**. At n = 64 it would be ~18 MB/s, which is why the cap is 32.
- **Votes:** ~3.3 KB each, two per view, to 31 peers: ~0.2 MB/s.
- **Blocks:** the leader sends its block to every peer. A full 4 MiB block to 31 peers is ~130 MB in about a second, once every 32 views on average: provision for 1 Gbit/s bursts.
- **Sentries:** each secondary peer adds ~150 KB/s of certificates per validator.

Budget **≥ 15 TB/month of egress per validator, unmetered or included in the price**. The dashboard's Network panel shows the real number; compare it with your plan monthly.

### Disk sizing

What a node's home holds (storage layout 4; the full layout and commit protocol are in `node/src/store.rs`):

- `blocks/`: executed blocks, one JSON line each, in segment files of 10,000 heights. ~0.4 KB per empty block, ~10.8 KB per transfer (txs are hex in the JSON).
- `cblocks/`: the exact consensus blocks, served to joining nodes by `/api/sync`. ~0.2 KB per empty block, ~5.4 KB per transfer (wire bytes).
- `state.redb`: the state database. The state itself (records and Merkle tree nodes, ~0.5 KB per account: ~0.5 GB at a million accounts, whatever the history), plus index rows (~0.25 KB per block, ~0.75 KB per tx: tx ids, account history) and a finalization certificate every 100 heights and at each epoch's end. A certificate holds a quorum of 3,309-byte signatures.
- `snapshots/`: up to four certified snapshots (compressed state): the two newest and two long-lived ones (the first of each of the last two ~15-minute stretches), so budget four times the compressed state. `consensus/`: marshal keeps the last 2,000 blocks and certificates (`MARSHAL_RETAIN`), and simplex keeps the journals of the current epoch and the two before it (`blockula-simplex-<epoch>`; older ones are deleted when an epoch starts and when the node starts).
- `slashing-protection.json` (validators): a few kilobytes, see [Slashing protection](#how-slashing-protection-works).

An empty chain, everything kept:

| Active validators n | Quorum | Certificate | Per block | Per day | Per year |
|---|---|---|---|---|---|
| 4 | 3 | ~10 KB | ~0.95 KB | ~82 MB | ~30 GB |
| 8 | 6 | ~20 KB | ~1.05 KB | ~91 MB | ~33 GB |
| 16 | 11 | ~36 KB | ~1.2 KB | ~104 MB | ~38 GB |
| 21 | 15 | ~50 KB | ~1.35 KB | ~117 MB | ~43 GB |
| 32 | 22 | ~73 KB | ~1.6 KB | ~136 MB | ~50 GB |

Transactions add far more than an idle chain: a transfer is stored in both logs plus its index rows, **~17 KB per tx**:

| Sustained load | Per day | Per year (archive) | Validator, default 2-day window |
|---|---|---|---|
| 1 tx/s | ~1.5 GB | ~0.54 TB | ~3 GB |
| 10 tx/s | ~15 GB | ~5.4 TB | ~30 GB |
| Every block full (`max_block_bytes` 2 MiB, ~388 tx/s) | ~0.57 TB | n/a: a flood, not a load | ~1.15 TB |

**Pruning.** Validators keep a window of recent blocks: about two days by default (`--prune-blocks N` sets the window in blocks; `--archive` keeps everything; full nodes keep everything unless given `--prune-blocks`). The window never goes below an epoch plus two long-lived snapshot intervals (900 blocks each at 1 s blocks) plus a tx's validity window (~9,000 blocks at 1 s with 1-hour epochs), so the node can always serve every snapshot it keeps with its certified epoch anchor (the previous epoch's last block), and the blocks after it to the joiner that downloaded it, and still recognize any replayed tx. A certificate the node missed while catching up is read back from marshal's store after the block executes. Older index rows go a few heights per block, then whole log segments are deleted; epoch boundaries' hashes, state roots and certificates are kept forever. `/api/block`, `/api/sync` and `/api/status` (`history_from`) say what was pruned; `/api/tx` no longer finds txs from pruned heights. A flood can't fill a pruning validator's disk faster than the window allows: size the volume for the window at full load, or shorten the window.

So:

- **Validators at genesis: 2 TB NVMe** (1 TB minimum, with `--prune-blocks 43200`, half a day), on a volume you can grow without downtime. `HostDiskAlmostFull` (80%) and `HostDiskFillsWithinAWeek` give you time to grow it.
- **Archival RPC nodes** (`--archive`, or full nodes' default) keep everything and need the most disk: plan from the archive column above.
- Losing `state.redb` costs no data: the node rebuilds it on start, by replaying its block logs from genesis (or from `base.state.zst` after a fast sync, or, on a pruned validator, from its newest local snapshot). That takes as long as the history; restarts otherwise take seconds.

## Ports and firewall

| Port | Who listens | Allow from |
|---|---|---|
| p2p TCP (the port in your registered `p2p_addr`; 30300 by convention) | validators, sentries | Anywhere. The handshake admits only keys in the tracked sets (validators, registered sentries) |
| HTTP 7070 (validator) | validator | **Only** its own sentries and monitoring, over a private link. Never the internet |
| HTTP 7171 (full node default) | sentry / RPC | Its reverse proxy on localhost |
| 443 (reverse proxy) | sentry / RPC | Anywhere |
| 22 (SSH) | all | Your admin IPs or VPN only; keys only, no passwords, no root login |
| 9090, 9093, 3000, 9100 | monitoring | localhost, or the private link to the monitoring host |
| Outbound | all | p2p to peers, NTP (UDP 123, or TCP 4460 for NTS), HTTPS to GitHub (contribution checks) |

`blockula run --bind` sets the interface for **both** HTTP and p2p. A validator that binds `0.0.0.0` for p2p therefore also listens for HTTP on every interface, and the firewall is what keeps HTTP private. With nftables (Debian/Ubuntu):

```bash
# /etc/nftables.conf (excerpt): drop everything inbound except SSH (admin), p2p and the private link.
table inet filter {
  chain input {
    type filter hook input priority 0; policy drop;
    ct state established,related accept
    iif lo accept
    ip saddr 203.0.113.0/28 tcp dport 22 accept      # your admin range
    tcp dport 30300 accept                           # p2p
    iifname "wg0" tcp dport { 7070, 9100 } accept    # sentries + monitoring over WireGuard
    icmp type echo-request limit rate 5/second accept
    icmpv6 type { nd-neighbor-solicit, nd-neighbor-advert, nd-router-advert, echo-request } accept
  }
}
```

Then `nft -f /etc/nftables.conf && systemctl enable --now nftables`, and check from outside: `nc -vz <validator-ip> 7070` must fail, `nc -vz <validator-ip> 30300` must connect. Cloud security groups work the same way; use both.

## Time

Verifiers reject a block whose timestamp is more than 1 s ahead of their own clock (`MAX_CLOCK_DRIFT_MS`), and leaders wait for `parent.timestamp + block_time` by their clock. A validator whose clock runs fast proposes blocks the others refuse; one that runs slow refuses honest blocks. Both cost votes long before the skew reaches a second.

- Run **chrony** with at least four sources from different operators, preferably NTS-authenticated:

  ```
  # /etc/chrony/chrony.conf
  server time.cloudflare.com iburst nts
  server nts.netnod.se iburst nts
  server ptbtime1.ptb.de iburst nts
  server ntppool1.time.nl iburst nts
  makestep 1.0 3        # step only at startup; slew afterwards
  rtcsync
  ```

  `systemctl restart chrony && chronyc tracking` should show "Leap status: Normal" and a system time offset well under 10 ms. `chronyc -N authdata` shows NTS working.
- Don't mix leap-smearing sources (Google, AWS's internal service) with non-smearing ones: during a leap second they disagree by up to 0.5 s. Use one kind or the other.
- The monitoring stack alerts at 100 ms (`HostClockSkew`) and 250 ms (`HostClockSkewCritical`) of offset, and when the kernel reports the clock unsynchronised (`HostClockNotSynchronising`).

## Sentries

The rule: **validators serve consensus, sentries serve people.**

- A validator's HTTP listens on a private interface (or is firewalled to one) and answers only its operator's sentries and monitoring. Validator-to-validator HTTP, where it's needed at all, also runs only over private links.
- Each operator runs at least one sentry, ideally two in different regions. The sentry does the public work: the API and explorer, `/api/sync` and snapshots for joiners, tx intake with rate limits.
- Register the sentry's public URL as your validator's `http_addr` (that's what other nodes and the explorer use), not the validator's own address. `deploy/launch.sh` does the same for the testnet.

**Today:** a sentry is a full node. It joins with `blockula join` (HTTP sync from its validator over the private link), forwards txs and faucet requests to the validator with `--peer-http http://<validator-private-ip>:7070`, and authenticates that forwarding with a shared `BLOCKULA_FORWARDER_SECRET` (≥ 32 chars, same value on both). Put a reverse proxy in front for TLS and rate limits, and tell the node which header carries the client IP:

```bash
# On the sentry, after `blockula join` once:
BLOCKULA_FORWARDER_SECRET=$(cat /etc/blockula/forwarder-secret) \
  blockula run --home /var/lib/blockula/full --port 7171 --bind 127.0.0.1 \
  --peer-http http://10.8.0.2:7070 --client-ip-header x-real-ip
```

```
# Caddyfile: TLS, per-client rate limits and the real client IP.
rpc.example.org {
  reverse_proxy 127.0.0.1:7171 {
    header_up X-Real-IP {remote_host}
  }
}
```

(Rate limiting needs the `caddy-ratelimit` plugin, or use nginx's `limit_req`.)

Prefer a header the proxy overwrites (`X-Real-IP` above, `Fly-Client-IP` on Fly). With an appending header (`X-Forwarded-For` as nginx's `$proxy_add_x_forwarded_for` builds it), the node reads it from the right: the entry the proxy added is the client, anything to its left is what the client claimed. Behind two appending proxies (a CDN, then nginx) pass `--client-ip-hops 2`. The hops count entries across every line of the header, in the order they arrived: a proxy that adds its own `X-Forwarded-For` line after the client's (HAProxy's `option forwardfor`) instead of extending it counts the same as one that appends to the line, so the client's own lines are always to the left. A request that reaches the node without the configured header (or with an unreadable one) is limited as the proxy's own address, so a misconfigured proxy shares one budget instead of lifting every limit.

**Protocol v1 (planned: SF-10, P-STAKE/P-SYNC):** operators register up to a few sentry keys and addresses on their validator record. Validators track registered sentries as *secondary* p2p peers: inbound-only, never dialed, receiving every vote and certificate. Sentries don't run simplex; they run marshal plus a passive certificate listener and backfill through the resolver. Validators then stop exposing HTTP entirely. The on-chain sentry list is capped (each sentry costs every validator ~150 KB/s, and an oversized peer set panics the p2p layer), so register only what you run.

## Running a validator

On a fresh Debian 12 / Ubuntu 24.04 host that meets the [hardware](#reference-hardware), [firewall](#ports-and-firewall) and [time](#time) requirements:

```bash
# 1. A service account and a home on the data volume.
sudo useradd --system --home /var/lib/blockula --shell /usr/sbin/nologin blockula
sudo install -d -o blockula -g blockula -m 700 /var/lib/blockula
```

2. **Install a verified binary**, as your own user with `gh auth login` done: [a first install](#a-first-install) (paste the block there). It checks the archive against the release announcement in the operators' channel, then the release's offline signature against the release key id, which it pins in `~/.blockula/release-key-id`. Every later upgrade is checked against that pin ([Upgrading](#upgrading)). Nothing the release ships, its `install.sh` included, runs before those checks pass.
3. **The genesis file and the validator home.** Take the genesis file from the signed release, checked against the pinned key, not from a node: `blockula release fetch` puts only files that match the release's signed `checksums.txt` into `genesis-$V`. Then `validator init` creates the validator home and its consensus key.

<!-- runbook-test: validator-genesis -->
```bash
( set -euo pipefail
  V=vX.Y.Z
  /usr/local/bin/blockula release fetch $V --file genesis-blockula-1.json --dir genesis-$V
  sudo install -m 644 genesis-$V/genesis-blockula-1.json /var/lib/blockula/genesis-blockula-1.json
  sudo -u blockula /usr/local/bin/blockula validator init --home /var/lib/blockula/val \
    --genesis /var/lib/blockula/genesis-blockula-1.json \
    --public-p2p-addr <your-ip>:30300 --public-http-addr https://<your sentry>
)
```

Each block runs in a subshell with `set -euo pipefail`, so it stops at the first command that fails: fill in the `<…>` values, paste it whole, and nothing after a failed check runs. The blocks hold no comments, so they paste the same into bash and zsh. `scripts/test-release-runbook.sh` runs them as they're written here (CI).

At genesis, keys come from the [genesis ceremony](GENESIS_CEREMONY.md) instead, and the home is built from the published `genesis.json` (verify its hash).

Run it under systemd:

```ini
# /etc/systemd/system/blockula.service
[Unit]
Description=Blockula validator
After=network-online.target chrony.service
Wants=network-online.target

[Service]
User=blockula
Group=blockula
# A unique id per machine: a home copied onto another machine refuses to start (double-sign guard).
Environment=BLOCKULA_INSTANCE_ID=%m
Environment=BLOCKULA_LOG=warn,blockula=info
EnvironmentFile=-/etc/blockula/env
ExecStart=/usr/local/bin/blockula run --home /var/lib/blockula/val --bind 0.0.0.0 --p2p-preset recommended
Restart=on-failure
RestartSec=10
# Deliberate stops stay stopped until an operator acts: 10 --halt-height reached, 11 a protocol
# version this binary doesn't run is in force, 12 another instance is signing with this key.
RestartPreventExitStatus=10 11 12
LimitNOFILE=65536
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/blockula
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

`/etc/blockula/env` (mode 600, root) holds `BLOCKULA_FORWARDER_SECRET=…` and a fine-grained, public-read-only `GITHUB_TOKEN=…` (validators check contribution claims against GitHub; without a token they hit the anonymous rate limit). Then `sudo systemctl enable --now blockula`, `journalctl -u blockula -f`, and `curl -s 127.0.0.1:7070/api/health`.

Exit codes: `0` never for a validator (it runs until stopped), `1` an error (the log says what), `10`, `11` and `12` the deliberate stops in the unit above ([Halted](#halted), [Upgrade required](#upgrade-required), [Slashing protection](#slashing-protection)).

Double-signing protection, in layers:

- **Slashing protection** (always on, can't be turned off): the record next to the key, observe-before-signing after every start, and a halt (exit 12) if anything signed with this key shows up that this process didn't sign. See [Slashing protection](#how-slashing-protection-works).
- Three startup guards: the node refuses a home that has executed blocks but no consensus journal (a restored or copied home); with `BLOCKULA_INSTANCE_ID` set it refuses a home last used by a different machine (`machine.lock`); and once slashing protection has stopped it (exit 12), it records that in `double-sign-halt.json` in the home and exits 12 again on every start, so the stop holds whatever restarts it (a supervisor without `RestartPreventExitStatus`, a redeploy, a host reboot; audit F52). `--i-know-this-key-is-not-running-elsewhere` overrides these three for one start (it deletes `double-sign-halt.json`), never slashing protection itself. Use it only after you have *proven* the other copy is gone.

Then install [monitoring](#monitoring) and join the on-call rotation and the private operator channel ([Incident response](#incident-response)).

## Keys, backups and restores

| Key | Where it lives | Backup |
|---|---|---|
| **Consensus key** (`validator.key`, a hex seed) | On the validator only (mode 600). Holds only a fee float once the fee payer is separate (OPS-1) | Offline, encrypted, two locations |
| **Owner account** (bond, rewards, commission) | Cold: an offline machine | Offline, encrypted, two locations |
| **Slashing-protection record** (`slashing-protection.json`: what the key signed in the newest views of its newest epoch) | Next to the consensus key | Never restored from backup; see below |
| **Release key** (maintainers) | Offline machine | See [Releases](#releases) |

Encrypt key backups with a passphrase you keep separately, for example with [age](https://age-encryption.org): `age -p -o validator.key.age validator.key`. Store copies in two places you control (a safe, a bank box), never on the validator, in a cloud drive, or in the same place as the passphrase. Test that you can decrypt them at every quarterly drill.

**Never restore a volume snapshot onto a running validator, and never run two copies of a key.** Two copies sign conflicting votes in the same view, and in protocol v1 that's slashed (5% of the pool) and tombstoned for good (MAINNET-DESIGN 3.4). Chain data is replaceable from peers; keys and the slashing-protection record aren't. So back up keys, not the chain. Turn off provider volume snapshots on validators (on Fly: `--scheduled-snapshots=false`).

### How slashing protection works

`node/src/protection.rs`; MAINNET-DESIGN 3.9. Every vote passes three rules before its signature exists:

1. **The record** (`slashing-protection.json` in the validator home). For the newest epoch the key signed in, the last 32 views: the notarize and finalize it signed (hashed) and whether it nullified. A vote that conflicts with it is refused (another block in a view, a finalize after a nullify or the reverse, an older epoch). The record is written and fsynced before the signature is released. A record for another key, or one that doesn't parse, stops the node at start.
2. **Observe before signing.** A restored volume rolls the record back with it, so after every start the validator signs nothing in its first epoch until it has watched the network finish two more views than it saw once the start settled (about 5 s at 1 s blocks), and then only views above every certificate it saw. Its earlier run stopped before this one started, so none of its votes can be in those views. `blockula_slashing_protection_abstaining` is 1 meanwhile. If the network can't make progress without this node (every validator restarting at once, or too many down), it signs again after ~20 s on the record alone: the one case where a restored disk could still double-sign. Silence alone isn't enough for that (audit F18): a node cut off from the network, or stuck in an epoch the network has left, sees no certificates either. So it also asks its HTTP peers (`--peer-http`, else the genesis validators' `http_addr`) for their tips every 2 s, and signs on the record alone only once more of the peers that answer are at exactly its height than claim more, and its height hasn't moved for most of those ~20 s. A peer that claims to be ahead is asked for the certified blocks after this node's tip: if it proves them, the network has finalized past this node and nothing reopens the escape; if it can't, its claim is one unverified answer, and one lying peer among honest ones doesn't hold the node back. Consensus traffic for a later epoch counts only once more of the epoch's validators than can be faulty send it, recently (one validator's word, or a non-member's, doesn't). No peer at its height, peers ahead that aren't outnumbered, or a proven later block keeps it abstaining (a missed vote, never a double-sign), with the reason in the log. Even then it signs only views above the newest block it executed. If the whole network is halted at `H` and this validator can't hear enough honest peers (none answers, or peers that answer claim to be past `H` without proving it), start it with `--network-halted-at H` (H: the height every node stopped at, which must be this node's height): the operator's word then stands in for every unverified answer (the log names what it overrode), though not for a block a peer proved or later-epoch traffic from more validators than can be faulty. Remove it once the chain runs again.
3. **Another instance.** A vote or certificate carrying this key's signature on something this process never signed, in a view an earlier, stopped run can't have reached, means a second copy is running. The node stops signing at once and exits with code 12.

What that means for you: the watch adds a few seconds to a restart once the node has its p2p links back (how long that takes depends on the p2p preset: [Upgrades](#upgrades)); a restore is survivable if the network is live, but don't rely on it; never run two copies.

**Restoring after losing a validator host:**

1. **Make sure the old host can't sign.** Power it off from the provider's console and detach its volume, or revoke its network. If you can't prove it's down, stop here: wait for the next epoch boundary, then for the operator channel to confirm the old key went silent.
2. Provision a new host (hardware, firewall, NTP, verified binary).
3. Restore `validator.key` from the offline backup into a new home with the published `genesis.json`.
4. **Slashing protection:** copy `slashing-protection.json` from the old disk if you still have it (the newest copy only; never an older one). If it's lost, start without it: the node watches the network before it signs (see above). Do this while the network is live (finalizing without you); if it's halted, wait until it isn't, or until the next epoch boundary.
5. Sync state from peers (fast sync from a certified snapshot), not from a chain-data backup.
6. Start the service. The startup guards will refuse the copied home on a new machine. Pass `--i-know-this-key-is-not-running-elsewhere` once, for this start only, after step 1. If the node exits with code 12 ("another instance is running"), step 1 failed: find the other copy before anything else.

**Quarterly restore drill** (OPS-6): on a testnet, restore a validator from its offline key backup onto a fresh host following the steps above, time it, and record the result in the operator log.

## Upgrades

Upgrades are announced, released, signalled, scheduled and activated at a height (`node/src/upgrade.rs`, MAINNET-DESIGN 3.9). Nobody restarts everything at once.

**What the protocol does.** Each block carries `version`, the protocol version in force at its height (verifiers reject any other), and `signal`, the newest version its proposer's binary supports. State holds the upgrade schedule `[(version, activation_height)]`, changed only by governance (`ScheduleUpgrade` adds an entry, `CancelUpgrade` withdraws one), and an upgrade activates at least 64 blocks after the block that schedules it (and can be withdrawn until 64 blocks before it). A new binary runs the old rules until the activation height. A node whose binary doesn't support the version in force at the next height executes nothing more: it stops after the last block it can run, doesn't vote on or propose later ones, and then

- a **validator exits with code 11** (systemd keeps it down, `RestartPreventExitStatus`), and does so again on every start until it's upgraded;
- a **full node keeps serving reads** at that height, with `/api/health` 503 and `"halted": "upgrade_required"`, until it's upgraded.

Install the new binary ([Upgrading](#upgrading)) and start it: it executes the new version's blocks and catches up.

**The hard-fork process.**

1. **Announce** at least two weeks ahead (planned upgrades): the version number, what changes, the release date, and the proposed activation height. Post in the operator channel and publicly.
2. **Release** a signed, reproducible build ([Releases](#releases)). Operators install it with `blockula release install` ([Upgrading](#upgrading)), which checks the ML-DSA signature against their pinned release key id and provenance; ideally they rebuild it too.
3. **Install and signal.** Each operator restarts onto the new binary in their own maintenance window, one node at a time, each only once the previous one is back (below). Across the network, never more than ⌊(n−1)/3⌋ validators out at once (one more halts the chain), counting any down for other reasons and any still reconnecting, so one at a time on sets of four to six: coordinate in the operator channel. The new binary signals the new version in every block it proposes. `GET /api/upgrade` shows readiness: the newest signal from each active validator and whether ≥ ⅔ are ready.
4. **Schedule.** Once ≥ ⅔ of the active set signal the version, a `ScheduleUpgrade { version, activation_height }` proposal goes to governance (a ⅔ supermajority; MAINNET-DESIGN 3.5; `blockula gov propose upgrade`). Pick the height so the vote, the timelock, a cancellation's vote (below; the node refuses heights that leave no room for one) and a margin for stragglers fit before it. Every node then shows `blockula_upgrade_next_version` and `…_next_height`; nodes without the new binary fire `BlockulaUpgradeRequired`, and `…Soon` a day before.
5. **Activate.** At the height every upgraded node switches rules. Watch `/api/upgrade` and the explorer: blocks from the activation height on carry the new `version`. Stragglers halt cleanly (above) instead of diverging; they upgrade and continue.

**Withdrawing a scheduled upgrade** (the release is pulled for a bug, a reproducibility failure or a release-key incident; the release slips past the height). Until the activation is near, governance can take it back: a `CancelUpgrade { version, activation_height }` proposal (`blockula gov propose cancel-upgrade <version> --title "…"`, which looks the height up in `/api/upgrade`; SDK `proposal.cancelUpgrade(version, height)`). It needs the same ⅔ supermajority as scheduling (or, during the bootstrap, ⅔ of the council), has no timelock (it runs the block after its vote closes: it keeps the rules in force), and withdraws that entry and any upgrade scheduled after it. It can be proposed once the `ScheduleUpgrade` has run (while that one is still being voted on, vote nay), and only while it would land at least 64 blocks before the activation: at least `voting_blocks + 65` blocks ahead (a week and a bit on mainnet). Every `ScheduleUpgrade` has to leave room for one (at the voting period in force when it was proposed), and a cancellation that finds nothing to withdraw when it runs (a second one, a stale one) fails and changes nothing. Once it has run, `/api/upgrade` no longer lists the version, `blockula_upgrade_next_*` drop, and nobody halts at that height. The withdrawn version number (and every one up to it) is spent: the chain refuses to schedule it again (`/api/upgrade`'s `withdrawn.highest_version`), because halts and readiness signals compare only numbers, and a binary of the pulled release would run a re-scheduled version with the pulled rules. Ship the fixed release as the number the cancellation's outcome names ("ship the next upgrade as vN or later", also `/api/upgrade`'s `next_version`: past every spent number and every superseded proposal's own), signal it, and propose its `ScheduleUpgrade` after the cancellation has run: any `ScheduleUpgrade` proposed in or before the cancellation's block (still voting or queued when it ran) is superseded and fails when it would run, since it was proposed against the schedule the cancellation withdrew from (the cancellation's outcome lists them, and each one's own outcome names the same number). A `ScheduleUpgrade` waiting in a multisig for approvals when the cancellation ran (the foundation's k-of-n keys, say) is superseded too: the chain refuses the approval that would file it ("superseded"; `/api/multisig` shows it as `superseded`), so abandon it (let it expire), treat its number as spent, and file the fixed release's `Propose` as a new multisig action after the cancellation (review-fix-ops-2; the outcome counts them). The numbers of the superseded proposals that had passed (their vote ended by the block the cancellation ran in, whatever the proposal ids: every vote of a block closes before anything runs in it) are spent by the chain too, since their releases may be out and signalled, and so is the number of one superseded while still being voted on that passes anyway (when its vote closes). A number whose `ScheduleUpgrade` was rejected, or that waited in a multisig, isn't spent by the chain (filing one mustn't spend numbers); the advice goes past those that were open or stored when the cancellation ran (`withdrawn.superseded_version`), since a release may carry them, and if a release for any number was published, never reuse it. Anyone can push the advice up by filing an upgrade proposal at the top of the step while a cancellation is pending (at most 16 numbers per cancellation, and the step moves with it): a number between `withdrawn.highest_version` and the advice is still accepted, so use one only if you know no release carries it. Versions move up in steps of at most 16 past the newest ever scheduled, withdrawn or superseded, so the advised number is always one the chain accepts (review-r3-gov-1), and an upgrade activates at most 365 days after its proposal's vote ends, so a mistaken huge version is refused when it's proposed (review-r2-gov-1). A node already running the pulled release keeps following the chain (it runs the rules in force); replace it with the previous or the fixed release before the fixed one's activation, since it halts there.

**Too late for a vote** (fewer than `voting_blocks + 65` blocks to go, or the cancellation didn't pass): the chain will switch at the height, and every node without a binary that supports the version halts at the block before it (exit 11). Either ship a fixed release of that version and get ≥ ⅔ of the set onto it before the height, or stop the chain on purpose and fix it while it's stopped:

1. The IC announces a halt at `H − 1` (the block before the activation) and why, in the operator channel and publicly.
2. Operators who installed the pulled release go back to the previous one (it halts at `H − 1` by itself, exit 11), or restart the new one with `--halt-height H−1` ([Coordinated restart at a height](#coordinated-restart-at-a-height)). Once ⅓ of the set has stopped, the chain stops at `H − 1`: check every node stopped at the same height and hash.
3. Ship an emergency release whose binary supports the scheduled version (its `SUPPORTED_PROTOCOL_VERSION` is that version) and runs the rules the operators agreed for it, for instance the previous version's rules unchanged ([Emergency patch path](#emergency-patch-path)). That spends the version number: the real upgrade comes later under the next one.
4. Operators install it and start without `--halt-height`; the chain resumes at ⅔ and runs block `H` under the agreed rules.

A restart takes a validator out until it's back: it loads the state database, executes at most the few blocks logged after its last commit, gets its p2p links back and watches the network before it votes again ([Slashing protection](#how-slashing-protection-works)). With the `public-nat` and `local` p2p presets that takes seconds; with `recommended` it can take a few minutes, since each peer that tried the node while it was down waits 60–120 s to dial it again and refuses it for 60 s (Commonware's reconnection cooldown). It's back once it signs again (`blockula_slashing_protection_abstaining` 0), is linked to every other validator (`blockula_p2p_peers` at least n − 1) and has proposed a block since it started.

**On-disk formats** carry versions: `consensus.version` (`STORAGE_VERSION`, the consensus journals), the state database's format and state schema (`STORE_FORMAT`, `SCHEMA_VERSION`; a mismatch rebuilds the database from the block logs on start, which takes as long as replaying the history), and the snapshot schema. Release notes say when a format changes and what the first start will do.

**Coordinated stop at a height** (incidents, emergency patches): `blockula run … --halt-height H` executes block `H` and stops. A validator doesn't vote on or propose anything above `H` and exits with code 10 (and again at once on every start with the same flag); a full node keeps serving reads at `H`. If at least ⅓ of the set runs with the same `H`, the chain itself stops at `H`. Remove the flag (or raise it) to continue.

The public testnet (`blockula-testnet-2`) runs the same version gating as mainnet: a release that changes no rules is a redeploy (`deploy/launch.sh`, validators in parallel, so blocks pause briefly), and a new protocol version goes through a `ScheduleUpgrade` the validators pass as its bootstrap council, as above, which is how upgrades are rehearsed before mainnet ([deploy/README.md](../deploy/README.md), "Day 2").

## Incident response

### Severities

| Sev | Examples | Response |
|---|---|---|
| **SEV-1** | Chain halted. Safety failure: conflicting finalizations, a state-root divergence between honest nodes. An exploit minting or moving funds. Compromise of keys for ≥ ⅓ of the set. | Page the on-call and security lead now. Incident commander within 15 min. Operator channel. Public status update within 30 min. |
| **SEV-2** | Liveness degraded: finality lagging chain-wide, more than ¼ of validators down. A critical vulnerability reported privately with a credible exploit path. Public RPC down. | Page the on-call. IC within 1 h. Operator channel. |
| **SEV-3** | One validator down or jailed. A non-critical bug. A single sentry down. | Owner handles it in working hours. |

### Who decides

- **Incident commander (IC):** the core-team on-call engineer, until they hand over. The IC runs the incident, decides what to ask of operators, and owns communication.
- **Security lead:** decides embargoes, private patch distribution and disclosure timing, together with the IC.
- **Operators decide for their own nodes.** There are no admin keys on mainnet (DEC-10): the team can ask operators to stop at a height or install a patch, but each operator verifies and acts. A fix takes effect when ≥ ⅔ of the set runs it.
- **Governance** makes any change to state or parameters after the incident (no emergency admin path).

### Channels

- **Private operator channel:** an invite-only, end-to-end-encrypted group (Signal or Matrix with E2EE) for every active operator and the core team, plus a mailing list as a fallback. Every operator registers two reachable contacts (name, phone, messenger handle) when they join the set. The contact list is re-confirmed quarterly with a test page.
- **Public:** the status page (a hosted status page on its own domain and provider, so it stays up when we're down), the project's social accounts and the GitHub security advisory.
- **Security reports** come in through [SECURITY.md](../SECURITY.md) and the [bug bounty](BUG_BOUNTY.md).

### Playbooks

**Chain halted (SEV-1).** Find out why before acting:
1. `BlockulaChainHalted` fired, or the explorer stalled. Check `/api/status` on several nodes: same height everywhere?
2. **Not enough voters** (validators down or partitioned): no node logs execution errors, and certificates stop. Get operators to bring nodes back. Nothing else needed.
3. **Deterministic halt:** every validator fails the same finalized block (an execution error or a panic in end-of-block code), and does again on every restart (OPS-5). Logs on all nodes show the same height and error. Then:
   - Stop restarting. Collect logs, the failing block (`/api/block/{height}`) and a repro.
   - Write the fix so it changes behaviour **only from the failing height on** (height-gated in code: `if height >= H_fix`), so history replays identically. A fix that changes consensus rules for later blocks is a protocol upgrade: give the binary a higher `SUPPORTED_PROTOCOL_VERSION` and schedule it once the chain moves ([Upgrades](#upgrades)).
   - Ship it as an emergency release ([Patch distribution](#patch-distribution)), verified like any release (`blockula release install` checks its signature against the pinned key).
   - Operators install it and start. The chain resumes when ≥ ⅔ of the set runs it.

**Safety failure (SEV-1):** two certificates for different blocks at one height, or nodes reporting different state roots for the same height. Ask every operator to stop their validator at once (stop the service; `--halt-height` only if a height in the near future is agreed), preserve disks and logs (copy them aside: the node deletes consensus journals older than two epochs), and don't restart until the IC calls it. Equivocation evidence from the consensus journals (`track_historical_votes`) identifies the signers (planned: P-STAKE).

**A validator exited with code 12 ("another instance is running with this validator.key"):** slashing protection saw this key sign something the process didn't. Treat it as a key compromise until proven otherwise: keep the node stopped, find every machine that has the key (old hosts, restored volumes, a copy started by automation), and stop them. Only then start one copy. If you're certain no other copy exists (for example the node had just been restored from an old volume while the network was halted), wait for the next epoch boundary before starting it. The node keeps itself stopped meanwhile: it recorded the stop in `double-sign-halt.json` in its home and exits 12 on every start until you start it once with `--i-know-this-key-is-not-running-elsewhere` (or delete that file). On Fly: `fly ssh console -a <app> -C "rm /data/home/double-sign-halt.json"`, then `fly machine restart`.

**Key compromise:** the operator stops the validator at once. In protocol v1 the stake belongs to the cold owner account, so a stolen consensus key can't move it, nor the owner's balance (the system txs it signs for the owner carry no tip), but it can still equivocate and get the pool slashed, and make the owner pay base fees for system txs. Protocol v1 has no consensus-key rotation: the owner undelegates the self-bond, registers a new validator with a fresh key generated offline, and asks delegators to redelegate (P-STAKE). Report it in the operator channel so others watch for votes from the old key.

**Vulnerability reported privately:** the security lead triages within the SECURITY.md targets, decides severity with the IC, and runs the private patch flow below. Credit and bounty per [BUG_BOUNTY.md](BUG_BOUNTY.md).

### Patch distribution

For a vulnerability that's exploitable before a fix is widely deployed:

1. Fix and review in a private fork (a GitHub private security advisory fork). Add a regression test.
2. Tag a release candidate in the private fork. Build it reproducibly (`scripts/release-build.sh`), and have at least one more maintainer rebuild it and match the hashes.
3. Put the archives in a directory of their own (nothing else: no `install.sh`, which operators don't use) and write `checksums.txt` with `scripts/release-checksums.sh vX.Y.Z <that dir>` (its first line names the version, so the signature binds the files to it). Sign it with the release key ([Releases](#releases)) and send the directory (archives, `checksums.txt`, `checksums.txt.sig`) and the source commit to operators through the private channel. Don't send the diff until enough of the set has upgraded, if the diff itself reveals the bug.
4. Operators install it with the binary they already run, which checks the signature against their pinned release key id, the version on `checksums.txt`'s first line (an older signed release can't pass for the patch), the archive's hash and the binary's version. The patch was built outside GitHub, so it has no build provenance, and the command doesn't ask for any: the offline signature is the check. `V` is the patch's version. Then confirm in the channel. Track it in a checklist: ⅔ of the set is the target, everyone is the goal.

<!-- runbook-test: patch-install -->
```bash
( set -euo pipefail
  V=vX.Y.Z
  P=<the directory the patch came in>
  /usr/local/bin/blockula release install $V --from "$P" --to ~/.blockula/bin/blockula
  sudo install -m 755 ~/.blockula/bin/blockula /usr/local/bin/blockula
)
```

5. When ≥ ⅔ have upgraded (or the activation height passes), publish the source, the public release (normal pipeline, same hashes) and the advisory.

### Coordinated restart at a height

1. The IC picks a halt height `H` (at least 30 minutes out, so everyone can act) and announces it with the reason.
2. Each operator restarts once with `--halt-height H` (a normal rolling restart, paced as in [Upgrades](#upgrades): the node votes again once it has its p2p links back and has watched the network) and confirms in the channel. Their sentries and RPC nodes can take the same flag; they keep serving reads at `H`.
3. At `H` the validators exit with code 10; once ⅓ have, the chain stops at `H`. Check that every node stopped at the same height and block hash (`curl -s 127.0.0.1:7070/api/block/H | jq .hash` on each, from their logs if the process is gone).
4. Operators install the new binary and start it **without** `--halt-height`. Every validator restarting at once means nobody's votes are observed at first: they all sign again after ~20 s, once their HTTP peers show the network standing still at `H` ([Slashing protection](#how-slashing-protection-works)). One peer lying about being ahead, or one validator sending traffic for a later epoch, can't hold them back (it's outvoted). A validator that can't reach any peer's HTTP API, or whose peers that claim to be past `H` aren't outnumbered by peers at `H`, keeps abstaining (its log says which): fix its `--peer-http`, or start it with `--network-halted-at H`, which overrides unverified answers. The chain resumes when ≥ ⅔ are back.
5. The IC confirms finality is advancing (`BlockulaChainHalted` clears) and closes the step.

### Emergency patch path

For a deterministic halt, or a vulnerability that must be closed before it's public:

1. Private fix with a regression test, height-gated (see the playbook above), reviewed by a second maintainer.
2. Reproducible build, signed offline ([Patch distribution](#patch-distribution)): operators install it with `blockula release install vX.Y.Z --from <dir>` (step 4 there), which checks the signature against their pinned release key id before anything from the patch runs.
3. If the chain is running: a coordinated restart at a height onto the patched binary (above). If it's halted: operators install and start; the chain resumes at ⅔.
4. Afterwards: the public release, the advisory, the post-mortem.

### Communication templates

Initial public notice:
> **[Investigating] Blockula block production stopped at height {H}.** We're investigating and will update by {time UTC}. Funds on chain are not at risk from a halt; don't resend transactions. Updates: {status page}.

Operator page:
> **SEV-{n}: {one line}.** Since {time UTC}, height {H}. Action for every validator: {exact action, or "none yet, stand by"}. IC: {name}. Next update {time}. Reply here when done.

Update (at least hourly during a SEV-1):
> **[Identified/Monitoring] {what we know}.** {what operators and users should do}. Next update {time UTC}.

Resolution:
> **[Resolved] Blocks are finalizing again since {time UTC} (height {H2}).** Cause: {one sentence}. A post-mortem follows within 7 days.

Post-mortem (published): summary; impact (duration, heights, who was affected); timeline in UTC; root cause; what went well and badly; action items with owners and dates. Blameless.

### Tabletop exercise

Run one before the incentivized testnet ends (OPS-5), then every six months. Template:

| | |
|---|---|
| Scenario | e.g. "Every validator panics at height 1,204,311 in end-of-block code; restarts repeat it." |
| Participants | IC, security lead, ≥ 5 operators, comms owner |
| Injects (every ~10 min) | 1. Alerts fire. 2. A user reports funds "missing". 3. A journalist asks. 4. The fix works on 60% of nodes, not the rest. 5. One operator is unreachable. |
| Expected actions | Severity set, IC named, operator page sent within 15 min, public notice within 30 min, a height-gated fix, coordinated restart. |
| Measure | Time to page, to diagnosis, to the ⅔ upgrade. Who couldn't be reached. Which runbook steps were wrong. |
| Output | Action items, with owners, filed as issues. Runbook fixes merged within two weeks. |

## Runbooks

Each alert in `deploy/monitoring/prometheus/alerts.yml` links here.

### Node down

`/metrics` doesn't answer. `systemctl status blockula`, `journalctl -u blockula -n 200`. If it exited, read the last error before restarting: a double-sign guard refusal means a copied home or another machine with this key (see [Keys, backups and restores](#keys-backups-and-restores)); "written by an older Blockula" means the on-disk format changed (follow the release notes). If the process runs but HTTP hangs, capture the threads' stacks (`gdb -p $(pidof blockula) -batch -ex "thread apply all bt" > stacks.txt`) before restarting, and attach them to an issue.

### Head stale

This node hasn't executed a block for over 10 s, but others have. Check p2p reachability (the firewall, `ss -tnp | grep 30300` for established peers), the clock ([Time](#time)), and execution lag (next runbook). A node that just restarted is replaying; wait for `BlockulaRestarted` to clear.

### A set of one or two

If jails, tombstones or under-bonded owners leave the active set with one or two eligible members (`/api/validators` shows `admission`; `blockula validator list` says so), every member is needed for each block, and so is the next newcomer. It joins only with the members' consent: each member's operator that trusts the candidate's operator to keep its node online runs `blockula validator admit <candidate address> --key <home>/validator.key --rpc …` (signed by the member's consensus key, paid by its owner; one admission per member and epoch). It takes the candidate's address only, never a name: get it from the candidate's operator and check it with them (`blockula validator list` shows it under candidates), since a node's answer for a name could be anyone's candidate and an admission can't be taken back. Admit early in the epoch: an admission counts for the epoch it lands in only. Once a quorum of the members (both of two; the one of one) admitted it, the candidate's node signals readiness by itself in the epoch's last tenth (`ValidatorReady`; its log says "showed up to join the validator set") and joins at the boundary. One that isn't admitted, or whose node is down in that window, stays out; admit it again the next epoch. Never admit a stranger: a newcomer that stops voting halts such a set for good (only a hard fork recovers), as any member could. From three members on, newcomers join on stake within the churn limit, without admission.

### Chain halted

No node you monitor has a fresh head. Check the explorer and the operator channel: if the whole network stopped, it's a SEV-1 ([Chain halted playbook](#playbooks)). If only your nodes stopped, they're cut off: check the network path and your provider's status.

### Execution lagging

Blocks are finalized but this node executes them slowly. Past 16 blocks (`MAX_EXEC_LAG`) its proposals are invalid. Causes: CPU starvation (other workloads, a burstable instance out of credits), slow disk fsyncs (`iostat -x 1`), or a replay after restart. Move to dedicated cores and local NVMe if it recurs.

### Mempool full

The mempool is near its 5,000-tx cap, so new txs get refused. During a spam wave this is expected and protects the node. If the mempool stays full while blocks are empty, txs aren't being included: check that this node proposes when it's leader (execution lag, clock) and ask in the operator channel whether others see the same.

### Restarts

The node restarted in the last 15 minutes. Was it you? If not: `journalctl -u blockula -b` for the exit reason, `dmesg | grep -i oom` for OOM kills (then see [Memory](#memory)), and the provider's console for host maintenance.

### Validator set

`BlockulaCertificateBelowQuorum`: the newest certificate has fewer signers than this node thinks the active set needs. That shouldn't happen: the node's view of the set disagrees with the certificates it holds. Compare `/api/validators` on several nodes and report in the operator channel with the output. `BlockulaNoRecentCertificate`: the node isn't receiving finalization certificates; it's syncing or cut off from the validators (check p2p, as in [Head stale](#head-stale)).

### Peers

`BlockulaFewPeers`: this validator is connected to fewer peers than a quorum needs besides itself, so its votes and proposals may not reach enough of the set. Check the firewall (p2p port open inbound), that the advertised `p2p_addr` is this host's public address, `ss -tnp | grep 30300` for established connections, and the clock. Other operators' nodes being down shows up here too: compare with the operator channel.

`BlockulaMissingPeers`: this validator, in the active set, isn't linked to every other validator; votes and blocks go only over direct links, so on a small set that costs blocks and leader slots. After a restart (its own or a peer's), validators on the `recommended` p2p preset can take a few minutes to re-link (the reconnection cooldown; `public-nat` and `local` take seconds): don't restart another validator until it clears. Otherwise check the same things as for `BlockulaFewPeers`.

### Halted

`BlockulaHalted`: the node stopped executing on purpose. `curl -s 127.0.0.1:7171/api/health` (or `:7070`) says why:

- `halt_height`: it was started with `--halt-height` and reached it. Expected during a coordinated restart; otherwise remove the flag.
- `upgrade_required`: the protocol version in force at the next height isn't supported by this binary. Install the release for it ([Upgrades](#upgrades)) and restart.

Validators exit instead (code 10 or 11, `BlockulaNodeDown`): `journalctl -u blockula -n 50` shows the same reason.

### Upgrade required

`BlockulaUpgradeRequired`: governance scheduled a protocol version this binary doesn't run (`curl -s 127.0.0.1:7070/api/upgrade | jq .next`). Install the announced, signed release before the activation height ([Upgrades](#upgrades)); the node will otherwise halt there. `…Soon` means it's within a day.

### Slashing protection

- `BlockulaSlashingProtectionRolledBack` (critical): the record went backwards, so the home was restored or copied. Stop the validator and follow [Keys, backups and restores](#keys-backups-and-restores) step 1: make sure no other copy runs.
- `BlockulaWatchingAfterStart`: a started validator is still watching the network minutes later. It signs once the network has certified a few views without it, or after ~20 s if the network can't progress without it, so minutes means it isn't receiving certificates: check [Peers](#peers) and the logs (`slashing protection` lines).
- `BlockulaValidatorNotSigning`: in the active set, blocks keep coming, and this validator signed nothing for 10 minutes. Look for `slashing protection refused a signature` (a conflict with the record: never delete the record to "fix" it) or `couldn't write the record` (disk full or read-only) in the logs.
- Exit code 12: see the playbook in [Incident response](#playbooks).

### Jailed or tombstoned

`/metrics` exports, per validator, the leader slots it missed in its downtime window (`blockula_validator_missed_slots`, jailed past `blockula_downtime_max_missed`) and its jail and tombstone flags; this node's own validator carries `own="1"`.

- `BlockulaValidatorMissingSlots`: it has missed half the slots its window allows. It's proposing nothing when its turn comes: check [Peers](#peers), [Time](#time), CPU, and that its blocks aren't refused (`rejecting block` lines on other nodes mean its proposals are invalid, for example a stale binary).
- `BlockulaValidatorJailed`: it missed too many slots (or committed to the randomness beacon and didn't reveal, so check that `beacon-secrets.json` survived), lost a light slash, and leaves the set at the next epoch boundary. Fix the cause, wait out `jail_blocks` (`curl -s 127.0.0.1:7070/api/validators | jq '.jailed'` shows `until`), make sure the self-bond is still at least `min_self_bond`, then have the owner send `Unjail` (`blockula validator unjail <validator address> --key <owner seed file> --rpc …`). It rejoins at a boundary once its stake ranks.
- `BlockulaValidatorTombstoned`: it signed two conflicting votes. Its pool was slashed and it can never be active again. Treat it as a key compromise or a copied home ([Keys, backups and restores](#keys-backups-and-restores)): stop every copy, find out how two instances signed, and register a new validator with a new key. Delegators should move their stake (`blockula stake undelegate`, then `withdraw` after unbonding).

Validators pay for their own system transactions (attestations, beacon commits and reveals, equivocation reports) from the owner account when the owner is a separate account (SF-13); `BLOCKULA_SYSTEM_FEES=self` makes the consensus key's own account pay instead. Keep a fee float there. They carry no tip, so a congested mempool keeps them in a small lane of their own (never evicted, first in proposals) rather than refusing them, as long as nothing else from the paying account is queued ahead of them: under congestion, send nothing untipped from the account that pays them (or set `BLOCKULA_SYSTEM_FEES=self`). A duty that stops applying while it waits is dropped, unrun and uncharged, and so is everything the paying account queued behind it, which waits on its nonce ("an earlier transaction from the same account was dropped"). That's routine for claim attestations: every active validator attests each claim, and one that arrives after the attestations that settle it, but before their block runs, is dropped. An owner tx signed in that block or two comes back dropped: check it (`tx_status`, `client.tx`) before sending it again, since it can still run if the gap ahead of it fills and it's resubmitted, or keep the owner's own txs clear of the duties with `BLOCKULA_SYSTEM_FEES=self`. Once Ghost Mode is on, don't shield from that account: nothing queues behind a shield, so its duties are refused until the shield is included (a missed beacon reveal gets the validator jailed). On a faucet chain, the faucet drips from the consensus key's account, so with `BLOCKULA_SYSTEM_FEES=self` (or a validator that owns itself) the drips queue ahead of its duties too. Pending equivocation evidence waits in `evidence.json` across restarts. Signed for the owner, a system tx whose action would fail is refused rather than charged, and the consensus key may update the validator for its owner once an epoch (audit F04). The validator's terms (commission, addresses) and `Unjail` are the owner's call: the consensus key never changes them from the validator's own account when the owner is another account, so a multisig owner's changes go through its approvals (`blockula multisig propose`). While the active set carries over only one or two members, a candidate node that a quorum of them admitted also sends `ValidatorReady` once, in the epoch's last tenth ("ready to join the validator set", [A set of one or two](#a-set-of-one-or-two)): it changes nothing but its readiness record, and it's a lane duty, like a member's admission. Both go through a multisig owner like the other system txs: no `BLOCKULA_SYSTEM_FEES=self` needed.

### Disk

Above 80%: grow the volume now (online resize: grow the block device, then `resize2fs` or `xfs_growfs`). Find what grew: `du -sh /var/lib/blockula/val/*`. `blocks/` and `cblocks/` stay within the pruning window (`--prune-blocks`; shorten it if the volume can't grow); `consensus/` holds marshal's last 2,000 blocks and the current and two previous epochs' simplex journals. Don't delete files by hand from a running validator's home. Never let a validator's disk fill: a failed fsync stops the node.

### Memory

Less than 10% available or an OOM kill. The state is in memory (history isn't), so memory grows with the number of accounts and records. Check `ps -o rss,cmd -u blockula`; move to the recommended 32 GB. Don't run other services on a validator.

### Alerting pipeline

The alerting pipeline itself: `Watchdog` should always reach your heartbeat service; `AlertmanagerNotificationsFailing` means a receiver is misconfigured or down (check `docker compose logs alertmanager`); `PrometheusRuleFailures` means a rule broke (check Prometheus's /rules page). See [deploy/monitoring/README.md](../deploy/monitoring/README.md).

## Weak subjectivity

A joining node checks every block, snapshot and certificate against a validator set, and learns each set from the one before it: 2f+1 of the outgoing set sign a handoff to the next, back to the genesis (MAINNET-DESIGN 3.10). That chain has a weak point. Handoffs can be signed at any time and aren't slashable, and a set whose stake has unbonded has nothing left to lose: its keys can sign a handoff to a set they made up, retroactively, and that set can then certify any history. A node that trusts only the genesis can't tell the difference. Every proof-of-stake chain has this property; the answer is to start from a recent root instead of the genesis.

**The period.** Stake stays slashable for the evidence window: `evidence_max_age_epochs` epochs, never more than `unbonding_blocks` (21 days at genesis). The weak-subjectivity period is half of that: **907,200 blocks, about 10.5 days**, at genesis parameters. The other half leaves time to notice a forgery, submit the equivocation evidence and slash before the forgers' stake is gone. `/api/status` shows the period as `weak_subjectivity_blocks` (with the current parameters) and the newest checkpoint the node trusts as `checkpoint_height`.

Both parameters are governance's (the evidence window is the token holders' alone, by supermajority; `unbonding_blocks`, which the bootstrap council can also change, can't fall below it), and a shorter window shortens the period, but never below **a week**: governance can't take the evidence window below two weeks (`params::EVIDENCE_WINDOW_FLOOR_DAYS`, twice the weekly checkpoint cadence below), so the weekly signed checkpoint is always fresh enough. A node enforces the shortest period it knows of: the genesis parameters', the one its release recorded with its newest checkpoint (`weak_subjectivity_blocks` in `node/checkpoints/<chain id>.json`, set when governance has shortened it), once it has executed the chain the one its own state's parameters set, and, before it syncs, the shortest one its peers report in `/api/status` (`join` asks every peer it will use; a full node asks on every start, and with an empty home waits until a peer answers). A peer's report only ever shortens the period, and never below the week the protocol allows, so a lying peer can at worst ask you for a checkpoint a week old. When a peer reports a shorter period than your root meets, `join` (or the first start) refuses and says which peer and what period; take a fresher checkpoint (the weekly one, or `blockula checkpoint`) or pass `--allow-stale-checkpoint`. One honest peer is enough; a joiner whose every peer lies after a shortening is only protected by a checkpoint younger than a week, so list peers run by independent operators. A fast-synced node also warns when the state it installed sets a period its root was older than.

**Test chains** may start below that floor, and then only move up: the public testnet (`blockula-testnet-2`) has a week of unbonding and of evidence, so its period is **3.5 days** (302,400 blocks), and `blockula dev` has minutes. A node joining the testnet later than that needs a checkpoint: `blockula checkpoint --peer https://blockula-testnet.fly.dev --genesis-hash <hash>`, a release's compiled-in one while it's fresh, or, for the testnet's own public node, one its validators agree on over the operator's private network ([deploy/README.md, Sync trust](../deploy/README.md#sync-trust)). Every such checkpoint is one operator's word until the testnet has independent operators.

**What a node trusts:**
- **The genesis hash.** `join` refuses a genesis nobody pinned: either this release knows the chain id (the hash is compiled in from `node/checkpoints/<chain id>.json`) or you pass `--genesis-hash`. A known chain's genesis must match the compiled-in hash, for validators too. `node/checkpoints/blockula-1.json` is a placeholder until the genesis ceremony, so every binary refuses `blockula-1` until the release that launches mainnet fills in the hash the genesis builders reproduced ([GENESIS_CEREMONY.md](GENESIS_CEREMONY.md), section 6).
- **Checkpoints**, `(height, block hash, validator-set hash, block time)`: compiled into each release, plus your own with `join --checkpoint HEIGHT:HASH[:SET_HASH[:TIMESTAMP_MS]]` (repeatable; saved in the home's `join.json` and enforced on every start; `HEIGHT:HASH::TIMESTAMP_MS` leaves the set out). The set hash is of the set that certifies the block after `height`; the time is the block's `timestamp_ms`, which its hash covers (`blockula checkpoint` prints all four, and release checkpoints carry them). Pinning it is what defeats a forged history, since every later set must descend from it. A node refuses a genesis, snapshot, epoch anchor or sync batch that contradicts a checkpoint. It also refuses a snapshot it can't check against one: a checkpoint older than the snapshot that pins no set.
- **Validator-set history.** Nodes serve it in `/api/epochs?from=` pages and keep the weak-subjectivity period of it plus a quarter (older changes are dropped once it passes 8 MiB). A joiner fetches and verifies only the history after its newest root, keeps only what it verified, and its validator signs handoffs only for set changes its own state shows.
- **Freshness.** A node with an empty home (`join`, or `run` before its first sync) refuses to sync when its newest root (the newest checkpoint, or the genesis) is older than the period (the shortest of this release's and its peers' reports, above). Age is measured against the highest the chain can have reached by the wall clock, counted from the newest block whose time the node trusts (the newest checkpoint that records its time, or the genesis): blocks are at least a block time apart, so missed slots only make a root look older, and only those missed since that block count. A chain's missed slots add up over its lifetime (every timed-out view, every outage), so a checkpoint without its time would be measured from the genesis and, on an old chain, could look older than the period even when taken at the tip; one with its time is as old as its time says (review-r3-sync-1). An empty home whose newest checkpoint has no time (the older `HEIGHT:HASH[:SET_HASH]` forms) reads it from the first peer that serves that block (`/api/sync`: the block itself, whose hash covers its time), and counts from the genesis only when none does (validators keep about two days of blocks; full nodes keep everything). The time is part of what you trust, like the hash: a node refuses a checkpoint whose time is earlier than the genesis (or an earlier timed checkpoint) allows or later than its clock, a block at that height carrying another time, and a later block earlier than that time allows. `--allow-stale-checkpoint` overrides the refusal. A node with history only warns: its own head is its root, and after an outage longer than the period it's in the same position as a joiner.
- **A checkpoint newer than every snapshot** (you took it a minute ago): a peer serves the snapshot reaching furthest, the joiner sees it ends before the checkpoint and waits up to 5 minutes for the next snapshot (one every 100 blocks) before it falls back to replaying from the genesis.
- **Time per peer.** A fast-sync attempt against one peer's manifest has one deadline for the validator-set history and the chunks: 2 minutes plus 2 s per epoch the history spans (at most 30 minutes), plus the download's budget (below). A manifest certifying blocks past the height the chain can have reached by the clock (plus an hour) is refused before anything else is fetched for it, a history page that says there's more must hold a set change, and a peer that runs out of time loses the attempt to the next peer.
- **Download time.** Nodes serve a state bigger than 32 MiB (compressed) from their newest long-lived snapshot, which stays on disk at least 15 minutes after a joiner fetches its manifest; a smaller state from the newest snapshot, which stays at least 100 blocks. A joiner that can't fetch the chunks in that time gets 404s and moves on to the next peer's manifest (a newer snapshot), so it needs about 5 MiB/s at the 4 GiB maximum. It fetches the chunks from every peer it knows that holds the same snapshot, drops a peer that stalls (30 s per 1 MiB chunk) or serves a wrong chunk, prefers the faster ones, and gives up on a manifest after 2 s per chunk plus 2 minutes. A checkpoint at or before the newest long-lived snapshot (the default `blockula checkpoint` height, an epoch boundary, usually is) lets the joiner use it; a newer one leaves it the 100-block budget.

**Getting a checkpoint.** Take it from several sources that don't depend on each other: the latest release (compiled in, and attached as `checkpoints-<chain id>.json`), the signed weekly checkpoint (below), operators you know. Or compute it from nodes run by independent operators:

```bash
blockula checkpoint --peer https://rpc.op-a.example --peer https://rpc.op-b.example --peer https://rpc.op-c.example
# 3600599:4f1c…:9a0e…:1798761599000        HEIGHT:HASH:SET_HASH:TIMESTAMP_MS
blockula join --genesis https://rpc.op-a.example --home /var/lib/blockula/full --checkpoint 3600599:4f1c…:9a0e…:1798761599000
```

`blockula checkpoint` reads each node's genesis, the block at the height (its hash and time) (default: the newest epoch boundary every peer has finalized; `--height` picks another) and its validator-set history (`/api/epochs` pages), verified from the newest set this release trusts (or, when the node no longer keeps history that old, from the oldest set it keeps, with a warning), and fails unless every node agrees. Agreement among independent operators is what a checkpoint attests; one node's word is worth no more than its own snapshot. `blockula checkpoint --genesis genesis.json` prints a genesis file's hash for `--genesis-hash`.

### Checkpoints in releases

Every release carries a fresh checkpoint for each live chain it knows, in `node/checkpoints/<chain id>.json`. It's compiled into the binary and attached to the release as `checkpoints-<chain id>.json`, listed in `checksums.txt`, so the offline release signature covers both.

1. In the release PR, take the checkpoint from at least three nodes run by different operators, not only the project's: `blockula checkpoint --json --peer … --peer … --peer …`. Append it to the file's `checkpoints` (keep the earlier ones; `scripts/pin-network.sh <chain id> <genesis hash> --checkpoint HEIGHT:HASH:SET_HASH` does it, and pins a new chain) and note in the PR which operators' nodes you asked and when. A chain with one operator (the public testnet today, all of whose nodes the project runs) can't meet that: take it from several of its nodes (`--peer` each validator and the public node) and say in the PR that it's one operator's word, which is what it is for joiners too ([deploy/README.md, Sync trust](../deploy/README.md#sync-trust)).
2. Two reviewers rerun it against nodes they pick, with `--height <H>`, and must get the same entry.
3. Tag and release as in [Releases](#releases). A release's checkpoint is only useful to joiners for the period after it was taken, so take it within a day or two of the tag.
4. **Between releases**, roots still go stale after ~10.5 days (less, if governance has shortened the evidence window, never less than a week: then also set `weak_subjectivity_blocks` in the release's file to the period the nodes report in `/api/status`). Every week the release team takes a checkpoint the same way (steps 1 and 2) and signs it on the offline machine like `checksums.txt`, with the same verified binary: `blockula release sign --key release-key.json checkpoint-<chain id>-<height>.txt > checkpoint-<chain id>-<height>.txt.sig`, where the file holds the `HEIGHT:HASH:SET_HASH:TIMESTAMP_MS` line. It's published with the signature through the same channels as the release key id; joiners verify it with `blockula release verify --sig checkpoint-<chain id>-<height>.txt.sig checkpoint-<chain id>-<height>.txt` (against the release key id they pinned; `--key-id` names one explicitly) and pass it with `--checkpoint`.

A new chain gets its own file (and a line in `EMBEDDED`, `node/src/checkpoints.rs`) in the first release after its genesis, with the genesis hash and a checkpoint if it's already older than a day or two: `scripts/pin-network.sh <chain id> <genesis hash> --genesis genesis.json [--checkpoint …]` writes both, the MCP's `KNOWN_GENESIS` (`mcp/src/network.ts`), the wallet's built-in testnet genesis (`web/lib/config.ts`) and `genesis/<chain id>/` (the genesis the release attaches as `genesis-<chain id>.json`, with its spec and operator files).

## Releases

Every release is reproducible, carries SLSA build provenance, and is signed offline with the ML-DSA-65 release key before it's published (OPS-7).

### How a release is made

1. A maintainer tags `vX.Y.Z` on `main` and pushes the tag. `.github/workflows/release.yml`:
   - builds the four targets with `scripts/release-build.sh`: the toolchain pinned in `rust-toolchain.toml`, Linux in a digest-pinned `rust` image, `--locked`, `SOURCE_DATE_EPOCH` from the commit, build paths remapped, deterministic archives;
   - rebuilds the x86_64 Linux archive on a fresh runner from a different directory and fails unless it's byte-identical;
   - refuses a tag that isn't `vX.Y.Z` or isn't `node/Cargo.toml`'s version, so `blockula --version` prints the tag;
   - writes `checksums.txt` with `scripts/release-checksums.sh`: a first line naming the tag (`# blockula-release vX.Y.Z`), then the archives, `install.sh`, committed genesis files and the checkpoint files `node/checkpoints/*.json` (see [Checkpoints in releases](#checkpoints-in-releases)), and attests every file with GitHub artifact attestations (SLSA provenance, `actions/attest-build-provenance`). The tag line is what binds the offline signature to this version: without it, an older release's genuine signature would pass for a newer one (audit supply-chain-B-2);
   - creates a **draft** release. Only that last job can write to the repo.

   Everything up to here runs on GitHub, so OPS-7's adversary (a compromised GitHub account, runner, action or release workflow) can change any of it: the tag, the archives, `install.sh`, the genesis and checkpoint files, `checksums.txt`, the provenance. Steps 2 and 3 are what the signer checks before vouching for it. Write down the commit you reviewed for the release (the release PR's merge commit) yourself, when you review it: it's `COMMIT` below.
2. **Reproduce.** At least two people rebuild the Linux archives independently and compare against the draft:

   ```bash
   gh release download vX.Y.Z --repo jackdishman/blockula --pattern checksums.txt --dir /tmp/rel
   COMMIT=<reviewed commit> CHECKSUMS=/tmp/rel/checksums.txt scripts/reproduce-release.sh vX.Y.Z                            # this machine's arch
   COMMIT=<reviewed commit> CHECKSUMS=/tmp/rel/checksums.txt scripts/reproduce-release.sh vX.Y.Z aarch64-unknown-linux-gnu  # emulated on x86: slow
   ```

   With `COMMIT`, the script refuses a tag that isn't that commit, and checks `checksums.txt`'s other lines as in step 3. Each builder posts the result in the release issue and sends the signer the line it prints (`<sha256>  blockula-<target>.tar.gz`) on the operators' channel, which GitHub doesn't control: GitHub serves the draft to each of them separately, so the signer must sign the archives the builders rebuilt, not just a draft that says so. macOS archives reproduce only with the same Xcode as the release run (its log prints `xcodebuild -version`).
3. **Check what you'll sign**, on an online machine with a checkout you trust, against the exact `checksums.txt` you'll carry offline:

   ```bash
   # rebuilt.txt: the builders' lines from step 2, as they sent them.
   COMMIT=<reviewed commit> REPRODUCED=rebuilt.txt scripts/check-release-files.sh vX.Y.Z /tmp/rel/checksums.txt
   gh attestation verify /tmp/rel/checksums.txt --repo jackdishman/blockula \
     --signer-workflow jackdishman/blockula/.github/workflows/release.yml --source-ref refs/tags/vX.Y.Z
   ```

   `scripts/check-release-files.sh` refuses unless the tag is the commit you reviewed, the first line is `# blockula-release vX.Y.Z`, both Linux archives are listed with the hashes the builders rebuilt (and they agree), `install.sh`, every `genesis-<id>.json` and every `checkpoints-<id>.json` hash exactly like the commit's `scripts/install.sh`, `genesis/<id>/genesis.json` and `node/checkpoints/<id>.json` (and all of the commit's are listed), the four archives are listed, and nothing else is (audit F49, review-r2-supply-4). Provenance adds that the files came out of the release workflow.
4. **Sign offline.** Carry `checksums.txt` (only that file) to the offline signing machine. It holds the release key and one `blockula` binary, and nothing else: no checkout, no `npm ci`, no Node.js tooling. A signing process trusts every line of code it loads, and the monorepo's npm tree (the wallet's toolchain included, with install scripts) is far more than the signer needs (audit supply-chain-B-4). The binary is one the signer verified before taking it offline: their own reproduced build (step 2's `scripts/reproduce-release.sh`, whose archive matched; take `blockula` out of it), or a release fetched and checked as in [To an offline machine](#to-an-offline-machine). Note its SHA-256 in the release issue, and carry new binaries over only the same way. A binary built with an older release key than the one signing (after a rotation) still checks the new key's signature when `--key-id` names it, with a note that it isn't the key it trusts; move its pin with the rotation statement (`blockula release pin --key-id <new> --rotation …`, as operators do) so that its `install` and `fetch` trust the new key too.

   ```bash
   blockula release selftest                          # this binary signs the known-answer vector
   head -n 1 checksums.txt                            # "# blockula-release vX.Y.Z": the release you mean to sign
   blockula release sign --key release-key.json checksums.txt > checksums.txt.sig
   blockula release verify --key-id <release key id> --tag vX.Y.Z --sig checksums.txt.sig checksums.txt
   ```

   Carry `checksums.txt.sig` back. `scripts/release-sig.mjs` has no `sign`: it only verifies, for machines that have a checkout.
5. **Publish.** `gh release upload vX.Y.Z checksums.txt.sig --repo jackdishman/blockula`, check the draft once more, then `gh release edit vX.Y.Z --draft=false --repo jackdishman/blockula`. With GitHub's immutable releases enabled (repository settings), nobody can swap an asset after that.
6. **Announce** it in the operators' channel and publicly: the tag, the commit, the release key id, and the SHA-256 of each archive (from the signed `checksums.txt`). A first install checks the archive against this announcement ([A first install](#a-first-install)), so post it from the signer's own account on a channel GitHub doesn't control.

**What the signature vouches for.** Exactly what steps 2 and 3 checked: the tag is the commit the signer reviewed; the Linux archives are the ones two builders rebuilt bit for bit from it, by the hashes they sent; `install.sh` and the genesis and checkpoint files are its own; nothing else is listed. The macOS archives are signed on provenance, plus a rebuild only where someone has the release run's Xcode, so for them a compromised release workflow is caught only by such a rebuild. Validators run Linux. A compromised GitHub side that changes anything the signer checks gets a release the signer refuses; one that changes a macOS archive gets past the signer unless someone rebuilds it.

### The release key

- One ML-DSA-65 key, generated on the offline signing machine (set up as in step 4 above: the key and a verified `blockula`, nothing else) with `blockula keygen > release-key.json`. Its **key id** is the hex BLAKE3 of the public key: `blockula release keyid --key release-key.json`. The key doesn't exist yet; generate it this way from the start, never on a machine that has had a checkout or an npm tree.
- Publish the key id in SECURITY.md, this file, `scripts/install.sh` (`BUILTIN_KEY_ID`), `node/src/release_cli.rs` (`BUILTIN_KEY_ID`, the key a fresh `blockula release install` trusts when nothing is pinned, and the only one it lets an operator pin by `--key-id`), `mcp/src/release.ts` (`RELEASE_KEY_ID`, the key the MCP's `install_node` requires), the website, the operators' channel and the project's social accounts, so a single compromised channel can't swap it. A test (`built_in_key_id_matches_install_sh`) fails unless the three built-in copies are equal. The `release_cli` unit tests pass their built-in id explicitly and don't depend on it, but `scripts/test-release-runbook.sh` signs its stand-in releases with keys it generates, so once a key id is built in, its first-install cases are refused ("this blockula was built with release key …") and the required Node check goes red (audit review-r4-supply-1). The ceremony PR has to give that test a binary built with its own test key id (for example a non-default cargo feature that takes the built-in id from a committed test key at compile time, never a runtime setting) before it fills in the real one; don't merge it by bypassing the check or by deleting those cases. **Release key id:** *not generated yet* (the maintainer fills this in at the key ceremony).
- **Operators pin it once.** Their first `blockula release install --key-id K` writes K to `~/.blockula/release-key-id` once the release verifies (`blockula release pin --key-id K` pins it by hand; `blockula release pin` shows it). A binary built after the key ceremony pins only the key id it was built with: given another, `install`, `fetch` and `pin` refuse and name both, since one of them isn't genuine (audit review-r3-supply-4). From then on, `blockula release install` and `fetch` check against the pin and refuse a different `--key-id`. `blockula release verify` checks against the pin (or, with nothing pinned, the built-in key) unless `--key-id` names one: it pins nothing, so it checks the key named and says when that isn't the key `install` would trust. `install.sh` relies on this: after a rotation it names the new key, which an older installed `blockula` wasn't built with (review-r4-supply-2). They never take the key id from a release again, or from SECURITY.md, which a compromised GitHub account can edit.
- Back it up like a validator key (encrypted, offline, two locations). **Rotation:** on the offline machine, sign a statement naming the new key id with the old key, and publish both through the same channels as the key id:

  ```bash
  printf '# blockula-release-key-rotation\nold %s\nnew %s\n' <old key id> <new key id> > rotation.txt
  blockula release sign --key release-key.json rotation.txt > rotation.txt.sig
  ```

  Operators cross-check the new key id on a channel GitHub doesn't control (the operators' channel), then move their pin, with `rotation.txt` and `rotation.txt.sig` from the announcement in the current directory. The pin moves only if the pinned key signed the statement, and it names the pinned key as `old` and the given key id as `new`. On a host with nothing pinned yet, whose binary was built before the rotation, the same command pins the new key if the key the binary was built with signed the statement:

<!-- runbook-test: rotation -->
```bash
( set -euo pipefail
  NEW=<new release key id>
  /usr/local/bin/blockula release pin --key-id $NEW --rotation rotation.txt --rotation-sig rotation.txt.sig
)
```

- Signature format: the message is `"BLOCKULA_RELEASE_V1"` followed by the exact bytes of `checksums.txt`, signed with FIPS 204 ML-DSA-65 (empty context, deterministic, as `crypto::Keys::sign`). `checksums.txt.sig` is one JSON line: `{"format":"blockula-release-signature-v1","key_id","public_key","signature"}` (hex). `checksums.txt`'s first line, `# blockula-release vX.Y.Z`, is part of the signed bytes; `verify --tag vX.Y.Z` checks it. Rotation statements and weekly checkpoints are signed the same way; their first lines differ, so none of them can pass for another. `blockula release` (`node/src/release_cli.rs`) signs and verifies; `scripts/release-sig.mjs` only verifies (it never loads a key). Both check the same known-answer vector (`blockula release selftest`, `node scripts/release-sig.mjs selftest`), so their signatures are byte-identical.

### Installing and upgrading

`blockula release install vX.Y.Z` installs a release with the `blockula` you already trust. It downloads `checksums.txt` and its signature (from the GitHub release, or `--from` a URL or directory), checks the signature against the pinned release key id, checks that the first line names vX.Y.Z, downloads this machine's archive and checks its SHA-256 against the signed list, with `--provenance` also asks `gh attestation verify` that the release workflow built it from the tag, extracts only the binary, checks that it reports vX.Y.Z, and installs it atomically (`--to`, default `~/.blockula/bin/blockula`). It refuses at the first check that fails, and it never downloads or runs the release's `install.sh`. `blockula release fetch vX.Y.Z --file <name> --dir <new dir>` does the same for other files of a release (the genesis file): only files that match land in the directory.

#### Upgrading

The `blockula` you already run checks the signature (against the pinned key id), the tag line, the archive's hash, provenance from the tag and the new binary's version:

<!-- runbook-test: upgrade -->
```bash
( set -euo pipefail
  V=vX.Y.Z
  /usr/local/bin/blockula release install $V --provenance --to ~/.blockula/bin/blockula
  sudo install -m 755 ~/.blockula/bin/blockula /usr/local/bin/blockula
)
```

Then restart the service in your maintenance window ([Upgrades](#upgrades)). A desktop install is the first line alone (`blockula release install vX.Y.Z`, `--provenance` with a logged-in `gh`).

#### A first install

A first install has no `blockula` to check the signature with, so something else has to vouch for the first binary. Here it's the release announcement ([How a release is made](#how-a-release-is-made), step 6): the archive must hash to the SHA-256 the signer announced in the operators' channel, a channel GitHub doesn't control. Provenance is checked too, but on its own it's what a compromised GitHub side can produce. Only then does anything from the release run: the binary it holds checks the release's signature with `K`, pins `K` and installs itself. (On macOS, set `T` to `aarch64-apple-darwin` or `x86_64-apple-darwin`.)

<!-- runbook-test: first-install -->
```bash
( set -euo pipefail
  V=vX.Y.Z
  K=<release key id>
  T=$(uname -m)-unknown-linux-gnu
  H=<SHA-256 of blockula-$T.tar.gz from the release announcement in the operators' channel>
  gh release download $V --repo jackdishman/blockula --dir first-$V --pattern "blockula-$T.tar.gz"
  cd first-$V
  echo "$H  blockula-$T.tar.gz" | sha256sum --strict -c -
  gh attestation verify blockula-$T.tar.gz --repo jackdishman/blockula \
    --signer-workflow jackdishman/blockula/.github/workflows/release.yml --source-ref refs/tags/$V
  tar -xzf blockula-$T.tar.gz blockula-$T/blockula
  ./blockula-$T/blockula release install $V --key-id $K --provenance --to ~/.blockula/bin/blockula
  sudo install -m 755 ~/.blockula/bin/blockula /usr/local/bin/blockula
)
```

`K` comes from SECURITY.md, this file, the website and the operators' channel, and they must agree; never from the release. After the key ceremony the binary also knows the release key id it was built with, and refuses a `K` that isn't that one ("this blockula was built with release key …, and you gave …"): then either your `K` or the archive isn't genuine, so stop and check both on the operators' channel. A key that replaced the built-in one after the binary was built comes in only through the rotation statement the built-in key signed ([The release key](#the-release-key)). `--strict` makes `sha256sum` refuse a malformed `H` (macOS's `sha256sum` otherwise accepts one; without it, use `shasum -a 256 -c -`). If you already run a verified `blockula` on another host of the same platform, copy it over instead, check its SHA-256 against the one you noted when you installed it, and run the upgrade block with it (`./blockula release install $V --key-id $K --provenance --to …`): then that binary is the trust root. Before the key ceremony there's no key id, and `release install` refuses: testnet installs use the one-line install below.

**The trust chain.** The first binary is trusted because of the announcement (or the binary you copied). Every later binary is trusted because the one before checked its signature against the pinned key. The signature is trusted for exactly what the signer checked ([What the signature vouches for](#how-a-release-is-made)). `gh attestation verify` proves only that GitHub's release workflow produced a file: a compromised workflow attests whatever it builds, so it's an extra check, never the trust root. Rebuilding (`COMMIT=<commit> scripts/reproduce-release.sh vX.Y.Z`) is the strongest check, and it's recommended for validators.

#### To an offline machine

An offline machine (the release signing machine, a genesis key machine) gets its `blockula` from an online machine that already runs a verified one with the release key id pinned (above; on a new machine, [a first install](#a-first-install) first). There, `blockula release fetch` downloads the archive for the offline machine's platform, checks it against the signed `checksums.txt` (the signature against the pinned key, the tag line, the archive's SHA-256) and leaves a new directory holding only what matched: the archive, `checksums.txt` and `checksums.txt.sig`. `T` is the offline machine's target (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` or `x86_64-apple-darwin`), whatever the online machine runs. The last line prints the archive's signed SHA-256: write it down, on paper or in the release or ceremony issue.

<!-- runbook-test: offline-fetch -->
```bash
( set -euo pipefail
  V=vX.Y.Z
  T=<the offline machine's target>
  /usr/local/bin/blockula release fetch $V --file blockula-$T.tar.gz --dir offline-$V
  grep -x "[0-9a-f]\{64\}  blockula-$T.tar.gz" offline-$V/checksums.txt
)
```

Copy `offline-$V` over on a fresh USB stick. On the offline machine, before anything from it runs, check the archive against the SHA-256 you wrote down (not against anything on the stick, which could have been changed on the way), then install its binary:

<!-- runbook-test: offline-unpack -->
```bash
( set -euo pipefail
  V=vX.Y.Z
  T=<the offline machine's target>
  H=<the SHA-256 you wrote down>
  cd offline-$V
  echo "$H  blockula-$T.tar.gz" | sha256sum --strict -c -
  tar -xzf blockula-$T.tar.gz blockula-$T/blockula
  sudo install -m 755 blockula-$T/blockula /usr/local/bin/blockula
  /usr/local/bin/blockula --version
)
```

(On a macOS machine without `sha256sum`, `shasum -a 256 -c -` does the same.) If the offline machine has the platform of a host you just did a first install on, the `first-$V/blockula-$T.tar.gz` that block checked against the announced SHA-256 can be carried instead, and checked offline against the announced SHA-256 the same way. Carry later binaries over only this way, and note each one's SHA-256 in the release or ceremony issue.

#### The one-line install

`curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh` (README) is for developers. It runs the release's own `install.sh`, so whoever controls the release controls the script and every check it makes: its checks protect against a swapped or corrupted download, not against OPS-7's adversary. Validators don't use it. What it checks:
- **The version** is bound to every check: `latest` is resolved to its tag first (a mirror, `BLOCKULA_RELEASE_URL`, needs `BLOCKULA_VERSION`), `checksums.txt` must name it on its first line, provenance must come from its tag, and the binary's `--version` must match before it's installed. An older, genuinely signed release served as the one you asked for is refused (audit supply-chain-B-2). Releases before v0.5.0 name no tag: install those with their own `install.sh`.
- **The checksum**, always. **Provenance**, when a logged-in `gh` is available. **The signature**, when an installed `blockula` can verify it.
- Once a release key id is known (`BLOCKULA_RELEASE_KEY_ID`, or built into the script after the key ceremony; if both are set they must agree), a release without `checksums.txt.sig` is refused: provenance alone is all a compromised GitHub account or release workflow can produce, and it can't forge the offline signature (audit F49; the MCP's `install_node` does the same, F69). With no installed verifier the signature can't be checked at all, so the script refuses if you gave `BLOCKULA_RELEASE_KEY_ID` or `BLOCKULA_REQUIRE_VERIFY=1`; with only the built-in key id (a developer's first install) it installs with a loud warning.
- It prints which checks ran and which it skipped. If neither provenance nor the signature ran, it prints a loud warning. `BLOCKULA_REQUIRE_VERIFY=1` refuses unless provenance was checked and, once a key id is known, the signature too.

`scripts/test-install.sh` tests this policy offline (CI, `ops` job), including an older signed release replayed as a newer one. `scripts/test-release-runbook.sh` (CI, `node` job) runs every block marked for it in this file as an operator pastes it, with the real `blockula` and stand-ins for GitHub and `sudo`: the genuine release installs, and an attacker's release (re-signed with another key, a swapped archive, an older release, an unsigned or unattested one, an announcement that doesn't match) is refused with nothing from it run and nothing installed. It also runs each block with its `( set -euo pipefail … )` taken off and checks that it then fails open, so a block that doesn't stop on failure can't pass. `scripts/test-release-files.sh` (CI, `ops`) tests the signer's file check.

### npm packages

`@blockula/sdk` and `@blockula/mcp` are published by `.github/workflows/npm-publish.yml`: manual only, from a release tag, a dry run unless you untick it, SDK first and then the MCP (which depends on it). It uses npm trusted publishing (OIDC), so no npm token exists anywhere, and every version carries a provenance attestation that links it to the workflow run and commit.

The OIDC credential would let any code running beside it publish a backdoored package with valid provenance, so it never meets third-party code: a `build` job without `id-token` installs the lockfile with `npm ci --ignore-scripts`, builds, and packs both tarballs (their SHA-256s go to the next job as an output); the `publish` job, the only one with `id-token: write` and the only one behind the `npm` environment's approval, installs and builds nothing: it checks the tarballs against those checksums and runs `npm publish <tarball> --provenance --ignore-scripts`. Keep it that way: never add `npm ci`, a build step or a third-party action to `publish`.

The MCP tarball carries `npm-shrinkwrap.json` (audit F75), so `npx -y @blockula/mcp@<version>` installs exactly the tree CI tested. The `build` job packs the SDK first and writes the shrinkwrap from the reviewed lockfile (`scripts/mcp-shrinkwrap.mjs --check`), with every entry pinned by its registry integrity and the SDK entry by the integrity of the SDK tarball it just packed; it refuses an MCP tarball without the file. Before publishing the MCP, `publish` checks that the SDK on npm has that integrity (it's the same tarball), so the shrinkwrap can't point at another SDK.

One-time setup by the maintainer:
1. npmjs.com → each package → Settings → **Trusted publishing** → GitHub Actions: owner `jackdishman`, repository `blockula`, workflow `npm-publish.yml`, environment `npm`.
2. Each package → Settings → Publishing access → **Require two-factor authentication and disallow tokens**. Revoke any old automation tokens.
3. GitHub → Settings → Environments → `npm`: add the maintainers as required reviewers and limit deployments to `v*` tags.

To publish: bump both `package.json` versions to the tag's version in the release PR, tag, and wait until the GitHub release for that tag is published (not a draft: the MCP's `install_node` installs that release, and refuses releases before v0.5.0, whose `checksums.txt` names no tag). Then Actions → "Publish npm packages" → Use workflow from: the tag → Run (dry run first). Users verify with `npm audit signatures`.

### Repository settings (GitHub)

The checks a PR must pass, who may push release tags and where fuzz crashes go are GitHub settings, which change without a commit. The intended ones are in [`deploy/github`](../deploy/github/README.md): `ruleset-main.json` (a code-owner-approved PR, squash merges, and every CI job that runs on pull requests as a required check, tied to the GitHub Actions app), `ruleset-tags.json` (`v*` tags admin-only) and the variable `FUZZ_CRASH_AGE_RECIPIENTS` (the maintainers' age keys, see [node/fuzz](../node/fuzz/README.md)). As the admin, apply them with `deploy/github/apply-rulesets.sh` (a dry-run diff first, then `--apply`); CI can't.

Required checks are job names. `.github/workflows/ci.yml`'s header lists them, and `scripts/check-required-checks.mjs` (CI, `ops`) fails when that list, `ruleset-main.json` and the workflows drift: a renamed job, a required job path-filtered on PRs, a new job nobody required (audit supply-chain-B-3). GitHub counts a skipped check as passing, so it also refuses a required job that could skip itself or pass without its work: a job-level `if:` other than `github.event_name != 'schedule'`, a `needs:` on a job that isn't required, `continue-on-error`, a matrix or a reusable workflow, or a step-level `if:` or `continue-on-error` that isn't one of the reviewed exceptions listed in the script. It reads the workflows with a YAML parser, as GitHub does, and refuses what it can't read the same way (audit review-r3-supply-1); no other job, whatever triggers it, may be able to report a required name ([deploy/github](../deploy/github/README.md#keeping-the-required-checks-in-step-audit-supply-chain-b-3)). Rename or add a required job in the same PR as the ruleset file, and apply the ruleset when it merges.

The live settings can still change without a commit. `.github/workflows/github-settings.yml` runs `deploy/github/apply-rulesets.sh --check` every day with a read-only token and fails when a live ruleset differs from its file (a removed check, a weakened review rule, a ruleset switched off or deleted). GitHub doesn't show bypass actors to a read-only token, so those aren't compared; the maintainer's own dry run does compare them. The rulesets were applied on 2026-10-17 (`apply-rulesets.sh --check`: both match).

With one maintainer, the admin bypass (pull-request mode) is how every PR merges, since nobody approves their own. When a second maintainer joins, make the bypass an emergency path and require a second person's review for `.github/**`, `scripts/**`, `deploy/**`, `deny.toml`, `rust-toolchain.toml` and the lockfiles ([deploy/github/README.md](../deploy/github/README.md#the-admin-bypass)).

## Monitoring

Every operator runs the reference stack in [`deploy/monitoring`](../deploy/monitoring/README.md): Prometheus scraping the node's `/metrics` and node_exporter, the alert rules (head age, finality lag, execution lag, certificates vs quorum, mempool, disk, memory, clock skew, restarts), Alertmanager, and a Grafana dashboard. Requirements:

- Alerts reach a human at any hour: a paging service or a phone, not only a chat channel.
- An external heartbeat watches the `Watchdog` alert, so a dead host or a dead monitor still pages someone.
- Each operator is in the on-call rotation for their own nodes. The core team runs a network-wide rotation (primary and secondary, weekly handover, escalation to the security lead after 15 minutes without acknowledgement for a SEV-1).
- The project runs a public status page fed by the public RPC nodes' health (`/api/health`).

Logs go to the journal (`journalctl -u blockula`). Keep at least 14 days (`SystemMaxUse=` in `journald.conf` sized for it); attach the relevant window to every incident.

## Checklists

**New validator operator**
- [ ] Hardware, bandwidth plan and disk meet this document; provider/ASN/jurisdiction declared and within the ¼ limit.
- [ ] Firewall: only p2p and admin SSH reachable from the internet (tested from outside).
- [ ] chrony with NTS sources; offset < 10 ms.
- [ ] Binary installed with `blockula release install` ([A first install](#a-first-install)), release key id pinned (`blockula release pin` shows it); genesis file from `blockula release fetch`, its hash checked.
- [ ] Consensus key and owner key backed up offline, encrypted, two places; restore tested.
- [ ] systemd unit with `BLOCKULA_INSTANCE_ID` and `RestartPreventExitStatus=10 11 12`; `GITHUB_TOKEN` set (public-read-only).
- [ ] Provider volume snapshots off on the validator volume.
- [ ] At least one sentry, registered as `http_addr`; validator HTTP private.
- [ ] Monitoring stack running; receivers and heartbeat tested with a test alert.
- [ ] Two contacts registered; in the operator channel and the on-call rotation.

**Quarterly**
- [ ] Restore drill from the offline key backup (testnet).
- [ ] Contact list re-confirmed with a test page.
- [ ] Egress against the bandwidth plan; disk growth against the forecast.
- [ ] Review alerts that fired: noisy ones tuned, missed ones added.
