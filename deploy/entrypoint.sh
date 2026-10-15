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
# GENESIS_HASH.
set -eu
bind=${BIND:-::}
set -- --bind "$bind"
[ -n "${P2P_PRESET:-}" ] && set -- "$@" --p2p-preset "$P2P_PRESET"
[ -n "${CLIENT_IP_HEADER:-}" ] && set -- "$@" --client-ip-header "$CLIENT_IP_HEADER"
for url in ${PEER_HTTP:-}; do set -- "$@" --peer-http "$url"; done

as_node() { exec setpriv --reuid=blockula --regid=blockula --init-groups "$@"; }

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
    as_node blockula run --home "$home" "$@"
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
