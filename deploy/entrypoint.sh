#!/bin/sh
# Seeds the node's home on the persistent volume, then runs the node as the unprivileged
# `blockula` user.
#   ROLE=validator: the validator key is copied from /etc/blockula/validator.key (a Fly secret file)
#                   ONLY when this volume has never held a key. launch.sh removes that secret after
#                   the first boot, so a cloned/scaled/replacement machine with an empty volume
#                   can't come up signing with the same key (double-signing).
#   ROLE=rpc:       public full node; joins once (pinning GENESIS_HASH, which `join` requires for
#                   a chain this binary doesn't know), then restarts with `blockula run`.
# Optional env (fly.toml): P2P_PRESET, CLIENT_IP_HEADER, PEER_HTTP (space-separated URLs), BIND,
# GENESIS_HASH, HALT_HEIGHT (a coordinated stop: OPERATIONS.md "Incidents").
#
# Deliberate stops stay down (OPS-5): a validator exits 10 at --halt-height, 11 when the chain
# needs a newer binary, 12 when slashing protection sees its key signing elsewhere. Restarting
# would only stop again (or, for 12, risk a double-sign), so this script then idles instead of
# exiting: the machine stays up without the node and no restart policy loops it. Deploying a new
# image or `fly machine restart` starts the node again, once the operator has acted; for 12 the
# node itself also stays stopped (it records the stop in $home/double-sign-halt.json and exits 12
# at every start until that file is deleted). Any other exit is passed on, so crashes restart as
# usual.
set -eu
bind=${BIND:-::}
set -- --bind "$bind"
[ -n "${P2P_PRESET:-}" ] && set -- "$@" --p2p-preset "$P2P_PRESET"
[ -n "${CLIENT_IP_HEADER:-}" ] && set -- "$@" --client-ip-header "$CLIENT_IP_HEADER"
[ -n "${HALT_HEIGHT:-}" ] && set -- "$@" --halt-height "$HALT_HEIGHT"
for url in ${PEER_HTTP:-}; do set -- "$@" --peer-http "$url"; done

as_node() { exec setpriv --reuid=blockula --regid=blockula --init-groups "$@"; }

# Runs the node as a child (signals forwarded), and stays down after a deliberate stop.
run_node() {
  setpriv --reuid=blockula --regid=blockula --init-groups "$@" &
  pid=$!
  trap 'kill -INT "$pid" 2>/dev/null || true' INT
  trap 'kill -TERM "$pid" 2>/dev/null || true' TERM
  code=0
  wait "$pid" || code=$?
  # `wait` returns early (above 128) when a trapped signal arrives: wait for the node itself.
  while kill -0 "$pid" 2>/dev/null; do code=0; wait "$pid" || code=$?; done
  case "$code" in
    10|11|12)
      case "$code" in
        10) why="it reached --halt-height (HALT_HEIGHT): unset it and restart once the network agrees to resume" ;;
        11) why="the chain needs a newer binary: deploy the release that supports it" ;;
        12) why="slashing protection saw this validator's key sign elsewhere: find the other instance (OPERATIONS.md), then delete /data/home/double-sign-halt.json and restart" ;;
      esac
      echo "blockula exited $code: staying down, $why" >&2
      # Idle until the machine is stopped or restarted (the node is down; nothing to forward).
      trap 'exit 0' INT TERM
      while :; do sleep 3600 & wait $!; done
      ;;
  esac
  exit "$code"
}

chown blockula:blockula /data
case "${ROLE:-}" in
  validator)
    home=/data/home
    install -d -o blockula -g blockula -m 700 "$home"
    [ -f "$home/genesis.json" ] || install -o blockula -g blockula -m 644 /etc/blockula/genesis.json "$home/genesis.json"
    if [ ! -f "$home/validator.key" ]; then
      if [ -f "$home/.seeded" ] || [ ! -s /etc/blockula/validator.key ]; then
        echo "refusing to start: no validator key on this volume (keys are seeded once; never clone or scale a validator)" >&2
        exit 1
      fi
      install -o blockula -g blockula -m 600 /etc/blockula/validator.key "$home/validator.key"
      touch "$home/.seeded"
    fi
    run_node blockula run --home "$home" "$@"
    ;;
  rpc)
    home=/data/full
    if [ -f "$home/genesis.json" ]; then
      as_node blockula run --home "$home" --port 7171 "$@"
    fi
    install -d -o blockula -g blockula -m 700 "$home"
    as_node blockula join --genesis /etc/blockula/genesis.json ${GENESIS_HASH:+--genesis-hash "$GENESIS_HASH"} --home "$home" --port 7171 "$@"
    ;;
  *) echo "set ROLE=validator or ROLE=rpc" >&2; exit 1 ;;
esac
