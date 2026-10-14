# Operating Blockula

For validator operators, sentry and RPC operators, and the release and incident teams. It closes the documentation side of OPS-1, OPS-4, OPS-5, OPS-6, OPS-7 and SF-14 in [MAINNET.md](MAINNET.md), and follows the decisions in [MAINNET-DESIGN.md](MAINNET-DESIGN.md) (DEC-3, DEC-14, section 3.9).

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
10. [Releases](#releases) and [npm packages](#npm-packages)
11. [Monitoring](#monitoring)
12. [Checklists](#checklists)

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
- **No hosting provider, ASN or jurisdiction above ¼ of the active set** (8 of 32), always below ⅓ (DEC-14). Operators declare provider, ASN and jurisdiction at registration and whenever they move; the foundation publishes the distribution monthly and asks operators in an over-represented provider, ASN or jurisdiction to move. Count by the ASN that actually routes the validator's IP (`whois -h whois.radb.net <ip>` or `curl ipinfo.io/<ip>/org`), not the reseller's brand.
- **Foundation nodes stay small.** The founder runs **at most 3 validators**, on Fly.io: `performance-8x` machines (8 dedicated vCPUs, 32 GB; `performance-4x` with 16 GB is the floor), each with a dedicated IPv4 for p2p and a volume sized per [Disk sizing](#disk-sizing). Fly volumes top out at 500 GB at the time of writing: plenty for an empty chain for years, not for a sustained flood, so foundation validators must run with epoch-journal pruning (planned: P-UPGRADE) and watch `HostDiskFillsWithinAWeek`. The public RPC nodes and the foundation's sentries also run on Fly. The foundation's share of the set shrinks as independent operators join.
- **Recommended providers, mixed.** Bare metal and dedicated servers give unmetered or generously included bandwidth and predictable disks:
  - Latitude.sh, OVHcloud, Cherry Servers, Hetzner (dedicated). **Hetzner:** its terms have restricted crypto workloads before (mining is banned, and some blockchain node operators have been asked to leave); every operator must check the current terms and get written confirmation before running a validator there.
  - Regional and colocation providers spread ASNs and jurisdictions better than any big brand. They are welcome.
  - AWS and GCP work, but **internet egress is billed per GB**: ~12 TB/month of certificates alone costs on the order of $1,000 per validator per month at list prices. Use them only with committed-use egress pricing, or for sentries in front of a bare-metal validator.

## Reference hardware

| | Minimum | Recommended | Why |
|---|---|---|---|
| CPU | 4 dedicated cores, x86-64 (AVX2) or arm64 | 8 cores / 16 threads, recent EPYC, Xeon or Ryzen | Signatures are cheap: ML-DSA-65 verifies in ~25–50 µs on a modern core, so a full block (~400 txs) plus a view's votes and certificates is 10–25 ms of one core. Cores buy headroom for the rest: today's node re-executes history from genesis on restart (until CV-1), verifiers will replay up to 64 unexecuted ancestors (CV-5, planned), p2p encryption runs over ~10 MB/s, and sentries pull sync batches. Execution is sequential, so single-thread speed matters more than core count. No shared or burstable vCPUs. |
| Memory | 16 GB | 32 GB | State is in memory until CV-1 (accounts, names, Hunts, claims, indexes), plus marshal's caches and a 5,000-tx mempool. |
| Disk | 1 TB NVMe SSD | 2 TB NVMe SSD, expandable online | The node fsyncs its block logs and consensus journals on every block (and, with CV-1, commits state before acknowledging consensus): low-latency local NVMe, not network storage with burst credits. Size: [Disk sizing](#disk-sizing). |
| Network | 1 Gbit/s, ≥ 15 TB/month included | 1 Gbit/s unmetered (or 10 Gbit/s) | [Bandwidth](#bandwidth). |
| Other | Dedicated public IPv4 | Plus IPv6 | p2p is raw TCP; NAT and shared IPs break dialing. |

Sentries and RPC nodes: 4 cores, 16 GB, 1 TB NVMe, and the same bandwidth (each sentry costs every validator it peers with ~150 KB/s of certificates at n = 32). The monitoring host: 2 cores, 4 GB, 50 GB.

### Bandwidth

Certificates dominate. Simplex sends every vote and certificate to every connected peer, and each node re-broadcasts each certificate it holds once (`.context/research/commonware.md` §6, §7):

- **Certificates:** a ⅔ certificate at n = 32 is ~73 KB (22 signatures × 3,309 B). Two per view (notarization, finalization) to 31 peers at one view per second: **~4.5 MB/s of egress per validator, ~12 TB/month**. At n = 64 it would be ~18 MB/s, which is why the cap is 32.
- **Votes:** ~3.3 KB each, two per view, to 31 peers: ~0.2 MB/s.
- **Blocks:** the leader sends its block to every peer. A full 4 MiB block to 31 peers is ~130 MB in about a second, once every 32 views on average: provision for 1 Gbit/s bursts.
- **Sentries:** each secondary peer adds ~150 KB/s of certificates per validator.

Budget **≥ 15 TB/month of egress per validator, unmetered or included in the price**. The dashboard's Network panel shows the real number; compare it with your plan monthly.

### Disk sizing

What a node stores per block today: ~330 B in `blocks.jsonl` and ~170 B in `cblocks.bin` for an empty block, plus a finalization certificate on disk every 100 heights and at each epoch's end (MAINNET.md appendix). A certificate holds a quorum of 3,309-byte signatures. Marshal keeps only the last 2,000 blocks and certificates (`MARSHAL_RETAIN`).

| Active validators n | Quorum | Certificate | Empty chain per block | Per day | Per year |
|---|---|---|---|---|---|
| 4 | 3 | ~10 KB | ~0.60 KB | ~52 MB | ~19 GB |
| 8 | 6 | ~20 KB | ~0.70 KB | ~60 MB | ~22 GB |
| 16 | 11 | ~36 KB | ~0.86 KB | ~75 MB | ~27 GB |
| 21 | 15 | ~50 KB | ~1.0 KB | ~86 MB | ~31 GB |
| 32 | 22 | ~73 KB | ~1.2 KB | ~106 MB | ~39 GB |

Transactions add far more than an idle chain. A transfer is stored twice today (hex JSON in `blocks.jsonl`, ~10.8 KB; wire bytes in `cblocks.bin`, ~5.4 KB), so **~16 KB per tx**:

| Sustained load | Per day | Per year |
|---|---|---|
| 1 tx/s | ~1.4 GB | ~0.5 TB |
| 10 tx/s | ~14 GB | ~5 TB |
| Every block full (~388 tx/s) | ~0.5 TB | n/a: a flood, not a load |

A block-filling flood costs ~2,200 FANG a day at today's minimum fee (MAINNET.md appendix) until the fee market (TOK-2) prices it. Old epochs' simplex journals are also kept until pruning lands (planned: P-UPGRADE, OPS-6). So:

- **Validators at genesis: 2 TB NVMe** (1 TB minimum), on a volume you can grow without downtime. `HostDiskAlmostFull` (80%) and `HostDiskFillsWithinAWeek` give you time to grow it.
- Validators don't need history once state lives in the store and snapshots are certified (CV-1, CRY-2). Archival RPC nodes keep everything and need the most disk.

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

**Protocol v1 (planned: SF-10, P-STAKE/P-SYNC):** operators register up to a few sentry keys and addresses on their validator record. Validators track registered sentries as *secondary* p2p peers: inbound-only, never dialed, receiving every vote and certificate. Sentries don't run simplex; they run marshal plus a passive certificate listener and backfill through the resolver. Validators then stop exposing HTTP entirely. The on-chain sentry list is capped (each sentry costs every validator ~150 KB/s, and an oversized peer set panics the p2p layer), so register only what you run.

## Running a validator

On a fresh Debian 12 / Ubuntu 24.04 host that meets the [hardware](#reference-hardware), [firewall](#ports-and-firewall) and [time](#time) requirements:

```bash
# 1. A service account and a home on the data volume.
sudo useradd --system --home /var/lib/blockula --shell /usr/sbin/nologin blockula
sudo install -d -o blockula -g blockula -m 700 /var/lib/blockula

# 2. Install a verified binary (see Releases). As your own user, with `gh auth login` done:
#    REQUIRE_VERIFY refuses anything that isn't proven by provenance or the release signature.
curl -fsSLO https://github.com/jackdishman/blockula/releases/latest/download/install.sh
gh attestation verify install.sh --repo jackdishman/blockula
BLOCKULA_REQUIRE_VERIFY=1 sh install.sh
sudo install -m 755 ~/.blockula/bin/blockula /usr/local/bin/blockula

# 3. Create the validator home (generates the consensus key). Use the genesis file from the
#    signed release (genesis-<chain id>.json, verified with the archives), not a node's copy.
sudo -u blockula blockula validator init --home /var/lib/blockula/val \
  --genesis ./genesis-blockula-1.json \
  --public-p2p-addr <your-ip>:30300 --public-http-addr https://<your sentry>
```

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

The node guards against double-signing in two ways today: it refuses to start a home that has executed blocks but no consensus journal (a restored or copied home), and with `BLOCKULA_INSTANCE_ID` set it refuses a home last used by a different machine (`machine.lock`). `--i-know-this-key-is-not-running-elsewhere` overrides both. Use it only after you have *proven* the other copy is gone.

Then install [monitoring](#monitoring) and join the on-call rotation and the private operator channel ([Incident response](#incident-response)).

## Keys, backups and restores

| Key | Where it lives | Backup |
|---|---|---|
| **Consensus key** (`validator.key`, a hex seed) | On the validator only (mode 600). Holds only a fee float once the fee payer is separate (OPS-1) | Offline, encrypted, two locations |
| **Owner account** (bond, rewards, commission) | Cold: an offline machine | Offline, encrypted, two locations |
| **Slashing-protection record** (last view signed per epoch; planned: P-UPGRADE) | Next to the consensus key | Never restored from backup; see below |
| **Release key** (maintainers) | Offline machine | See [Releases](#releases) |

Encrypt key backups with a passphrase you keep separately, for example with [age](https://age-encryption.org): `age -p -o validator.key.age validator.key`. Store copies in two places you control (a safe, a bank box), never on the validator, in a cloud drive, or in the same place as the passphrase. Test that you can decrypt them at every quarterly drill.

**Never restore a volume snapshot onto a running validator, and never run two copies of a key.** Two copies sign conflicting votes in the same view, and in protocol v1 that's slashed (5% of the pool) and tombstoned for good (MAINNET-DESIGN 3.4). Chain data is replaceable from peers; keys and the slashing-protection record aren't. So back up keys, not the chain.

**Restoring after losing a validator host:**

1. **Make sure the old host can't sign.** Power it off from the provider's console and detach its volume, or revoke its network. If you can't prove it's down, stop here: wait for the next epoch boundary, then for the operator channel to confirm the old key went silent.
2. Provision a new host (hardware, firewall, NTP, verified binary).
3. Restore `validator.key` from the offline backup into a new home with the published `genesis.json`.
4. **Slashing protection** (planned: P-UPGRADE): copy the record from the old disk if you still have it. If it's lost, the node must not sign in the current epoch: it starts with an empty record that refuses views below the network's current one, and you wait for the next epoch boundary before signing. Never restore an older copy of the record.
5. Sync state from peers (fast sync from a certified snapshot), not from a chain-data backup.
6. Start the service. The double-sign guard will refuse the copied home on a new machine. Pass `--i-know-this-key-is-not-running-elsewhere` once, for this start only, after step 1.

**Quarterly restore drill** (OPS-6): on a testnet, restore a validator from its offline key backup onto a fresh host following the steps above, time it, and record the result in the operator log.

## Upgrades

Upgrades are announced, released, signalled and activated at a height. Nobody restarts everything at once.

1. **Announce** at least two weeks ahead for planned upgrades: version, what changes, whether it's consensus-breaking, the proposed activation height, and the release date. Post in the operator channel and publicly.
2. **Release** a signed, reproducible build ([Releases](#releases)). Operators verify it themselves: provenance, the ML-DSA signature, and ideally a rebuild.
3. **Install and signal.** Each operator restarts onto the new binary in their own maintenance window, one node at a time. The new binary runs the old rules until the activation height. Blocks carry `signal`, the newest protocol version the proposer's binary supports (MAINNET-DESIGN 3.2), so readiness is visible on chain and in the explorer. Wait for ≥ ⅔ of the active set to signal.
4. **Activate.** A governance `ScheduleUpgrade` proposal (planned: P-GOV, P-UPGRADE) puts `(version, activation_height)` in state. At that height every upgraded node switches rules. A node whose binary doesn't support the scheduled version halts cleanly before executing that height, with a non-zero exit and the reason in `/api/health`, instead of diverging. Upgrade it and it continues.

Rolling restarts: keep fewer than ⌊(n−1)/3⌋ validators restarting at any moment across the whole network, so coordinate windows in the operator channel. Restarting your own validator takes it out for the replay (minutes today; seconds once CV-1 lands).

**Coordinated stop at a height** (incidents, emergency patches): `blockula run … --halt-height H` executes `H` and stops (planned: P-UPGRADE). Until that flag exists, watch `curl -s 127.0.0.1:7070/api/status | jq .height` and stop the service once it passes `H`.

Today's testnet binary has no version gating: the testnet is upgraded by redeploying all validators (`deploy/launch.sh`). That's acceptable only because the testnet holds no value.

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
   - Write the fix so it changes behaviour **only from the failing height on** (height-gated), so history replays identically.
   - Ship it as an emergency release ([Patch distribution](#patch-distribution)), verified like any release.
   - Operators install it and start. The chain resumes when ≥ ⅔ of the set runs it.

**Safety failure (SEV-1):** two certificates for different blocks at one height, or nodes reporting different state roots for the same height. Ask every operator to stop their validator at the current height (`--halt-height`, or stop the service), preserve disks and logs, and don't restart until the IC calls it. Equivocation evidence from the consensus journals (`track_historical_votes`) identifies the signers (planned: P-STAKE).

**Key compromise:** the operator stops the validator at once. In protocol v1 the stake belongs to the cold owner account, so a stolen consensus key can't move it, but it can still equivocate and get the pool slashed. Protocol v1 has no consensus-key rotation: the owner undelegates the self-bond, registers a new validator with a fresh key generated offline, and asks delegators to redelegate (P-STAKE). Report it in the operator channel so others watch for votes from the old key.

**Vulnerability reported privately:** the security lead triages within the SECURITY.md targets, decides severity with the IC, and runs the private patch flow below. Credit and bounty per [BUG_BOUNTY.md](BUG_BOUNTY.md).

### Patch distribution

For a vulnerability that's exploitable before a fix is widely deployed:

1. Fix and review in a private fork (a GitHub private security advisory fork). Add a regression test.
2. Tag a release candidate in the private fork. Build it reproducibly (`scripts/release-build.sh`), and have at least one more maintainer rebuild it and match the hashes.
3. Sign `checksums.txt` with the release key ([Releases](#releases)) and send the archives, `checksums.txt`, its signature and the source commit to operators through the private channel. Don't send the diff until enough of the set has upgraded, if the diff itself reveals the bug.
4. Operators verify the signature against the published release key id, install and confirm in the channel. Track it in a checklist: ⅔ of the set is the target, everyone is the goal.
5. When ≥ ⅔ have upgraded (or the activation height passes), publish the source, the public release (normal pipeline, same hashes) and the advisory.

### Coordinated restart at a height

1. The IC picks a halt height `H` (at least 30 minutes out, so everyone can act) and announces it with the reason.
2. Each operator restarts with `--halt-height H` (or stops the service after `H` by hand) and confirms in the channel.
3. Once every validator has stopped at `H` (same height and block hash on all nodes), operators install the new binary and start it. The chain resumes when ≥ ⅔ are back.
4. The IC confirms finality is advancing and closes the step.

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

### Disk

Above 80%: grow the volume now (online resize: grow the block device, then `resize2fs` or `xfs_growfs`). Find what grew: `du -sh /var/lib/blockula/val/*`. Old epochs' simplex journals under `consensus/` are kept until pruning lands (planned: P-UPGRADE); don't delete files by hand from a running validator's home. Never let a validator's disk fill: a failed fsync stops the node.

### Memory

Less than 10% available or an OOM kill. State is in memory until CV-1, so memory grows with the chain. Check `ps -o rss,cmd -u blockula`; move to the recommended 32 GB. Don't run other services on a validator.

### Alerting pipeline

The alerting pipeline itself: `Watchdog` should always reach your heartbeat service; `AlertmanagerNotificationsFailing` means a receiver is misconfigured or down (check `docker compose logs alertmanager`); `PrometheusRuleFailures` means a rule broke (check Prometheus's /rules page). See [deploy/monitoring/README.md](../deploy/monitoring/README.md).

## Releases

Every release is reproducible, carries SLSA build provenance, and is signed offline with the ML-DSA-65 release key before it's published (OPS-7).

### How a release is made

1. A maintainer tags `vX.Y.Z` on `main` and pushes the tag. `.github/workflows/release.yml`:
   - builds the four targets with `scripts/release-build.sh`: the toolchain pinned in `rust-toolchain.toml`, Linux in a digest-pinned `rust` image, `--locked`, `SOURCE_DATE_EPOCH` from the commit, build paths remapped, deterministic archives;
   - rebuilds the x86_64 Linux archive on a fresh runner from a different directory and fails unless it's byte-identical;
   - writes `checksums.txt` (archives and `install.sh`) and attests every file with GitHub artifact attestations (SLSA provenance, `actions/attest-build-provenance`);
   - creates a **draft** release. Only that last job can write to the repo.
2. **Reproduce.** At least two people rebuild the Linux archives independently and compare against the draft:

   ```bash
   gh release download vX.Y.Z --repo jackdishman/blockula --pattern checksums.txt --dir /tmp/rel
   CHECKSUMS=/tmp/rel/checksums.txt scripts/reproduce-release.sh vX.Y.Z                            # this machine's arch
   CHECKSUMS=/tmp/rel/checksums.txt scripts/reproduce-release.sh vX.Y.Z aarch64-unknown-linux-gnu  # emulated on x86: slow
   ```

   Each builder posts the result (and their hashes) in the release issue. macOS archives reproduce only with the same Xcode as the release run (its log prints `xcodebuild -version`).
3. **Check provenance** of what you'll sign: `gh attestation verify /tmp/rel/checksums.txt --repo jackdishman/blockula --signer-workflow jackdishman/blockula/.github/workflows/release.yml`.
4. **Sign offline.** Carry `checksums.txt` (only that file) to the offline signing machine, which holds the release key and a checkout with `npm ci` already run:

   ```bash
   node scripts/release-sig.mjs sign --key release-key.json checksums.txt > checksums.txt.sig
   node scripts/release-sig.mjs verify --key-id <release key id> --sig checksums.txt.sig checksums.txt
   ```

   Carry `checksums.txt.sig` back.
5. **Publish.** `gh release upload vX.Y.Z checksums.txt.sig --repo jackdishman/blockula`, check the draft once more, then `gh release edit vX.Y.Z --draft=false --repo jackdishman/blockula`. With GitHub's immutable releases enabled (repository settings), nobody can swap an asset after that.

### The release key

- One ML-DSA-65 key, generated on an offline machine with a verified `blockula`: `blockula keygen > release-key.json`. Its **key id** is the hex BLAKE3 of the public key: `node scripts/release-sig.mjs keyid --key release-key.json`.
- Publish the key id in SECURITY.md, this file, `scripts/install.sh` (`RELEASE_KEY_ID`), the website and the project's social accounts, so a single compromised channel can't swap it. **Release key id:** *not generated yet* (the maintainer fills this in at the key ceremony).
- Back it up like a validator key (encrypted, offline, two locations). Rotation: sign a statement naming the new key id with the old key, publish it through the same channels, and update every place above.
- Signature format: the message is `"BLOCKULA_RELEASE_V1"` followed by the exact bytes of `checksums.txt`, signed with FIPS 204 ML-DSA-65 (empty context, deterministic, as `crypto::Keys::sign`). `checksums.txt.sig` is one JSON line: `{"format":"blockula-release-signature-v1","key_id","public_key","signature"}` (hex). `node scripts/release-sig.mjs selftest` checks the implementation against a known-answer vector that the node's own `ml-dsa` crate reproduces.
- A built-in `blockula release sign|verify` with the same format is planned (it's what `install.sh` calls to check the signature with an already-installed binary).

### Verifying a release as a user or operator

```bash
V=vX.Y.Z; T=x86_64-unknown-linux-gnu
gh release download $V --repo jackdishman/blockula --pattern "blockula-$T.tar.gz" --pattern 'checksums.txt*'
# 1. Provenance: built by this repo's release workflow from the tag.
gh attestation verify blockula-$T.tar.gz --repo jackdishman/blockula \
  --signer-workflow jackdishman/blockula/.github/workflows/release.yml --source-ref refs/tags/$V
# 2. The offline signature (from a checkout, after npm ci), with the published key id.
node scripts/release-sig.mjs verify --key-id <release key id> --sig checksums.txt.sig checksums.txt
# 3. The archive matches the signed checksums.
sha256sum --ignore-missing -c checksums.txt
# 4. Optional, and recommended for validators: rebuild it.
scripts/reproduce-release.sh $V
```

`scripts/install.sh` does a subset automatically: the SHA-256 check always, provenance when a logged-in `gh` is available, and the signature when an installed `blockula` can verify it. If neither of the last two ran, it prints a loud warning, and `BLOCKULA_REQUIRE_VERIFY=1` turns that into a refusal. Validators should always install with `BLOCKULA_REQUIRE_VERIFY=1`.

### npm packages

`@blockula/sdk` and `@blockula/mcp` are published by `.github/workflows/npm-publish.yml`: manual only, from a release tag, a dry run unless you untick it, SDK first and then the MCP (which depends on it). It uses npm trusted publishing (OIDC), so no npm token exists anywhere, and every version carries a provenance attestation that links it to the workflow run and commit.

One-time setup by the maintainer:
1. npmjs.com → each package → Settings → **Trusted publishing** → GitHub Actions: owner `jackdishman`, repository `blockula`, workflow `npm-publish.yml`, environment `npm`.
2. Each package → Settings → Publishing access → **Require two-factor authentication and disallow tokens**. Revoke any old automation tokens.
3. GitHub → Settings → Environments → `npm`: add the maintainers as required reviewers and limit deployments to `v*` tags.

To publish: bump both `package.json` versions to the tag's version in the release PR, tag, then Actions → "Publish npm packages" → Use workflow from: the tag → Run (dry run first). Users verify with `npm audit signatures`.

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
- [ ] Binary installed with `BLOCKULA_REQUIRE_VERIFY=1`; genesis hash checked.
- [ ] Consensus key and owner key backed up offline, encrypted, two places; restore tested.
- [ ] systemd unit with `BLOCKULA_INSTANCE_ID`; `GITHUB_TOKEN` set (public-read-only).
- [ ] At least one sentry, registered as `http_addr`; validator HTTP private.
- [ ] Monitoring stack running; receivers and heartbeat tested with a test alert.
- [ ] Two contacts registered; in the operator channel and the on-call rotation.

**Quarterly**
- [ ] Restore drill from the offline key backup (testnet).
- [ ] Contact list re-confirmed with a test page.
- [ ] Egress against the bandwidth plan; disk growth against the forecast.
- [ ] Review alerts that fired: noisy ones tuned, missed ones added.
