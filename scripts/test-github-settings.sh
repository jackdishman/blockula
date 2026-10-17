#!/usr/bin/env bash
# Tests the repository-settings tooling offline (audit supply-chain-B-3):
#   - scripts/check-required-checks.mjs passes on this tree and fails on each way the ruleset and
#     the workflows drift apart (variants are edits of the real files, and each edit must change
#     its file, so the cases can't silently go stale);
#   - deploy/github/apply-rulesets.sh, against a stand-in `gh`: a dry run changes nothing, --apply
#     writes exactly the rulesets that differ, a ruleset that matches is left alone.
#   bash scripts/test-github-settings.sh
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
CHECK=$ROOT/scripts/check-required-checks.mjs
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
pass=0
fail() { echo "✗ $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "✓ $1"; }

node "$CHECK" > /dev/null || fail "check-required-checks.mjs refuses this tree"
ok "this tree: the ruleset, ci.yml's list and the pull-request jobs agree"

# variant NAME EXPECTED FILE EDIT: copies the workflows and the ruleset, edits FILE (relative to
# the copy: wf/<name>.yml or ruleset.json) with EDIT (sed -E; "jq:<filter>" for the ruleset; or
# "js:<function of the text>"), and expects the check to fail saying EXPECTED.
variant() {
  local name=$1 want=$2 file=$3 edit=$4 out
  rm -rf "$T/v" && mkdir -p "$T/v/wf"
  cp .github/workflows/*.yml "$T/v/wf/"
  cp deploy/github/ruleset-main.json "$T/v/ruleset.json"
  cp "$T/v/$file" "$T/v/before"
  case $edit in
    jq:*) jq "${edit#jq:}" "$T/v/before" > "$T/v/$file" ;;
    js:*) node -e 'const fs = require("fs"); fs.writeFileSync(process.argv[2], eval(process.argv[3])(fs.readFileSync(process.argv[1], "utf8")))' "$T/v/before" "$T/v/$file" "${edit#js:}" ;;
    *) sed -E "$edit" "$T/v/before" > "$T/v/$file" ;;
  esac
  if cmp -s "$T/v/before" "$T/v/$file"; then fail "$name: the edit no longer changes $file"; fi
  if out=$(node "$CHECK" --ruleset "$T/v/ruleset.json" --workflows "$T/v/wf" 2>&1); then fail "$name: the check accepts it"; fi
  printf '%s' "$out" | grep -qF -- "$want" || fail "$name: expected \"$want\" in: $out"
  ok "$name: refused"
}
# The finding: a required job renamed (the ruleset's context is never reported again).
variant "a required job renamed" 'requires "Packages + wallet build", which no job reports' wf/ci.yml 's/^    name: Packages \+ wallet build$/    name: Packages + wallet build + SDK vectors/'
# The finding's other half: the live ruleset (two contexts) leaves the supply-chain jobs optional.
variant "the live ruleset: supply-chain jobs not required" '"Supply-chain pins, workflows, monitoring config" (ci.yml: ops) runs on every pull request but isn'"'"'t a required check' ruleset.json \
  'jq:.rules |= map(if .type == "required_status_checks" then .parameters.required_status_checks |= map(select(.context == "Node (Rust tests + local multi-validator testnet)" or .context == "Packages + wallet build")) else . end)'
variant "a check any app could post (no integration_id)" "must have integration_id 15368" ruleset.json \
  'jq:.rules |= map(if .type == "required_status_checks" then .parameters.required_status_checks[0] |= del(.integration_id) else . end)'
variant "a non-strict ruleset" "strict_required_status_checks_policy must be true" ruleset.json \
  'jq:.rules |= map(if .type == "required_status_checks" then .parameters.strict_required_status_checks_policy = false else . end)'
variant "a paths filter on a required job's workflow" "runs on pull_request only with paths" wf/fuzz.yml '/^  pull_request:$/a\
    paths: ["node/**"]'
variant "a required job skipped on pull requests" "skips it on pull requests" wf/ci.yml '/^    name: Clippy$/{n;s/^    if: .*/    if: github.event_name != '"'"'pull_request'"'"'/;}'
variant "a new pull-request job nobody required" '"Lint docs" (crypto.yml: docs) runs on every pull request but isn'"'"'t a required check' wf/crypto.yml '$a\
  docs:\
    name: Lint docs\
    runs-on: ubuntu-24.04\
    steps:\
      - run: "true"'
variant "a required job missing from ci.yml's list" "header doesn't list it as required" wf/ci.yml '/^#   required: Clippy$/d'
variant "ci.yml's list naming a check the ruleset lacks" 'lists "Something else" as required, but deploy/github/ruleset-main.json' wf/ci.yml '/^#   required: Clippy$/a\
#   required: Something else'
variant "two pull-request jobs with one name" 'are both named "Clippy"' wf/crypto.yml 's/^    name: ml-dsa \(node\)$/    name: Clippy/'

# Round 3: a required job that can skip itself, or pass without its work, counts as passing.
# The R1 reviewer's two bypasses first: an if: that skips it without naming pull_request, and a
# needs: on a job that isn't required (listed optional, so nothing else objects).
variant "a required job whose if: skips it (not naming pull_request)" "its if: (github.ref == 'refs/heads/main') can skip it" wf/ci.yml '/^    name: Clippy$/{n;s/^    if: .*/    if: github.ref == '"'"'refs\/heads\/main'"'"'/;}'
variant "a required job that needs an optional job" 'it needs "Lint prep" (ci.yml: lintprep), which isn'"'"'t' wf/ci.yml 'js:(s) => s.replace("    name: Clippy\n", "    name: Clippy\n    needs: lintprep\n").replace("\n  deps:\n", "\n  lintprep:\n    name: Lint prep\n    runs-on: ubuntu-24.04\n    steps:\n      - run: exit 1\n\n  deps:\n").replace("#   required: Fuzz harness\n", "#   required: Fuzz harness\n#   optional: Lint prep\n")'
variant "needs: as a list, naming an optional job" 'it needs "Lint prep" (ci.yml: lintprep), which isn'"'"'t' wf/ci.yml 'js:(s) => s.replace("    name: Clippy\n", "    name: Clippy\n    needs:\n      - deps\n      - lintprep\n").replace("\n  deps:\n", "\n  lintprep:\n    name: Lint prep\n    runs-on: ubuntu-24.04\n    steps:\n      - run: exit 1\n\n  deps:\n").replace("#   required: Fuzz harness\n", "#   required: Fuzz harness\n#   optional: Lint prep\n")'
variant "needs: [..] naming a job that doesn't exist" 'needs "nosuchjob", which isn'"'"'t a job in ci.yml' wf/ci.yml 's/^    name: Clippy$/    name: Clippy\n    needs: [deps, nosuchjob]/'
variant "continue-on-error on a required job" "has continue-on-error: true" wf/ci.yml 's/^    name: Clippy$/    name: Clippy\n    continue-on-error: true/'
variant "continue-on-error on a required job's check step" 'its step "(unnamed)" has continue-on-error: true' wf/ci.yml 's/^      - run: scripts\/clippy.sh$/      - run: scripts\/clippy.sh\n        continue-on-error: true/'
variant "an if: that skips a required job's check step" "has if: github.event_name == 'push'" wf/ci.yml 's/^      - run: scripts\/clippy.sh$/      - if: github.event_name == '"'"'push'"'"'\n        run: scripts\/clippy.sh/'
variant "if: failure() on a check step (it never runs when the job is green)" 'its step "Known-answer and cross-implementation tests" has if: failure()' wf/crypto.yml 's/^(      - name: Known-answer and cross-implementation tests)$/\1\n        if: failure()/'
variant "a job written in flow style (the checker can't read it)" 'job "docs: { name: Lint docs' wf/crypto.yml '$a\
  docs: { name: Lint docs, runs-on: ubuntu-24.04, steps: [{ run: "true" }] }'
variant "a push-triggered job reusing a required name" 'the required check "Clippy" is the name of 2 jobs' wf/release.yml 's/^    name: Tag is the crate version$/    name: Clippy/'
# The safe forms still pass: the allowed job condition in ${{ }}, and the reviewed step exceptions.
rm -rf "$T/v" && mkdir -p "$T/v/wf" && cp .github/workflows/*.yml "$T/v/wf/"
sed -E '/^    name: Clippy$/{n;s/^    if: .*/    if: ${{ github.event_name != '"'"'schedule'"'"' }}/;}' .github/workflows/ci.yml > "$T/v/wf/ci.yml"
cmp -s .github/workflows/ci.yml "$T/v/wf/ci.yml" && fail "the \${{ }} edit no longer changes ci.yml"
node "$CHECK" --workflows "$T/v/wf" > /dev/null || fail "the check refuses the allowed condition written as \${{ … }}"
grep -qF "if: failure()" .github/workflows/ci.yml && grep -qF "continue-on-error: true" .github/workflows/ci.yml || fail "the reviewed step exceptions are gone from ci.yml: update SAFE_STEPS"
ok "allowed: the reviewed job condition (also as \${{ … }}) and step exceptions"

