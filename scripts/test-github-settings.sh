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

# Named edits for the YAML-spelling variants (round 4): each replaces text that must be there
# exactly once, so an edit can't quietly stop applying.
cat > "$T/edits.cjs" <<'JS'
const rep = (s, a, b) => {
  const n = s.split(a).length - 1;
  if (n !== 1) throw new Error(`expected one ${JSON.stringify(a)}, found ${n}`);
  return s.replace(a, () => b);
};
const HEAD = "    name: Clippy\n    if: github.event_name != 'schedule'\n";
const CHECK = "      - run: scripts/clippy.sh\n";
// The Clippy job's text, edited by f.
const clippy = (s, f) => {
  const a = s.indexOf("\n  clippy:\n"), b = s.indexOf("\n\n  deps:\n", a);
  if (a < 0 || b < 0) throw new Error("no clippy job");
  return s.slice(0, a) + f(s.slice(a, b)) + s.slice(b);
};
// Clippy's steps as an indentless sequence (GitHub's starter-workflow style), check step edited.
const compact = (s, after) => clippy(s, (j) => {
  const [head, steps] = j.split("    steps:\n");
  return head + "    steps:\n" + rep(steps.replace(/^  /gm, ""), "    - run: scripts/clippy.sh", "    - run: scripts/clippy.sh" + after);
});
// An optional job that fails, for Clippy to need.
const lintprep = (s) => rep(rep(s, "\n  deps:\n", "\n  lintprep:\n    name: Lint prep\n    runs-on: ubuntu-24.04\n    steps:\n      - run: exit 1\n\n  deps:\n"), "#   required: Fuzz harness\n", "#   required: Fuzz harness\n#   optional: Lint prep\n");
const job = (lines) => "\n  sneaky:\n" + lines.map((l) => `    ${l}\n`).join("") + "    runs-on: ubuntu-24.04\n    steps:\n      - run: \"true\"\n";
module.exports = {
  // The round-3 reviewer's (supply-chain-B-3): an indentless step list, quoted keys.
  indentlessStepIf: (s) => compact(s, "\n      if: github.ref == 'refs/heads/main'"),
  quotedStepIf: (s) => rep(s, CHECK, CHECK + "        \"if\": github.ref == 'refs/heads/main'\n"),
  quotedJobIf: (s) => rep(s, HEAD, "    name: Clippy\n    \"if\": github.ref == 'refs/heads/main'\n"),
  quotedJobContinue: (s) => rep(s, HEAD, HEAD + "    'continue-on-error': true\n"),
  quotedNameElsewhere: (s) => s + job(["\"name\": Clippy"]),
  computedNameElsewhere: (s) => s + job(["name: ${{ 'Clippy' }}"]),
  // review-r3-supply-1: a two-line if, compact and multi-line needs, compact steps, a wide item.
  twoLineIf: (s) => rep(s, HEAD, HEAD + "      && github.event_name != 'pull_request'\n"),
  compactNeeds: (s) => lintprep(rep(s, "    name: Clippy\n", "    name: Clippy\n    needs:\n    - lintprep\n")),
  flowNeedsTwoLines: (s) => lintprep(rep(s, "    name: Clippy\n", "    name: Clippy\n    needs: [deps,\n      lintprep]\n")),
  compactSteps: (s) => compact(s, "\n      if: github.event_name == 'push'\n      continue-on-error: true"),
  wideStepItem: (s) => rep(s, CHECK, "      -   run: scripts/clippy.sh\n          if: false\n"),
  // More spellings GitHub reads: folded scalars, aliases, key case; and what can't be read.
  foldedIf: (s) => rep(s, HEAD, "    name: Clippy\n    if: >-\n      github.event_name != 'schedule' &&\n      github.event_name != 'pull_request'\n"),
  aliasIf: (s) => rep(rep(s, "env:\n  CARGO_TERM_COLOR: always\n", "env:\n  CARGO_TERM_COLOR: always\n  MAIN_ONLY: &main-only github.ref == 'refs/heads/main'\n"), HEAD, "    name: Clippy\n    if: *main-only\n"),
  capitalIf: (s) => rep(s, HEAD, "    name: Clippy\n    If: github.ref == 'refs/heads/main'\n"),
  mergeKey: (s) => rep(s, HEAD, HEAD + "    <<: { continue-on-error: true }\n"),
  taggedMergeKey: (s) => rep(s, HEAD, HEAD + "    !!merge <<: { continue-on-error: true }\n"),
  taggedIf: (s) => rep(s, HEAD, "    name: Clippy\n    if: !!str github.event_name != 'schedule'\n"),
  duplicateIf: (s) => rep(s, HEAD, HEAD + "    if: github.ref == 'refs/heads/main'\n"),
  secondDocument: (s) => s + "---\njobs:\n  clippy:\n    if: false\n",
  flowStepIf: (s) => rep(s, CHECK, "      - { run: scripts/clippy.sh,\n          if: false }\n"),
  booleanStepIf: (s) => rep(s, CHECK, CHECK + "        if: false\n"),
  needsMap: (s) => rep(s, "    name: Clippy\n", "    name: Clippy\n    needs: { job: deps }\n"),
  caseNameElsewhere: (s) => s + job(["name: clippy"]),
  matrixNameElsewhere: (s) => s + job(["name: ${{ matrix.n }}", "strategy:", "  matrix:", "    n: [Clippy]"]),
  reusableRequired: (s) => clippy(s, (j) => j.split("    runs-on:")[0] + "    uses: ./.github/workflows/clippy.yml\n"),
  matrixRequired: (s) => rep(s, HEAD, HEAD + "    strategy:\n      matrix:\n        os: [a]\n"),
  typesFilter: (s) => rep(s, "  pull_request:\n", "  pull_request:\n    types: [labeled]\n"),
  prOnlyJob: (s) => s + "\n  docs:\n    name: Lint docs\n    if: github.event_name == 'pull_request'\n    runs-on: ubuntu-24.04\n    steps:\n      - run: \"true\"\n",
  // A check step renamed to a reviewed exception's name isn't one.
  checkAsTraces: (s) => rep(s, "      - run: npm run test:e2e -w @blockula/web\n", "      - name: Traces of failed tests\n        if: failure()\n        run: npm run test:e2e -w @blockula/web\n"),
  auditAsNonBlocking: (s) => rep(s, "      - name: npm audit (runtime dependencies)\n        run:", "      - name: npm audit (all, non-blocking)\n        continue-on-error: true\n        run:"),
  // Allowed: the same jobs in other spellings (the parser reads them; nothing changes).
  restyled: (s) => rep(compact(s, ""), HEAD, "    \"name\": Clippy\n    'if': >-\n      ${{ github.event_name\n      != 'schedule' }}\n"),
};
JS

