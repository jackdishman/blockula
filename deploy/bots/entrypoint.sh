#!/bin/sh
# Runs the playground's house bots (deploy/host-bots.sh) from the volume at /data: each bot's key
# and state live there. On the first boot the keys come from the staged secrets TRICKORTREAT_SEED
# and GUESTBOOK_SEED; once a key file exists its secret is never read again, and host-bots.sh
# unsets the secrets after that boot. Each bot is restarted a minute after it stops (it stops while
# its name belongs to someone else, until the name's transfer is accepted).
set -eu
chown node:node /data
umask 077
for bot in trickortreat guestbook; do
  key=/data/$bot-key.json
  [ -s "$key" ] && continue
  case $bot in trickortreat) hex=${TRICKORTREAT_SEED:-} ;; guestbook) hex=${GUESTBOOK_SEED:-} ;; esac
  [ -n "$hex" ] || { echo "no key for $bot on the volume and no secret to seed it" >&2; exit 1; }
  printf '{"%s":"%s"}' seed "$hex" > "$key"
  chown node:node "$key"
done
unset TRICKORTREAT_SEED GUESTBOOK_SEED
NETWORK=${BOT_NETWORK:-demo}

run() {
  while :; do
    setpriv --reuid=node --regid=node --init-groups "$@" || true
    echo "$2 stopped; starting again in 60 s" >&2
    sleep 60
  done
}
cd /data
run node /app/examples/trick-or-treat-bot.mjs --network "$NETWORK" --key /data/trickortreat-key.json --state /data/trickortreat-state.json &
run node /app/examples/guestbook.mjs serve --network "$NETWORK" --key /data/guestbook-key.json --state /data/guestbook-state.json &
wait