# ---- deploy/github/apply-rulesets.sh against a stand-in gh ----
# GitHub has "main: reviewed PRs with green CI" (id 1) as FAKE_MAIN says (live: the two checks it
# required before; same: exactly ruleset-main.json) and no "release tags" ruleset unless FAKE_TAGS=1.
mkdir -p "$T/bin"
jq '.rules |= map(if .type == "required_status_checks" then .parameters.required_status_checks |= .[0:2] else . end)' deploy/github/ruleset-main.json > "$T/live-main.json"
cat > "$T/bin/gh" <<SH
#!/usr/bin/env bash
echo "\$*" >> "$T/gh.log"
# A read-only token (CI's) isn't shown the bypass actors.
ro_view() { if [ "\${FAKE_READONLY:-}" = 1 ]; then jq 'del(.bypass_actors)'; else cat; fi; }
case "\$*" in
  "api --paginate repos/jackdishman/blockula/rulesets?includes_parents=false")
    # Two rulesets come back as two pages, as --paginate prints them: one array each.
    if [ "\${FAKE_TAGS:-}" = 1 ]; then echo '[{"id":1,"name":"main: reviewed PRs with green CI","target":"branch"}]'; echo '[{"id":2,"name":"release tags","target":"tag"}]'
    else echo '[{"id":1,"name":"main: reviewed PRs with green CI","target":"branch"}]'; fi ;;
  "api repos/jackdishman/blockula/rulesets/1")
    src=$T/live-main.json; [ "\${FAKE_MAIN:-live}" = same ] && src=$ROOT/deploy/github/ruleset-main.json
    jq '. + {id: 1, source: "jackdishman/blockula", node_id: "x", _links: {}, created_at: "t", updated_at: "t", current_user_can_bypass: "always"}' "\$src" | ro_view ;;
  "api repos/jackdishman/blockula/rulesets/2")
    jq '. + {id: 2, _links: {}}' "\${FAKE_TAGS_FILE:-$ROOT/deploy/github/ruleset-tags.json}" | ro_view ;;
  "api --method PUT "* | "api --method POST "*) echo '{}' ;;
  *) echo "unexpected gh call: \$*" >&2; exit 9 ;;
