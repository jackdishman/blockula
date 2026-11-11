#!/usr/bin/env bash
# Verify a built node image embeds the served wallet. No host volumes, wallet keys or
# public network: the throwaway dev chain runs as the image's unprivileged user.
#   docker build -f deploy/Dockerfile -t blockula-wallet-test .
#   bash deploy/test-wallet-image.sh blockula-wallet-test
set -euo pipefail
image=${1:-blockula-wallet-test}
work=$(mktemp -d)
container=""
cleanup() {
  if [ -n "$container" ]; then docker rm -f "$container" >/dev/null 2>&1 || true; fi
  rm -rf "$work"
}
trap cleanup EXIT
container=$(docker run -d --read-only --user 10001:10001 \
  --tmpfs /data:rw,uid=10001,gid=10001,mode=700 \
  -e BLOCKULA_HOME=/data -p 127.0.0.1::7070 \
  --entrypoint blockula "$image" dev --bind 0.0.0.0 --port 7070)
port=$(docker port "$container" 7070/tcp | sed -n 's/^127\.0\.0\.1://p')
[ -n "$port" ] || { echo "No loopback test port" >&2; exit 1; }
url="http://127.0.0.1:$port"
for _ in $(seq 1 60); do
  if curl -fsS --max-time 2 "$url/api/health" > /dev/null 2>&1; then break; fi
  if [ "$(docker inspect --format '{{.State.Running}}' "$container")" != true ]; then
    docker logs "$container" >&2
    exit 1
  fi
  sleep 1
done
curl -fsS --max-time 5 "$url/api/status" > "$work/status.json"
node -e 'const s=require(process.argv[1]); if(s.wallet!=="/wallet/") throw Error("wallet is not embedded: "+JSON.stringify(s.wallet)); console.log("wallet:",s.wallet)' "$work/status.json"
curl -fsS --max-time 5 -D "$work/headers" "$url/wallet/" > "$work/index.html"
grep -iq '^content-security-policy:.*script-src' "$work/headers"
asset=$(node -e 'const s=require("node:fs").readFileSync(process.argv[1],"utf8");const m=s.match(/src="(\/wallet\/_next\/static\/[^"?]+\.js[^" ]*)"/);if(!m)throw Error("no exported wallet script");console.log(m[1])' "$work/index.html")
curl -fsS --max-time 5 "$url$asset" > "$work/asset.js"
test -s "$work/asset.js"
# The final image contains the binary, not the Node.js toolchain or either build stage.
docker run --rm --entrypoint /bin/sh "$image" -c 'test ! -e /src && test ! -e /wallet && ! command -v node'
echo "✓ image serves /wallet/ and its JavaScript under CSP; no Node.js/build-stage files in runtime"