# variant NAME EXPECTED FILE EDIT: copies the workflows and the ruleset, edits FILE (relative to
# the copy: wf/<name>.yml or ruleset.json) with EDIT (sed -E; "jq:<filter>" for the ruleset;
# "js:<function of the text>"; or "jsf:<name of an edit in edits.cjs>"), and expects the check to
# fail saying EXPECTED.
variant() {
  local name=$1 want=$2 file=$3 edit=$4 out
  rm -rf "$T/v" && mkdir -p "$T/v/wf"
  cp .github/workflows/*.yml "$T/v/wf/"
  cp deploy/github/ruleset-main.json "$T/v/ruleset.json"
  cp "$T/v/$file" "$T/v/before"
  case $edit in
    jq:*) jq "${edit#jq:}" "$T/v/before" > "$T/v/$file" ;;
    js:*) node -e 'const fs = require("fs"); fs.writeFileSync(process.argv[2], eval(process.argv[3])(fs.readFileSync(process.argv[1], "utf8")))' "$T/v/before" "$T/v/$file" "${edit#js:}" ;;
    jsf:*) node -e 'const fs = require("fs"); fs.writeFileSync(process.argv[2], require(process.argv[4])[process.argv[3]](fs.readFileSync(process.argv[1], "utf8")))' "$T/v/before" "$T/v/$file" "${edit#jsf:}" "$T/edits.cjs" ;;
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
variant "a job written in flow style (read as GitHub reads it)" '"Lint docs" (crypto.yml: docs) runs on every pull request but isn'"'"'t a required check' wf/crypto.yml '$a\
  docs: { name: Lint docs, runs-on: ubuntu-24.04, steps: [{ run: "true" }] }'
variant "a push-triggered job reusing a required name" 'the required check "Clippy" could be reported by 2 jobs' wf/release.yml 's/^    name: Tag is the crate version$/    name: Clippy/'

# Round 4 (review-r3-supply-1, supply-chain-B-3's round-3 review): the checker read one spelling
# of YAML and skipped what it couldn't place, so valid YAML that GitHub (and actionlint) read as an
# if:, a continue-on-error, a needs or a name got past it. Each spelling below is refused now.
# The round-3 reviewer's: an indentless step list, quoted step and job keys, quoted or computed
# names on a push-triggered job.
variant "an indentless step list with an if: on the check step" 'its step "(unnamed)" has if: github.ref == '"'"'refs/heads/main'"'" wf/ci.yml jsf:indentlessStepIf
variant "a quoted \"if\" on the check step" 'its step "(unnamed)" has if: github.ref == '"'"'refs/heads/main'"'" wf/ci.yml jsf:quotedStepIf
variant "a quoted \"if\" on a required job" "its if: (github.ref == 'refs/heads/main') can skip it" wf/ci.yml jsf:quotedJobIf
variant "a quoted 'continue-on-error' on a required job" "has continue-on-error: true" wf/ci.yml jsf:quotedJobContinue
variant "a push-triggered job with a quoted \"name\" reusing a required one" 'the required check "Clippy" could be reported by 2 jobs' wf/release.yml jsf:quotedNameElsewhere
variant "a push-triggered job whose computed name is a required one" 'the required check "Clippy" could be reported by 2 jobs' wf/release.yml jsf:computedNameElsewhere
# review-r3-supply-1's five: a two-line if:, a compact needs list, a flow needs list over two
# lines, compact steps with if/continue-on-error, a step item with extra spaces after "-".
variant "a two-line if: that starts with the allowed condition" "its if: (github.event_name != 'schedule' && github.event_name != 'pull_request') can skip it" wf/ci.yml jsf:twoLineIf
variant "a compact needs: list naming an optional job" 'it needs "Lint prep" (ci.yml: lintprep), which isn'"'"'t' wf/ci.yml jsf:compactNeeds
variant "a flow needs: list over two lines naming an optional job" 'it needs "Lint prep" (ci.yml: lintprep), which isn'"'"'t' wf/ci.yml jsf:flowNeedsTwoLines
variant "compact steps with if: and continue-on-error on the check step" "has if: github.event_name == 'push'" wf/ci.yml jsf:compactSteps
variant "a step item written \"-   run:\" with if: false" 'its step "(unnamed)" has if: false' wf/ci.yml jsf:wideStepItem
# More spellings: a folded scalar, an alias, a capitalised key, a flow step over two lines, a
# boolean if.
variant "a folded (>-) if: on a required job" "its if: (github.event_name != 'schedule' && github.event_name != 'pull_request') can skip it" wf/ci.yml jsf:foldedIf
variant "an if: taken from an anchor elsewhere (*alias)" "its if: (github.ref == 'refs/heads/main') can skip it" wf/ci.yml jsf:aliasIf
variant "a capitalised If: on a required job" "its if: (github.ref == 'refs/heads/main') can skip it" wf/ci.yml jsf:capitalIf
variant "a flow-style check step over two lines with if: false" 'its step "(unnamed)" has if: false' wf/ci.yml jsf:flowStepIf
variant "if: false on the check step" 'its step "(unnamed)" has if: false' wf/ci.yml jsf:booleanStepIf
# What it can't read as GitHub does is refused, not skipped.
variant "a merge key (<<) on a required job" "a merge key (<<)" wf/ci.yml jsf:mergeKey
variant "a merge key tagged !!merge (the parser would merge it)" "a merge key (<<)" wf/ci.yml jsf:taggedMergeKey
variant "an explicit tag, even on the allowed condition" "an explicit YAML tag (tag:yaml.org,2002:str)" wf/ci.yml jsf:taggedIf
variant "a duplicate if: key" "ci.yml can't be read the way GitHub reads it" wf/ci.yml jsf:duplicateIf
variant "a second YAML document" "2 YAML documents" wf/ci.yml jsf:secondDocument
variant "a needs: that isn't a job id or a list" "its needs: ({\"job\":\"deps\"}) isn't a job id or a list of them" wf/ci.yml jsf:needsMap
# Names: other letter case, a matrix value; a required job that reports under another name.
variant "a push-triggered job named like a required one in lower case" 'the required check "Clippy" could be reported by 2 jobs' wf/release.yml jsf:caseNameElsewhere
variant "a push-triggered job whose matrix makes a required name" 'the required check "Clippy" could be reported by 2 jobs' wf/release.yml jsf:matrixNameElsewhere
variant "a required job calling a reusable workflow" "it calls a reusable workflow" wf/ci.yml jsf:reusableRequired
variant "a required job with a matrix" 'it has a matrix: its checks are named "Clippy (…)"' wf/ci.yml jsf:matrixRequired
# Triggers and conditions: a pull_request types filter; an if: that names pull_request but runs on them.
variant "a types filter on a required job's workflow" "runs on pull_request only with types" wf/ci.yml jsf:typesFilter
variant "a job that runs only on pull requests, not required" '"Lint docs" (crypto.yml: docs) runs on every pull request but isn'"'"'t a required check' wf/crypto.yml jsf:prOnlyJob
# A check step renamed to a reviewed exception's name, with its condition: still refused (the
# exception binds what the step runs).
variant "the e2e tests renamed to the reviewed traces upload, if: failure()" 'its step "Traces of failed tests" has if: failure()' wf/ci.yml jsf:checkAsTraces
variant "the blocking npm audit renamed to the non-blocking one" 'its step "npm audit (all, non-blocking)" has continue-on-error: true' wf/ci.yml jsf:auditAsNonBlocking
# The safe forms still pass: the allowed job condition in ${{ }}, and the reviewed step exceptions.
rm -rf "$T/v" && mkdir -p "$T/v/wf" && cp .github/workflows/*.yml "$T/v/wf/"
sed -E '/^    name: Clippy$/{n;s/^    if: .*/    if: ${{ github.event_name != '"'"'schedule'"'"' }}/;}' .github/workflows/ci.yml > "$T/v/wf/ci.yml"
cmp -s .github/workflows/ci.yml "$T/v/wf/ci.yml" && fail "the \${{ }} edit no longer changes ci.yml"
node "$CHECK" --workflows "$T/v/wf" > /dev/null || fail "the check refuses the allowed condition written as \${{ … }}"
grep -qF "if: failure()" .github/workflows/ci.yml && grep -qF "continue-on-error: true" .github/workflows/ci.yml || fail "the reviewed step exceptions are gone from ci.yml: update SAFE_STEPS"
ok "allowed: the reviewed job condition (also as \${{ … }}) and step exceptions"
# The same jobs in other spellings (an indentless step list, quoted keys, a folded ${{ }}
# condition) are read as what they are, and pass.
rm -rf "$T/v" && mkdir -p "$T/v/wf" && cp .github/workflows/*.yml "$T/v/wf/"
node -e 'const fs = require("fs"); fs.writeFileSync(process.argv[2], require(process.argv[3]).restyled(fs.readFileSync(process.argv[1], "utf8")))' .github/workflows/ci.yml "$T/v/wf/ci.yml" "$T/edits.cjs"
cmp -s .github/workflows/ci.yml "$T/v/wf/ci.yml" && fail "the restyling edit no longer changes ci.yml"
node "$CHECK" --workflows "$T/v/wf" > /dev/null || fail "the check refuses Clippy restyled (indentless steps, quoted keys, a folded condition): $(node "$CHECK" --workflows "$T/v/wf" 2>&1)"
ok "allowed: the same jobs restyled (indentless steps, quoted keys, a folded \${{ }} condition)"

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