esac
SH
chmod +x "$T/bin/gh"
apply() { : > "$T/gh.log"; env PATH="$T/bin:$PATH" "$@" deploy/github/apply-rulesets.sh ${APPLY:-} 2>&1; }
writes() { grep -cE -- '--method (PUT|POST)' "$T/gh.log" || true; }

out=$(apply env) || fail "dry run failed: $out"
[ "$(writes)" = 0 ] || fail "the dry run wrote to GitHub: $(cat "$T/gh.log")"
printf '%s' "$out" | grep -qE '^\+ +"context": "Clippy",?$' || fail "the dry run doesn't show the added checks: $out"
printf '%s' "$out" | grep -qF "+ release tags: no such ruleset" || fail "the dry run doesn't show the ruleset it would create: $out"
printf '%s' "$out" | grep -qF "Dry run: nothing changed" || fail "the dry run doesn't say so: $out"
ok "apply-rulesets.sh dry run: shows the diff and the new ruleset, writes nothing"

out=$(APPLY=--apply apply env) || fail "--apply failed: $out"
grep -qxF -- "api --method PUT repos/jackdishman/blockula/rulesets/1 --input ruleset-main.json" "$T/gh.log" || fail "--apply didn't PUT the main ruleset: $(cat "$T/gh.log")"
grep -qxF -- "api --method POST repos/jackdishman/blockula/rulesets --input ruleset-tags.json" "$T/gh.log" || fail "--apply didn't create the tag ruleset: $(cat "$T/gh.log")"
[ "$(writes)" = 2 ] || fail "--apply wrote more than the two rulesets: $(cat "$T/gh.log")"
ok "apply-rulesets.sh --apply: updates the differing ruleset, creates the missing one"

