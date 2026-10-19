#!/usr/bin/env bash
# Sets GITHUB_TOKEN on every testnet app. Validators use it to check contribution claims on GitHub
# (2 calls per claim per validator); the public node uses it for /api/github link checks. Without
# it each machine gets GitHub's 60 unauthenticated calls an hour.
#
#   pbpaste | deploy/set-github-token.sh
#
# The token is read from stdin, so it never appears in argv or shell history. Use a fine-grained
# token (github.com/settings/personal-access-tokens/new) with "Public repositories (read-only)"
# and no permissions: it ends up on internet-facing machines.
#
# Apps restart one at a time, through the participation gate (deploy/restart-gate.sh): nothing
# restarts until the network is at full participation, and the next app waits until the restarted
# validator is back (a new process that signs, is linked to every other validator and has proposed
# a block) and the chain is at full pace again. The public node goes last. When that can't be
# confirmed (an app or the public node doesn't answer, or GATE_SECS go by), it stops there and
# leaves the remaining apps untouched.
set -euo pipefail

PREFIX=${PREFIX:-blockula}
RPC_APP=${RPC_APP:-$PREFIX-testnet}
N=${N:-4}
RPC_URL=https://$RPC_APP.fly.dev
HTTP_PORT=7070   # validators' HTTP, on their machines
RPC_PORT=7171    # the public node's
# shellcheck source=deploy/restart-gate.sh
source "$(dirname "$0")/restart-gate.sh"

IFS= read -r TOKEN || true
TOKEN=${TOKEN//[[:space:]]/}
[[ "$TOKEN" =~ ^(github_pat_|ghp_)[A-Za-z0-9_]+$ ]] || { echo "stdin doesn't look like a GitHub token" >&2; exit 1; }

# The header goes through stdin too (curl -H @-), not argv.
headers=$(printf 'Authorization: Bearer %s\n' "$TOKEN" | curl -sS -D - -o /dev/null -H @- https://api.github.com/rate_limit)
status=$(printf '%s\n' "$headers" | head -1 | awk '{print $2}')
[ "$status" = 200 ] || { echo "GitHub rejected the token (HTTP $status)" >&2; exit 1; }
scopes=$(printf '%s\n' "$headers" | tr -d '\r' | awk -F': ' 'tolower($1) == "x-oauth-scopes" {print $2}')
if [ -n "$scopes" ]; then
  echo "This is a classic token with scopes: $scopes" >&2
  echo "It would sit on public servers. Use a fine-grained, public-read-only token instead." >&2
  exit 1
fi

GENESIS_HASH=$(curl -fsS -m 10 "$RPC_URL/api/status" | jq -r '.genesis_hash') || GENESIS_HASH=""
[[ "$GENESIS_HASH" =~ ^[0-9a-f]{64}$ ]] || { echo "No valid genesis hash at $RPC_URL; nothing restarted" >&2; exit 1; }

gate_wait "before the first restart" gate_check \
  || { echo "❌ the network isn't at full participation: $GATE_WHY. Nothing restarted." >&2; exit 1; }
for i in $(seq 0 $((N - 1))); do
  app=$PREFIX-v$i
  # Its process start time, from the last check (which just passed).
  start=${GATE_STARTED[$i]:-}
  [ -n "$start" ] || { echo "❌ no process start time for $app: nothing more restarted" >&2; exit 1; }
  printf 'GITHUB_TOKEN=%s\n' "$TOKEN" | fly secrets import -a "$app" >/dev/null
  echo "🔑 $app: GITHUB_TOKEN set (machine restarted)"
  gate_wait "after restarting $app" gate_check "$i=$start" \
    || { echo "❌ $app isn't back: $GATE_WHY. Stopping before the next app (the rest are untouched): check fly logs -a $app, then rerun." >&2; exit 1; }
done
gate_wait "before restarting $RPC_APP" gate_answering rpc \
  || { echo "❌ $GATE_WHY: $RPC_APP not restarted (the validators have the token). Rerun once it answers." >&2; exit 1; }
start=$GATE_RPC_STARTED
printf 'GITHUB_TOKEN=%s\n' "$TOKEN" | fly secrets import -a "$RPC_APP" >/dev/null
echo "🔑 $RPC_APP: GITHUB_TOKEN set (machine restarted)"
gate_wait "after restarting $RPC_APP" gate_public_back "$start" \
  || { echo "❌ $RPC_APP isn't back: $GATE_WHY. Check fly logs -a $RPC_APP." >&2; exit 1; }
echo "✓ GITHUB_TOKEN set on $N validators and $RPC_APP"
