# Monitoring a Blockula node

A reference stack every operator runs (OPS-4): Prometheus scrapes the node's `/metrics` and node_exporter, evaluates the alert rules, and Alertmanager pages you. Grafana shows one dashboard. The runbooks behind each alert are in [docs/OPERATIONS.md](../../docs/OPERATIONS.md#monitoring).

| File | What |
|---|---|
| `docker-compose.yml` | Prometheus, Alertmanager, Grafana, node_exporter. Images pinned by digest. Host networking, everything on `127.0.0.1`. |
| `prometheus/prometheus.yml` | Scrape targets: the node (5 s), node_exporter, Prometheus and Alertmanager themselves. |
| `prometheus/alerts.yml` | Alert rules, with thresholds taken from the protocol constants. |
| `prometheus/alerts.test.yml` | Unit tests for the rules (`promtool test rules`). |
| `alertmanager/alertmanager.yml` | Routing, inhibition, and two receivers you must fill in. |
| `grafana/` | Data source and the "Blockula node" dashboard, provisioned read-only. |
| `check.sh` | Validates all of the above with the pinned images. CI runs it. |

## Run it

On the validator host (Linux, Docker with the compose plugin):

```bash
cd deploy/monitoring
echo "GRAFANA_ADMIN_PASSWORD=$(openssl rand -hex 16)" > .env   # git-ignored
$EDITOR prometheus/prometheus.yml        # operator name, node labels, targets
$EDITOR alertmanager/alertmanager.yml    # your pager/chat receiver and heartbeat URL
./check.sh                               # needs docker; validates everything
docker compose up -d
```

Then, from your laptop: `ssh -N -L 3000:127.0.0.1:3000 -L 9090:127.0.0.1:9090 -L 9093:127.0.0.1:9093 you@validator` and open http://localhost:3000 (Grafana, user `admin`), http://localhost:9090/alerts and http://localhost:9093.

Nothing here listens on a public interface. Keep it that way: Grafana and Prometheus have no business on the internet, and a validator's attack surface should be its p2p port and SSH only (OPERATIONS.md "Ports and firewall").

