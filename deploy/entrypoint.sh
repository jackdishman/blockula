#!/bin/sh
# Seeds the node's home on the persistent volume, then runs the node as the unprivileged
# `blockula` user.
#   ROLE=validator: the validator key is written from the VALIDATOR_KEY_B64 secret (an
#                   environment variable, base64 of validator.key) ONLY when this volume has never
#                   held a key. launch.sh deletes that secret after the first boot, so a
#                   cloned/scaled/replacement machine with an empty volume can't come up signing
#                   with the same key (double-signing). Nothing in the machine's configuration
#                   refers to the secret, so restarts after it's gone read the key from the volume
#                   alone. The variable is dropped before the node starts.
#   ROLE=rpc:       public full node; with an empty home it joins once (pinning GENESIS_HASH,
#                   which `join` requires for a chain this binary doesn't know, and a checkpoint:
#                   CHECKPOINT, or one its PEER_HTTP nodes agree on, below), then restarts with
#                   `blockula run`.
# Optional env (fly.toml): P2P_PRESET, CLIENT_IP_HEADER, PEER_HTTP (space-separated URLs), BIND,
# GENESIS_HASH, HALT_HEIGHT (a coordinated stop: OPERATIONS.md "Incidents"), NETWORK_HALTED_AT
# (`--network-halted-at`: the whole network is halted at that height, OPERATIONS.md), PRUNE_BLOCKS
# (`--prune-blocks`: the block window to keep; validators keep ~2 days without it, full nodes
# everything), ARCHIVE=1 (`--archive`), CHECKPOINT (HEIGHT:HASH:SET_HASH for an empty rpc home),
# HTTP_PORT (the HTTP port: the rpc's 7171 and a validator's 7070 by default).
#
# Joining after the weak-subjectivity period (relaunch problem 1): a full node with an empty home
# refuses to sync from a root older than the period (OPERATIONS.md "Weak subjectivity"), and a
# testnet's genesis is older than that within days. So an empty rpc home takes its checkpoint
# from CHECKPOINT if set, else asks `blockula checkpoint` for the one every PEER_HTTP node agrees
# on (each must serve the genesis GENESIS_HASH pins). On the testnet those are the operator's own
# validators over Fly's private network: the operator trusting its own validators, which is what
# a checkpoint from "sources you trust" means for a node it runs itself. If no peer answers, it
# joins without one (fine while the chain is younger than the period; otherwise `join` refuses
# and the machine restarts to try again).
#
# Deliberate stops stay down (OPS-5): a validator exits 10 at --halt-height, 11 when the chain
# needs a newer binary, 12 when slashing protection sees its key signing elsewhere. Restarting
# would only stop again (or, for 12, risk a double-sign), so this script then idles instead of
# exiting: the machine stays up without the node and no restart policy loops it. Deploying a new
# image or `fly machine restart` starts the node again, once the operator has acted; for 12 the
# node itself also stays stopped (it records the stop in $home/double-sign-halt.json and exits 12
# at every start until that file is deleted). Any other exit is passed on, so crashes restart as
# usual.
#
# BLOCKULA_DATA (/data), BLOCKULA_ETC (/etc/blockula) and BLOCKULA_BIN (blockula) move it off the
# image for tests (deploy/test-deploy.sh) and local rehearsals; run as a user other than root it
# doesn't change users.
set -eu
data=${BLOCKULA_DATA:-/data}
etc=${BLOCKULA_ETC:-/etc/blockula}
bin=${BLOCKULA_BIN:-blockula}
bind=${BIND:-::}
set -- --bind "$bind"
[ -n "${P2P_PRESET:-}" ] && set -- "$@" --p2p-preset "$P2P_PRESET"
[ -n "${CLIENT_IP_HEADER:-}" ] && set -- "$@" --client-ip-header "$CLIENT_IP_HEADER"
[ -n "${HALT_HEIGHT:-}" ] && set -- "$@" --halt-height "$HALT_HEIGHT"
[ -n "${NETWORK_HALTED_AT:-}" ] && set -- "$@" --network-halted-at "$NETWORK_HALTED_AT"
case "${ARCHIVE:-}" in
  1 | true) set -- "$@" --archive ;;
  "" | 0 | false) [ -n "${PRUNE_BLOCKS:-}" ] && set -- "$@" --prune-blocks "$PRUNE_BLOCKS" ;;
  *) echo "ARCHIVE must be 1 or 0, got $ARCHIVE" >&2; exit 1 ;;
esac
for url in ${PEER_HTTP:-}; do set -- "$@" --peer-http "$url"; done

if [ "$(id -u)" = 0 ]; then
  root=1
else
  root=""
