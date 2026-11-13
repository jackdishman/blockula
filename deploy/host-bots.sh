#!/usr/bin/env bash
# Runs the playground's house bots on Fly.io, next to a hosted chain's validators: the trick-or-treat
# house (examples/trick-or-treat-bot.mjs, as trickortreat.boo) and the guestbook
# (examples/guestbook.mjs, as guestbook.boo). It's how the demo chain's playground stays open
# (docs/PLAYGROUND.md). One machine, no public ports, a 1 GB volume for the bots' keys and state.
#
#   deploy/host-bots.sh                      # the demo chain, Fly app blockula-demo-house
#
# Re-run it to pick up a changed example or SDK, or after a name's transfer was offered to the
# house: it never makes a second key. Settings (env):
#
#   NETWORK      the SDK network the bots pin (default demo)
#   APP          Fly app (default blockula-demo-house)
#   ORG, REGION  Fly org and region (default: personal, iad)
#   HOSTED_DIR   where the bots' keys live on this machine
#                (default ~/.config/blockula/hosted/blockula-demo-2-house, mode 700)
#
# What it does, each step skipped once done:
#   1. makes a key for each bot in HOSTED_DIR (trickortreat-key.json, guestbook-key.json, mode 600).
#      Two accounts, never one: the wallet's guestbook page lists every payment to the guestbook's
#      owner, so the house's plays mustn't land there;
#   2. accepts either name if it has been offered to its bot's account (deploy/bots/accept-names.mjs).
#      A bot whose name belongs to someone else waits, starting again every minute, and registers
#      a name that's free;
#   3. builds the SDK from this checkout and packs it, so the bots run the examples on the SDK they
#      were written for;
#   4. creates the app and its volume, stages the keys as secrets on the first boot and deploys;
#      once the machine runs, deletes the secrets (the keys live on the volume, and here).
#
# The coins are worthless and the house's dice are its operator's: it's a toy.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
die() { echo "host-bots: $*" >&2; exit 1; }

NETWORK=${NETWORK:-demo}
APP=${APP:-blockula-demo-house}
ORG=${ORG:-personal}
REGION=${REGION:-iad}
HOSTED_DIR=${HOSTED_DIR:-$HOME/.config/blockula/hosted/blockula-demo-2-house}
for t in fly jq node npm; do command -v "$t" >/dev/null || die "needs $t"; done

# 1. The keys.
install -d -m 700 "$HOSTED_DIR"
for bot in trickortreat guestbook; do
  key=$HOSTED_DIR/$bot-key.json
  [ -s "$key" ] && continue
  (umask 077 && node -e 'process.stdout.write(JSON.stringify({ ["seed"]: require("node:crypto").randomBytes(32).toString("hex") }))' > "$key")
  echo "✓ a new key for $bot in $key"
done

# 3 (first: step 2 runs on this SDK). The SDK, built and packed from this checkout.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
(cd "$repo" && npm run build -w @blockula/sdk >/dev/null && npm pack -w @blockula/sdk --pack-destination "$work" >/dev/null)

# 2. The names.
(cd "$repo" && node deploy/bots/accept-names.mjs "$HOSTED_DIR" --network "$NETWORK")

# 4. The app.
fly apps list --org "$ORG" --json | jq -e --arg a "$APP" '.[] | select(.Name == $a)' >/dev/null \
  || { fly apps create "$APP" --org "$ORG" >/dev/null; echo "✓ created app $APP"; }
# flyctl's volume listing names the field `name`; older ones printed `Name`. Missing it made a
# volume on every re-run, each left unattached beside the one the machine mounts.
if ! fly volumes list -a "$APP" --json | jq -e '.[] | select((.name // .Name) == "data")' >/dev/null; then
  fly volumes create data -a "$APP" -r "$REGION" -s 1 --yes >/dev/null
  echo "✓ volume data (1 GB, $REGION)"
fi
first_boot=""
if [ ! -f "$HOSTED_DIR/keys-on-volume" ]; then
  first_boot=1
  {
    printf '%s=%s\n' TRICKORTREAT_SEED "$(jq -r .seed "$HOSTED_DIR/trickortreat-key.json")"
    printf '%s=%s\n' GUESTBOOK_SEED "$(jq -r .seed "$HOSTED_DIR/guestbook-key.json")"
  } | fly secrets import -a "$APP" --stage >/dev/null
fi

cat > "$work/fly.toml" <<TOML
app = "$APP"
primary_region = "$REGION"
[env]
  BOT_NETWORK = "$NETWORK"
[mounts]
  source = "data"
  destination = "/data"
[[restart]]
  policy = "always"
[[vm]]
  size = "shared-cpu-1x"
  memory = "256mb"
TOML

# The build context is the files the image needs and nothing else from the working tree.
ctx=$work/context
install -d "$ctx/examples" "$ctx/deploy/bots"
cp "$repo/examples/trick-or-treat-bot.mjs" "$repo/examples/guestbook.mjs" "$ctx/examples/"
cp "$here/bots/Dockerfile" "$here/bots/entrypoint.sh" "$ctx/deploy/bots/"
mv "$work"/blockula-sdk-*.tgz "$ctx/sdk.tgz"
fly deploy "$ctx" -c "$work/fly.toml" --dockerfile "$ctx/deploy/bots/Dockerfile" --ha=false --depot=false --yes

if [ -n "$first_boot" ]; then
  for _ in $(seq 1 30); do
    fly machines list -a "$APP" --json | jq -e '.[] | select(.state == "started")' >/dev/null && break
    sleep 5
  done
  fly machines list -a "$APP" --json | jq -e '.[] | select(.state == "started")' >/dev/null || die "the machine didn't start (fly logs -a $APP)"
  sleep 15 # the entrypoint writes the keys before it starts the bots
  fly secrets unset TRICKORTREAT_SEED GUESTBOOK_SEED -a "$APP" >/dev/null
  touch "$HOSTED_DIR/keys-on-volume"
  echo "✓ the bots' keys are on the volume; the Fly secrets are gone"
fi

cat <<EOF

🎃 The house bots run on Fly app $APP (fly logs -a $APP).
   trickortreat: $(cd "$repo" && node -e 'import("@blockula/sdk").then(async ({ Client, Wallet }) => { const c = new Client({ network: process.argv[1] }); await c.addressPrefix(); console.log(Wallet.fromSeed(JSON.parse(require("node:fs").readFileSync(process.argv[2], "utf8")).seed, c).address) })' "$NETWORK" "$HOSTED_DIR/trickortreat-key.json")
   guestbook:    $(cd "$repo" && node -e 'import("@blockula/sdk").then(async ({ Client, Wallet }) => { const c = new Client({ network: process.argv[1] }); await c.addressPrefix(); console.log(Wallet.fromSeed(JSON.parse(require("node:fs").readFileSync(process.argv[2], "utf8")).seed, c).address) })' "$NETWORK" "$HOSTED_DIR/guestbook-key.json")
   A bot whose name isn't the house's yet waits; re-run this script once a transfer is offered.
EOF