**Before you rely on it:**
1. Replace both receivers in `alertmanager.yml`. Until you do, every notification fails, and `AlertmanagerNotificationsFailing` fires where nobody sees it. Send yourself a test alert: `docker compose exec alertmanager amtool alert add alertname=Test severity=warning --alertmanager.url=http://127.0.0.1:9093`.
2. Point the `deadman` receiver at an external heartbeat service (healthchecks.io, Dead Man's Snitch, Better Stack). `Watchdog` fires all the time; the service pages you when it stops arriving. That's the only way a monitor on the same host can tell you the host is gone.
3. Join the operator on-call rotation and the private operator channel (OPERATIONS.md "Incident response").

## Layouts

- **One host (default).** The stack runs next to the node and scrapes `127.0.0.1`. Simple, but it dies with the host, so the external heartbeat (above) is mandatory.
- **A monitoring host (recommended for more than one node).** Run this stack on a separate small machine. Put each node's private address in `prometheus.yml` (`10.x.x.x:7070` for a validator, `:7171` for a sentry) and node_exporter's on `:9100`, reachable only over a private link (WireGuard or the provider's VPC). Bind node_exporter on the nodes to that private address instead of `127.0.0.1`. Keep a heartbeat anyway.

Give every target a `node` label (a validator and its node_exporter share it): alerts, inhibition and the dashboard's Node selector all key on it.

## Metrics

From `node/src/api.rs` (`GET /metrics`):

| Metric | Type | Meaning |
|---|---|---|
| `blockula_height` | gauge | Executed height |
| `blockula_finalized_height` | gauge | Highest certified finalized height |
| `blockula_finalized_lag` | gauge | Finalized minus executed height. Past `MAX_EXEC_LAG` (16) a validator's proposals are invalid |
| `blockula_head_age_seconds` | gauge | Seconds since the head block's timestamp. `/api/health` fails past 10 blocks |
| `blockula_mempool_size` | gauge | Pending txs (cap `MAX_MEMPOOL` = 5,000) |
| `blockula_epoch` | gauge | Current epoch |
| `blockula_active_validators` | gauge | Validators in the active set |
| `blockula_validators_online` | gauge | Signers on the newest finalization certificate; `NaN` if none in the last 16 heights |
| `blockula_tx_submitted_total`, `blockula_tx_rejected_total` | counter | Txs accepted / rejected over HTTP |
| `blockula_faucet_drips_total` | counter | Faucet drips (test chains) |
| `blockula_build_info{version,git_sha}` | gauge | Always 1 |
| `blockula_process_start_time_seconds` | gauge | When the process started (unix seconds) |
| `blockula_protocol_version` | gauge | Protocol version in force at the next height |
| `blockula_supported_protocol_version` | gauge | Newest protocol version this binary runs (its blocks signal it) |
| `blockula_upgrade_next_version`, `blockula_upgrade_next_height` | gauge | The next scheduled upgrade (0 if none) |
| `blockula_halted`, `blockula_halt_info{kind}` | gauge | 1 while the node has stopped executing (`halt_height`, `upgrade_required`); `/api/health` gives the reason |
| `blockula_p2p_peers` | gauge | Validators: connected p2p peers |
| `blockula_slashing_protection_epoch`, `…_last_view` | gauge | Validators: the newest epoch and view the key signed in (the record next to `validator.key`) |
| `blockula_slashing_protection_voting` | gauge | Validators: 1 if in the current active set |
| `blockula_slashing_protection_abstaining` | gauge | Validators: 1 while watching the network after a start, before signing |
| `blockula_slashing_protection_refused_total` | counter | Validators: vote signatures refused (while watching, or conflicting with the record) |
| `blockula_beacon_commitments`, `blockula_beacon_reveals_owed`, `blockula_beacon_reveals` | gauge | The randomness beacon's rounds: commitments for the epoch after next; validators owing a reveal for the next epoch, and reveals so far (`/api/beacon`) |
| `blockula_beacon_last_reveals`, `blockula_beacon_last_missed` | gauge | Reveals in this epoch's beacon value (0: the hash chain alone), and committers that didn't reveal (jailed) |
| `blockula_beacon_own_missed` | gauge | Validators: 1 if this validator missed its reveal for this epoch's beacon |

A recording rule adds `blockula:quorum` = n − ⌊(n−1)/3⌋. Host metrics come from node_exporter; the clock alerts use its `timex` collector (`node_timex_offset_seconds`, `node_timex_sync_status`).

**Read `blockula_validators_online` as a lower bound.** Simplex stops collecting votes once a certificate has a quorum, so the newest certificate usually carries exactly a quorum of signers even when everyone is online. A halt shows up as `BlockulaChainHalted` (no fresh head anywhere), not as a dip here.

## Alerts

| Alert | Severity | Fires when |
|---|---|---|
| `BlockulaNodeDown` | critical | `/metrics` unreachable for 1 min |
| `BlockulaHeadStale` | warning | This node's head is older than 10 s for 1 min |
| `BlockulaChainHalted` | critical | Every scraped node's head is older than 60 s for 1 min |
| `BlockulaExecutionLagging` / `…PastMaxLag` | warning / critical | Execution > 8 / > 16 blocks behind finality |
| `BlockulaMempoolNearCap` | warning | Mempool above 4,000 of 5,000 for 5 min |
| `BlockulaTxRejectionsHigh` | info | More txs rejected than accepted, over 1/s, for 10 min |
| `BlockulaRestarted` | warning | The process started, or the height dropped, in the last 15 min |
| `BlockulaVersionMismatch` | info | Your nodes run different versions for over an hour |
| `BlockulaHalted` | critical | The node stopped executing (`--halt-height`, or a protocol version its binary doesn't run) for 1 min |
| `BlockulaUpgradeRequired` / `…Soon` | warning / critical | A protocol version this binary doesn't run is scheduled / activates within a day |
| `BlockulaFewPeers` | warning | Fewer p2p peers than quorum − 1 for 5 min |
| `BlockulaSlashingProtectionRolledBack` | critical | The slashing-protection record went backwards (a restored or copied home) |
| `BlockulaValidatorNotSigning` | warning | In the active set, the chain advancing, and no new vote for 10 min |
| `BlockulaWatchingAfterStart` | warning | Still watching the network after a start, 3 min on |
| `BlockulaValidatorJailed`, `BlockulaValidatorTombstoned` | critical | This node's validator (`own="1"`) is jailed, or was tombstoned for double-signing |
| `BlockulaBeaconRevealMissed` | warning | This node's validator committed to the randomness beacon and didn't reveal this epoch (it's jailed for it: check its beacon secrets file and fee payer) |
| `BlockulaValidatorMissingSlots` | warning | This node's validator missed half the leader slots its downtime window allows, 2 min on |
| `BlockulaCertificateBelowQuorum` | critical | The newest certificate has fewer signers than the active set's quorum |
| `BlockulaNoRecentCertificate` | warning | No certificate among the last 16 heights for 5 min |
| `HostDiskAlmostFull` / `HostDiskCritical` | warning / critical | A filesystem above 80% / 90% |
| `HostDiskFillsWithinAWeek` | warning | Linear forecast says a filesystem fills within 7 days |
| `HostMemoryLow`, `HostOomKill` | warning | Under 10% memory available; an OOM kill |
| `HostClockSkew` / `…Critical` | warning / critical | Clock off NTP by > 100 ms / > 250 ms (verifiers reject blocks > 1 s ahead) |
| `HostClockNotSynchronising` | warning | The kernel reports the clock unsynchronised |
| `Watchdog` | none | Always (heartbeat) |
| `AlertmanagerNotificationsFailing`, `PrometheusRuleFailures` | critical / warning | The pipeline itself is broken |

