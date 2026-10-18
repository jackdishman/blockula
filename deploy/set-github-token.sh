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
# Apps restart one at a time, and the next waits until the restarted app itself has finalized
# new blocks. If it cannot confirm that, stop there and leave the remaining apps untouched.
set -euo pipefail

PREFIX=${PREFIX:-blockula}
RPC_APP=${RPC_APP:-$PREFIX-testnet}
N=${N:-4}
RPC_URL=https://$RPC_APP.fly.dev
PROGRESS_SECS=${PROGRESS_SECS:-5}   # between the liveness checks (60 of them), for tests

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

height() { curl -fsS -m 5 "$RPC_URL/api/status" 2>/dev/null | jq -r '.height // 0' || echo 0; }
GENESIS_HASH=$(curl -fsS -m 5 "$RPC_URL/api/status" | jq -r '.genesis_hash')
[[ "$GENESIS_HASH" =~ ^[0-9a-f]{64}$ ]] || { echo "No valid genesis hash at $RPC_URL; nothing restarted" >&2; exit 1; }

for app in $(for i in $(seq 0 $((N - 1))); do echo "$PREFIX-v$i"; done) "$RPC_APP"; do
  before=$(height)
  printf 'GITHUB_TOKEN=%s\n' "$TOKEN" | fly secrets import -a "$app" >/dev/null
  echo "🔑 $app: GITHUB_TOKEN set (machine restarted)"
  ok=""
  port=7070; [ "$app" != "$RPC_APP" ] || port=7171
  target=$((before + 6))
  for _ in $(seq 60); do
    if [ "$(height)" -ge "$target" ] && fly ssh console -a "$app" -C "blockula checkpoint --peer http://127.0.0.1:$port --height $target --genesis-hash $GENESIS_HASH" >/dev/null 2>&1; then ok=1; break; fi
    sleep "$PROGRESS_SECS"
  done
  [ -n "$ok" ] || { echo "❌ $app did not confirm new finalized blocks after its restart (from #$before): stopping before the next app. Check fly logs -a $app, then rerun." >&2; exit 1; }
done
echo "✓ GITHUB_TOKEN set on $N validators and $RPC_APP"
