#!/usr/bin/env bash
# Validates this monitoring stack with the same pinned images it runs: the Prometheus config,
# the alert rules and their unit tests, the Alertmanager config, the compose file, the dashboard,
# and that every blockula_* metric used here is one the node exports (node/src/api.rs).
# CI runs it (ci.yml, "ops" job); run it after editing anything in deploy/monitoring.
set -euo pipefail
cd "$(dirname "$0")"
image() { sed -nE "s|^ *image: *($1:[^ ]+).*|\1|p" docker-compose.yml; }

docker run --rm -v "$PWD/prometheus:/etc/prometheus:ro" --entrypoint promtool "$(image prom/prometheus)" check config /etc/prometheus/prometheus.yml
docker run --rm -v "$PWD/prometheus:/etc/prometheus:ro" -w /etc/prometheus --entrypoint promtool "$(image prom/prometheus)" test rules alerts.test.yml
docker run --rm -v "$PWD/alertmanager:/etc/alertmanager:ro" --entrypoint amtool "$(image prom/alertmanager)" check-config /etc/alertmanager/alertmanager.yml
GRAFANA_ADMIN_PASSWORD=check docker compose -f docker-compose.yml config --quiet
jq -e '.uid == "blockula-node" and (.panels | length) > 0' grafana/dashboards/blockula.json >/dev/null

# Metric names: a rename in the node must show up here, not as a silently empty panel.
exported=$(grep -oE 'blockula_[a-z0-9_]+' ../../node/src/api.rs | sort -u)
missing=0
while read -r m; do
  grep -qx "$m" <<< "$exported" || { echo "✗ $m isn't exported by node/src/api.rs" >&2; missing=1; }
done < <(grep -ohE 'blockula_[a-z0-9_]+' prometheus/alerts.yml grafana/dashboards/blockula.json | sort -u)
[ $missing = 0 ]
echo "✓ monitoring config, rules + tests, alertmanager, compose, dashboard, metric names"
