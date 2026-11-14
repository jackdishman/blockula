#!/bin/sh
# Runs the blackjack dealer daemon (services/blackjack/dealer) from the volume at /data: its key,
# the store's secret, the encrypted seed store and the store's backup all live there.
#
# Secrets: on the first boot the key file and the secret file come from the Fly secrets
# DEALER_KEY_JSON and STORE_SECRET_JSON (each the whole JSON `blackjack-dealer init` writes). Once
# a file exists its secret is never read again, and both variables are unset before anything else
# runs: the dealer never sees a secret in its environment. Nothing here prints a secret.
#
# The start gate: nothing is dealt until /data/ready exists. So the machine can be deployed, the
# old dealer stopped and its encrypted store copied onto the volume, and only then
# `touch /data/ready`: two dealers never run on one table.
set -eu
chown node:node /data
umask 077

# 1. The secrets, into files (never over a file that's there), then out of the environment.
seed_file() { # seed_file <path> <the secret's text>
  [ -e "$1" ] && return 0
  [ -n "$2" ] || return 0
  printf '%s\n' "$2" > "$1.new" # printf is the shell's own: the text is in no argument list
  chmod 600 "$1.new"
  chown node:node "$1.new"
  mv "$1.new" "$1"
  echo "wrote $1 from its Fly secret (mode 0600); unset the secret once the dealer runs"
}
seed_file /data/dealer-key.json "${DEALER_KEY_JSON:-}"
seed_file /data/store-secret.json "${STORE_SECRET_JSON:-}"
unset DEALER_KEY_JSON STORE_SECRET_JSON

# 2. The start gate. PID 1 gets no default signal handling, so a stop is answered here.
trap 'echo "stopped while waiting for /data/ready"; exit 0' INT TERM
while [ ! -e /data/ready ]; do
  echo "waiting for /data/ready (copy the store to /data if there is one, then: touch /data/ready)"
  sleep 15 &
  wait $! || true
done
trap - INT TERM

# 3. The files, as the dealer needs them: its own, and private. (What was copied in over ssh is root's.)
for f in /data/dealer-key.json /data/store-secret.json /data/seeds.enc.json /data/seeds.enc.json.backup; do
  [ -e "$f" ] || continue
  chown node:node "$f"
  chmod 600 "$f"
done
for f in /data/dealer-key.json /data/store-secret.json; do
  [ -s "$f" ] || { echo "$f is missing: set its Fly secret (DEALER_KEY_JSON, STORE_SECRET_JSON) or copy the file to the volume" >&2; exit 1; }
done
# This volume is mounted by this machine alone and nothing else has started in it: a lock here
# was left by a dealer that was killed (or copied in with the store), whatever pid it names.
rm -f /data/seeds.enc.json.lock

# 4. The settings: flags, which win over the environment, so the files can't be pointed off the
# volume. Every other BJ_DEALER_* setting (lowWater, pollMs, dryRun, …) is read by the dealer itself.
[ -n "${BJ_DEALER_RPC:-}" ] || { echo "BJ_DEALER_RPC isn't set (fly.toml [env])" >&2; exit 1; }
[ -n "${BJ_DEALER_TABLE:-}" ] || { echo "BJ_DEALER_TABLE isn't set (fly.toml [env])" >&2; exit 1; }
set -- run \
  --rpc "$BJ_DEALER_RPC" \
  --table "$BJ_DEALER_TABLE" \
  --dir /data \
  --key-file /data/dealer-key.json \
  --secret-file /data/store-secret.json \
  --store /data/seeds.enc.json \
  --backup /data/seeds.enc.json.backup \
  "--allow-remote=${BJ_DEALER_ALLOW_REMOTE:-false}" \
  --keychain=false
[ -z "${BJ_DEALER_CHAIN_ID:-}" ] || set -- "$@" --chain-id "$BJ_DEALER_CHAIN_ID"
[ -z "${BJ_DEALER_GENESIS_HASH:-}" ] || set -- "$@" --genesis-hash "$BJ_DEALER_GENESIS_HASH"
unset BJ_DEALER_CONFIG

# 5. The dealer, as `node`, in this process's place: it is PID 1, so Fly's SIGINT or SIGTERM
# reaches it directly and it stops cleanly (it lets what it sent come back, then exits 0).
export HOME=/home/node
cd /data
echo "starting the dealer for table $BJ_DEALER_TABLE on $BJ_DEALER_RPC"
exec setpriv --reuid=node --regid=node --init-groups node /app/services/blackjack/dealer/dist/cli.js "$@"
