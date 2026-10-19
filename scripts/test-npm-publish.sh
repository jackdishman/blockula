#!/usr/bin/env bash
# Runs npm-publish.yml's own publish step (the "Publish @blockula/sdk, then @blockula/mcp" run
# script, read out of the workflow) as a dry run against tarballs packed from this checkout, so a
# publish command npm can't read fails here rather than at release time. v0.5.0's dry run failed
# because `npm publish dist-npm/x.tgz` is read as a GitHub owner/repo shorthand; the path needs "./".
#
# Nothing can be published: an `npm` stand-in drops --provenance (no OIDC token here) and refuses
# any `npm publish` without --dry-run, and the tarballs carry a version that is never released.
# Needs node, npm, the yaml package (npm ci --ignore-scripts) and the npm registry (npm view).
#   bash scripts/test-npm-publish.sh
set -euo pipefail
cd "$(dirname "$0")/.."
T=$(mktemp -d "${TMPDIR:-/tmp}/npm-publish-test.XXXXXX")
trap 'rm -rf "$T"' EXIT
fails=0
ok() { echo "✓ $1"; }
bad() { echo "✗ $1" >&2; fails=$((fails + 1)); }

# The publish step's script, exactly as the workflow runs it.
node --input-type=module - "$T/publish.sh" <<'JS'
import { readFileSync, writeFileSync } from "node:fs";
import { parse } from "yaml";
const wf = parse(readFileSync(".github/workflows/npm-publish.yml", "utf8"));
const step = (wf.jobs?.publish?.steps ?? []).find((s) => s.name === "Publish @blockula/sdk, then @blockula/mcp");
if (!step?.run) { console.error("✗ npm-publish.yml has no publish step named \"Publish @blockula/sdk, then @blockula/mcp\""); process.exit(1); }
writeFileSync(process.argv[2], step.run);
JS

# Tarballs as the build job packs them, named for a version no release has.
VERSION=0.0.0-publish-test
mkdir -p "$T/work/dist-npm"
sdk_json=$(npm pack -w @blockula/sdk --ignore-scripts --pack-destination "$T/work/dist-npm" --json 2> /dev/null)
mcp_json=$(npm pack -w @blockula/mcp --ignore-scripts --pack-destination "$T/work/dist-npm" --json 2> /dev/null)
sdk_file=$(node -p 'JSON.parse(process.argv[1])[0].filename' "$sdk_json")
mcp_file=$(node -p 'JSON.parse(process.argv[1])[0].filename' "$mcp_json")
SDK_INTEGRITY=$(node -p 'JSON.parse(process.argv[1])[0].integrity' "$sdk_json")
mv "$T/work/dist-npm/$sdk_file" "$T/work/dist-npm/blockula-sdk-$VERSION.tgz"
mv "$T/work/dist-npm/$mcp_file" "$T/work/dist-npm/blockula-mcp-$VERSION.tgz"

# The npm stand-in: logs each publish, keeps it a dry run, passes everything else on.
REAL_NPM=$(command -v npm)
mkdir -p "$T/bin"
cat > "$T/bin/npm" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [ "\${1:-}" = publish ]; then
  echo "\$*" >> "$T/calls"
  case " \$* " in *" --dry-run "*) ;; *) echo "stand-in npm: refusing a publish without --dry-run" >&2; exit 97 ;; esac
  args=(); for a in "\$@"; do [ "\$a" = --provenance ] || args+=("\$a"); done
  exec "$REAL_NPM" "\${args[@]}"
fi
exec "$REAL_NPM" "\$@"
EOF
chmod +x "$T/bin/npm"

rc=0
(cd "$T/work" && env PATH="$T/bin:$PATH" DRY_RUN=true GITHUB_REF_NAME="v$VERSION" SDK_INTEGRITY="$SDK_INTEGRITY" \
  bash -e "$T/publish.sh") > "$T/out" 2>&1 || rc=$?
[ "$rc" = 0 ] && ok "the publish step's dry run succeeds" || bad "the publish step failed (exit $rc): $(tail -n 8 "$T/out")"
! grep -q 'ls-remote' "$T/out" && ok "npm reads the tarballs as files (no git lookup)" || bad "npm took a tarball path for a git repository"
[ "$(grep -c 'Publishing to https://registry.npmjs.org/ with tag latest and public access (dry-run)' "$T/out")" = 2 ] && ok "both packages reach npm's dry-run publish" || bad "expected two dry-run publishes: $(grep -E 'notice|error' "$T/out" | tail -n 6)"
calls=$(cat "$T/calls" 2> /dev/null || true)
grep -q "publish \./dist-npm/blockula-sdk-$VERSION.tgz " <<< "$calls" && grep -q "publish \./dist-npm/blockula-mcp-$VERSION.tgz " <<< "$calls" && ok "the SDK, then the MCP, by ./ paths" || bad "publish calls: $calls"
[ "$(head -n 1 <<< "$calls" | grep -c blockula-sdk)" = 1 ] && ok "the SDK goes first" || bad "the MCP was published before the SDK"

if [ $fails -gt 0 ]; then echo "✗ $fails failed" >&2; exit 1; fi
echo "✓ npm-publish.yml's publish step works as a dry run"
