# shellcheck shell=bash
# The Fly machine configurations deploy/launch.sh deploys, as functions of its settings (sourced
# by launch.sh; deploy/test-deploy.sh checks them offline). Every function prints TOML.
#
# Reads: PREFIX, RPC_APP, N, REGIONS, RPC_REGION, NET (genesis.json's directory), P2P_PORT,
# HTTP_PORT, RPC_PORT, VALIDATOR_VM, VALIDATOR_MEMORY, RPC_VM, RPC_MEMORY, GENESIS_HASH, and the
# optional HALT_HEIGHT and NETWORK_HALTED_AT (validators only), VALIDATOR_PRUNE_BLOCKS,
# RPC_PRUNE_BLOCKS and RPC_ARCHIVE.
#
# What the validators' configurations never hold: their key. Each consensus key is the Fly
# secret VALIDATOR_KEY_B64 only until it's on the volume (deploy/entrypoint.sh reads it from the
# environment once, launch.sh then deletes it), and no `[[files]]` entry refers to it, so a
# restart after it's gone (set-github-token.sh, `fly machine restart`, a later launch.sh) starts
# from the volume alone (relaunch problem 8).

# `  KEY = "value"` when value is set.
fly_env() { [ -z "${2:-}" ] || printf '  %s = "%s"\n' "$1" "$2"; }

# The validators' private HTTP endpoints over Fly's network, but validator $1's own.
validator_peer_urls() {
  local out="" j
  for j in $(seq 0 $((N - 1))); do
    [ "$j" = "${1:-}" ] || out="$out http://$PREFIX-v$j.internal:$HTTP_PORT"
  done
  echo "${out# }"
}

# A validator's HTTP peers (`--peer-http`): the other validators and the public node, so sync,
# epoch exchange and slashing protection's peer checks (OPERATIONS.md "How slashing protection
# works") never rest on one node.
validator_peers() { echo "$(validator_peer_urls "$1") http://$RPC_APP.internal:$RPC_PORT"; }

validator_toml() { # index
  local i=$1 app=$PREFIX-v$1
  cat <<TOML
app = "$app"
primary_region = "${REGIONS[$i]}"
[env]
  ROLE = "validator"
  P2P_PRESET = "public-nat"
  BLOCKULA_LOG = "warn,blockula=info"
  PEER_HTTP = "$(validator_peers "$i")"
$(fly_env PRUNE_BLOCKS "${VALIDATOR_PRUNE_BLOCKS:-}")
$(fly_env HALT_HEIGHT "${HALT_HEIGHT:-}")
$(fly_env NETWORK_HALTED_AT "${NETWORK_HALTED_AT:-}")
[mounts]
  source = "data"
  destination = "/data"
[[files]]
  guest_path = "/etc/blockula/genesis.json"
  local_path = "$NET/genesis.json"
[[services]]
  internal_port = $P2P_PORT
  protocol = "tcp"
  auto_stop_machines = "off"
  auto_start_machines = false
  min_machines_running = 1
  [[services.ports]]
    port = $P2P_PORT
[[restart]]
  policy = "on-failure"
  retries = 10
[metrics]
  port = $HTTP_PORT
  path = "/metrics"
[[vm]]
  size = "$VALIDATOR_VM"
  memory = "$VALIDATOR_MEMORY"
TOML
}

rpc_toml() {
  local archive=""
  case "${RPC_ARCHIVE:-}" in 1 | true) archive=1 ;; esac
  cat <<TOML
app = "$RPC_APP"
primary_region = "$RPC_REGION"
[env]
  ROLE = "rpc"
  GENESIS_HASH = "$GENESIS_HASH"
  CLIENT_IP_HEADER = "fly-client-ip"
  PEER_HTTP = "$(validator_peer_urls)"
  HTTP_PORT = "$RPC_PORT"
  BLOCKULA_LOG = "warn,blockula=info"
$(fly_env ARCHIVE "$archive")
$( [ -n "$archive" ] || fly_env PRUNE_BLOCKS "${RPC_PRUNE_BLOCKS:-}")
[mounts]
  source = "data"
  destination = "/data"
[[files]]
  guest_path = "/etc/blockula/genesis.json"
  local_path = "$NET/genesis.json"
[http_service]
  internal_port = $RPC_PORT
  force_https = true
  auto_stop_machines = "off"
  min_machines_running = 1
  [[http_service.checks]]
    method = "GET"
    path = "/api/health"
    interval = "15s"
    timeout = "5s"
    grace_period = "60s"
[[restart]]
  policy = "on-failure"
  retries = 10
[metrics]
  port = $RPC_PORT
  path = "/metrics"
[[vm]]
  size = "$RPC_VM"
  memory = "$RPC_MEMORY"
TOML
}
