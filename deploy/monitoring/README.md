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
| `BlockulaRestarted` | warning | Counters reset or the height dropped in the last 15 min |
| `BlockulaVersionMismatch` | info | Your nodes run different versions for over an hour |
| `BlockulaCertificateBelowQuorum` | critical | The newest certificate has fewer signers than the active set's quorum |
| `BlockulaNoRecentCertificate` | warning | No certificate among the last 16 heights for 5 min |
| `HostDiskAlmostFull` / `HostDiskCritical` | warning / critical | A filesystem above 80% / 90% |
| `HostDiskFillsWithinAWeek` | warning | Linear forecast says a filesystem fills within 7 days |
| `HostMemoryLow`, `HostOomKill` | warning | Under 10% memory available; an OOM kill |
| `HostClockSkew` / `…Critical` | warning / critical | Clock off NTP by > 100 ms / > 250 ms (verifiers reject blocks > 1 s ahead) |
| `HostClockNotSynchronising` | warning | The kernel reports the clock unsynchronised |
| `Watchdog` | none | Always (heartbeat) |
| `AlertmanagerNotificationsFailing`, `PrometheusRuleFailures` | critical / warning | The pipeline itself is broken |

## Planned (TODO)

These need metrics the node doesn't export yet. The dashboard shows them as TODO panels; add the rules when the metrics land:

- **Peers:** connected p2p peers vs the active set (`blockula_p2p_peers`). Alert below quorum − 1.
- **Missed leader slots** per validator over the downtime window (P-STAKE, MAINNET-DESIGN 3.4). Warn well before the jail threshold.
- **Jailed / tombstoned** for this operator's validators (P-STAKE).
- **Slashing protection:** last signed view per epoch (P-UPGRADE, 3.9). Alert if it goes backwards or stalls while the chain advances.
- **Process start time**, for an exact restart alert instead of counter resets.