out=$(APPLY=--apply apply env FAKE_MAIN=same FAKE_TAGS=1) || fail "--apply with nothing to do failed: $out"
[ "$(writes)" = 0 ] || fail "--apply rewrote rulesets that already match: $(cat "$T/gh.log")"
[ "$(printf '%s\n' "$out" | grep -c '^✓ ')" = 2 ] || fail "expected both rulesets to match: $out"
ok "apply-rulesets.sh: rulesets that already match are left alone"

if out=$(env PATH="$T/bin:$PATH" deploy/github/apply-rulesets.sh --force 2>&1); then fail "an unknown flag was accepted: $out"; fi
ok "apply-rulesets.sh: an unknown flag is refused"

# --check, the daily drift job (.github/workflows/github-settings.yml): exit 1 on any difference,
# never a write, and with a read-only token (no bypass actors shown) the rest is still compared.
out=$(APPLY=--check apply env FAKE_MAIN=same FAKE_TAGS=1) || fail "--check refused matching rulesets: $out"
[ "$(writes)" = 0 ] || fail "--check wrote: $(cat "$T/gh.log")"
ok "apply-rulesets.sh --check: matching rulesets pass"
out=$(APPLY=--check apply env FAKE_MAIN=same FAKE_TAGS=1 FAKE_READONLY=1) || fail "--check with a read-only token refused matching rulesets: $out"
printf '%s' "$out" | grep -qF "bypass actors aren't visible to this token" || fail "--check didn't say it skipped the bypass actors: $out"
ok "apply-rulesets.sh --check, read-only token: bypass actors skipped (and said so), the rest matches"
if out=$(APPLY=--check apply env FAKE_TAGS=1 FAKE_READONLY=1); then fail "--check accepted the live ruleset that requires two checks: $out"; fi
[ "$(writes)" = 0 ] || fail "--check wrote: $(cat "$T/gh.log")"
printf '%s' "$out" | grep -qE '^\+ +"context": "Clippy",?$' || fail "--check doesn't show the difference: $out"
ok "apply-rulesets.sh --check: a live ruleset missing required checks fails the job, nothing written"
jq '.enforcement = "disabled"' deploy/github/ruleset-tags.json > "$T/tags-off.json"
if out=$(APPLY=--check apply env FAKE_MAIN=same FAKE_TAGS=1 FAKE_TAGS_FILE="$T/tags-off.json" FAKE_READONLY=1); then fail "--check accepted a disabled tag ruleset: $out"; fi
ok "apply-rulesets.sh --check: a ruleset switched off fails the job"
if out=$(APPLY=--check apply env FAKE_MAIN=same); then fail "--check accepted a missing ruleset: $out"; fi
ok "apply-rulesets.sh --check: a missing ruleset fails the job"
echo "✓ github settings: $pass cases"