fi
# Runs a command as the node user (root drops to `blockula`; anyone else stays who they are).
as_user() {
  if [ -n "$root" ]; then setpriv --reuid=blockula --regid=blockula --init-groups "$@"; else "$@"; fi
}
as_node() {
  if [ -n "$root" ]; then exec setpriv --reuid=blockula --regid=blockula --init-groups "$@"; else exec "$@"; fi
}
# A directory (mode 700) or file the node user owns.
mkhome() {
  if [ -n "$root" ]; then install -d -o blockula -g blockula -m 700 "$1"; else install -d -m 700 "$1"; fi
}
put() { # mode src dst
  if [ -n "$root" ]; then install -o blockula -g blockula -m "$1" "$2" "$3"; else install -m "$1" "$2" "$3"; fi
}

# Runs the node as a child (signals forwarded), and stays down after a deliberate stop.
run_node() {
  # Started directly (not through a function), so $! is the node itself and signals reach it.
  if [ -n "$root" ]; then setpriv --reuid=blockula --regid=blockula --init-groups "$@" & else "$@" & fi
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
        12) why="slashing protection saw this validator's key sign elsewhere: find the other instance (OPERATIONS.md), then delete $data/home/double-sign-halt.json and restart" ;;
      esac
      echo "blockula exited $code: staying down, $why" >&2
      # Idle until the machine is stopped or restarted (the node is down; nothing to forward).
      trap 'exit 0' INT TERM
      while :; do sleep 3600 & wait $!; done
      ;;
  esac
  exit "$code"
}

# The validator key from VALIDATOR_KEY_B64 into $1 (a 32-byte hex seed), or a refusal.
seed_key() {
  key=$(printf '%s' "$VALIDATOR_KEY_B64" | base64 -d 2>/dev/null | tr -d ' \r\n') || key=""
  case "$key" in
    *[!0-9a-f]* | "") echo "refusing to start: VALIDATOR_KEY_B64 isn't a base64 validator.key (a 64-hex-digit seed)" >&2; exit 1 ;;
  esac
  [ "${#key}" -eq 64 ] || { echo "refusing to start: VALIDATOR_KEY_B64 isn't a base64 validator.key (a 64-hex-digit seed)" >&2; exit 1; }
  tmp="$1.tmp"
  (umask 077; printf '%s\n' "$key" > "$tmp")
  [ -z "$root" ] || chown blockula:blockula "$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$1"
}

[ -z "$root" ] || chown blockula:blockula "$data"
case "${ROLE:-}" in
  validator)
    home=$data/home
    mkhome "$home"
    [ -f "$home/genesis.json" ] || put 644 "$etc/genesis.json" "$home/genesis.json"
    if [ ! -f "$home/validator.key" ]; then
      if [ -f "$home/.seeded" ] || [ -z "${VALIDATOR_KEY_B64:-}" ]; then
        echo "refusing to start: no validator key on this volume (keys are seeded once; never clone or scale a validator)" >&2
        exit 1
      fi
      seed_key "$home/validator.key"
      touch "$home/.seeded"
      echo "🔑 validator key seeded onto this volume" >&2
    fi
    # The node never sees the secret, whether or not it was used.
    unset VALIDATOR_KEY_B64
    run_node "$bin" run --home "$home" ${HTTP_PORT:+--port "$HTTP_PORT"} "$@"
    ;;
  rpc)
    unset VALIDATOR_KEY_B64
    home=$data/full
    port=${HTTP_PORT:-7171}
    if [ -f "$home/join.json" ]; then
      as_node "$bin" run --home "$home" --port "$port" "$@"
    fi
    mkhome "$home"
    cp=${CHECKPOINT:-}
    if [ -z "$cp" ] && [ -n "${PEER_HTTP:-}" ] && [ -n "${GENESIS_HASH:-}" ]; then
      peers=""
      for url in $PEER_HTTP; do peers="$peers --peer $url"; done
      tries=${CHECKPOINT_TRIES:-6}
      while [ "$tries" -gt 0 ]; do
        # shellcheck disable=SC2086 # one --peer per URL
        if cp=$(as_user "$bin" checkpoint $peers --genesis-hash "$GENESIS_HASH"); then break; fi
        cp=""
        tries=$((tries - 1))
        [ "$tries" -eq 0 ] || sleep "${CHECKPOINT_RETRY_SECS:-10}"
      done
      if [ -n "$cp" ]; then
        echo "🔐 checkpoint $cp, as every PEER_HTTP node reports it" >&2
      else
        echo "⚠️  no checkpoint from PEER_HTTP ($PEER_HTTP): joining from the genesis alone (refused if the chain is older than the weak-subjectivity period)" >&2
      fi
    fi
    as_node "$bin" join --genesis "$etc/genesis.json" ${GENESIS_HASH:+--genesis-hash "$GENESIS_HASH"} ${cp:+--checkpoint "$cp"} --home "$home" --port "$port" "$@"
    ;;
  *) echo "set ROLE=validator or ROLE=rpc" >&2; exit 1 ;;
esac
