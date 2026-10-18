#!/usr/bin/env bash
# The public testnet, watched from outside Fly: .github/workflows/testnet-watch.yml runs this on a
# GitHub runner about every 10 minutes (deploy/README.md "External watch"). Each check is a line
# of the report:
#   health                /api/health answers HTTP 200 with ok: true
#   chain id              /api/status reports CHAIN_ID
#   finality              its finalized height advances across WAIT_SECS (two reads)
#   head age              its head is at most MAX_HEAD_AGE_MS old
#   active validators     at least EXPECT_VALIDATORS
#   jailed or tombstoned  /api/validators lists none (the report counts each)
# No answer, a timeout or an answer that isn't JSON fails the check that needed it.
#
# The alert is a GitHub issue labeled testnet-alert. When a check fails (or TEST_ALERT=true), the
# open one gets the report as a comment, but while the same checks keep failing only every
# REMIND_SECS (a hidden marker in each report records which checks failed and when), since every
# comment notifies the maintainer. With none open, one is opened (the label created if missing)
# with the report, the time, the run's URL and an @-mention of the maintainer, so GitHub emails
# and pushes it to him. When every check passes, the open one gets a "Recovered" comment and is
# closed. A test alert is opened (or commented) like a real one and says it's a test; it stays
# open until the next run that passes every check closes it, which exercises the recovery path
# with the same token.
#
# Exit status: 0 when every check passes or the alert was delivered (the issue is the alert; a red
# run as well would be a second, noisier channel); 1 when a gh call failed, so a broken alert
# path shows as a failed run; 2 on bad input. Needs curl, jq and gh (GH_TOKEN: issues: write).
#   scripts/testnet-watch.sh
#   DRY_RUN=true scripts/testnet-watch.sh   # prints the gh calls instead of making them (no gh
#                                           # needed; it assumes no alert issue is open)
# Environment: RPC, CHAIN_ID, EXPECT_VALIDATORS, MAX_HEAD_AGE_MS, TEST_ALERT, DRY_RUN, GH (the gh
# command; the tests use a stand-in), WAIT_SECS, CURL_TIMEOUT, CURL_RETRIES, MENTION, REMIND_SECS.
set -euo pipefail

RPC=${RPC:-https://blockula-testnet.fly.dev}
RPC=${RPC%/}
CHAIN_ID=${CHAIN_ID:-blockula-testnet-2}
EXPECT_VALIDATORS=${EXPECT_VALIDATORS:-4}
MAX_HEAD_AGE_MS=${MAX_HEAD_AGE_MS:-60000}
TEST_ALERT=${TEST_ALERT:-false} # empty on scheduled runs: the input exists on manual ones only
DRY_RUN=${DRY_RUN:-false}
GH=${GH:-gh}
WAIT_SECS=${WAIT_SECS:-30}
CURL_TIMEOUT=${CURL_TIMEOUT:-15} # seconds per attempt
CURL_RETRIES=${CURL_RETRIES:-2}
MENTION=${MENTION:-@jackdishman}
REMIND_SECS=${REMIND_SECS:-3600} # while the same checks keep failing, a comment at most this often
LABEL=testnet-alert

usage() { echo "✗ testnet-watch.sh: $*" >&2; exit 2; }
for v in EXPECT_VALIDATORS MAX_HEAD_AGE_MS WAIT_SECS CURL_TIMEOUT CURL_RETRIES REMIND_SECS; do
  [[ ${!v} =~ ^[0-9]+$ ]] || usage "$v must be a whole number (got '${!v}')"
done
[ "$CURL_TIMEOUT" -gt 0 ] || usage "CURL_TIMEOUT must be at least 1"
case $TEST_ALERT in true | false) ;; *) usage "TEST_ALERT must be true or false (got '$TEST_ALERT')" ;; esac
case $DRY_RUN in true | false) ;; *) usage "DRY_RUN must be true or false (got '$DRY_RUN')" ;; esac
for c in curl jq; do command -v "$c" > /dev/null || usage "needs $c"; done

T=$(mktemp -d "${TMPDIR:-/tmp}/testnet-watch.XXXXXX")
trap 'rm -rf "$T"' EXIT
exec 3>&1 # the log, while a gh call's output goes to a file
# gh acts on the repository the run belongs to, whatever the checkout's remote says.
if [ -n "${GITHUB_REPOSITORY:-}" ] && [ -z "${GH_REPO:-}" ]; then export GH_REPO=$GITHUB_REPOSITORY; fi
NOW=$(date -u '+%Y-%m-%d %H:%M:%S UTC')
NOW_S=$(date -u +%s)
RUN_URL=""
if [ -n "${GITHUB_SERVER_URL:-}" ] && [ -n "${GITHUB_REPOSITORY:-}" ] && [ -n "${GITHUB_RUN_ID:-}" ]; then
  RUN_URL=$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID
fi

# clean TEXT [MAX]: text from the node (or curl) made safe to quote in the issue: one line of at
# most MAX characters (200), no backtick to end the report's code block, no @ to mention anyone.
clean() {
  printf '%s' "$1" | jq -Rrs --argjson max "${2:-200}" 'gsub("[[:cntrl:]`]+"; " ") | gsub("@"; "(at)") | sub("^ +"; "") | sub(" +$"; "") | .[0:$max]'
}
# jqr FILTER NAME: FILTER over the answer in $T/NAME.json (nothing if it can't be read that way).
jqr() { jq -r "$1" "$T/$2.json" 2> /dev/null || true; }
# num NAME KEY: the answer's KEY if it's a whole number, else nothing.
num() { jqr ".$2 | select(type == \"number\" and . >= 0 and . == floor)" "$1"; }

# fetch NAME PATH: GET $RPC$PATH into $T/NAME.json. Sets CODE (the HTTP status, 000 without one)
# and PROBLEM: why there's no JSON answer to read (no answer, a timeout, not JSON), or nothing.
# curl retries (CURL_RETRIES times) a refused connection, a timeout and HTTP 408, 429, 500 and
# 502-504; only the last attempt's answer is kept.
fetch() {
  local rc=0 err
  CODE=$(curl -sS -o "$T/$1.json" -w '%{http_code}' --connect-timeout 5 --max-time "$CURL_TIMEOUT" \
    --retry "$CURL_RETRIES" --retry-delay 2 --retry-max-time 60 --retry-connrefused \
    -H 'Accept: application/json' "$RPC$2" 2> "$T/$1.err") || rc=$?
  PROBLEM=""
  if [ "$rc" != 0 ]; then
    err=$(tail -n 1 "$T/$1.err" | sed -E 's/^curl: \([0-9]+\) //' || true)
    PROBLEM="no answer (curl exit $rc: $(clean "$err"))"
  elif ! jq -e 'type == "object"' "$T/$1.json" > /dev/null 2>&1; then
    PROBLEM="HTTP $CODE, not a JSON answer"
  fi
}

REPORT="" FAILURES=0 FIRST="" FIRST_NAME=""
# check ok|fail NAME DETAIL: a line of the report.
check() {
  if [ "$1" = ok ]; then
    REPORT+="✓ $2: $3"$'\n'
  else
    REPORT+="✗ $2: $3"$'\n'
    FAILURES=$((FAILURES + 1))
    if [ -z "$FIRST" ]; then FIRST="$2: $3" FIRST_NAME=$2; fi
  fi
}

# ---- the checks ----

fetch health /api/health
if [ -n "$PROBLEM" ]; then
  check fail health "$PROBLEM"
elif [ "$CODE" = 200 ] && [ "$(jqr '.ok == true' health)" = true ]; then
  check ok health "HTTP 200, ok"
else
  check fail health "HTTP $CODE, not ok: $(clean "$(jqr '.reason // "no reason given" | tostring' health)")"
fi

fetch status1 /api/status
P1=$PROBLEM
if [ -z "$P1" ] && [ "$CODE" != 200 ]; then P1="HTTP $CODE"; fi
sleep "$WAIT_SECS"
fetch status2 /api/status
P2=$PROBLEM
if [ -z "$P2" ] && [ "$CODE" != 200 ]; then P2="HTTP $CODE"; fi
# The newest status that answered is what the chain id, head age and validator checks read.
if [ -z "$P2" ]; then S=status2; elif [ -z "$P1" ]; then S=status1; else S=""; fi

if [ -z "$S" ]; then
  check fail "chain id" "/api/status: $P2"
else
  got=$(jqr '.chain_id // "none" | tostring' "$S")
  if [ "$got" = "$CHAIN_ID" ]; then check ok "chain id" "$CHAIN_ID"
  else check fail "chain id" "$(clean "$got"), expected $CHAIN_ID"; fi
fi

if [ -n "$P1" ]; then
  check fail finality "/api/status: $P1"
elif [ -n "$P2" ]; then
  check fail finality "/api/status ${WAIT_SECS} s later: $P2"
else
  f1=$(num status1 finalized_height)
  f2=$(num status2 finalized_height)
  if [ -z "$f1" ] || [ -z "$f2" ]; then check fail finality "/api/status has no finalized_height"
  elif [ "$f2" -gt "$f1" ]; then check ok finality "finalized height $f1 → $f2 in ${WAIT_SECS} s"
  else check fail finality "finalized height $f1 → $f2 in ${WAIT_SECS} s: not advancing"; fi
fi

if [ -z "$S" ]; then
  check fail "head age" "/api/status: $P2"
  check fail "active validators" "/api/status: $P2"
else
  age=$(num "$S" head_age_ms)
  if [ -z "$age" ]; then check fail "head age" "no head (head_age_ms: $(clean "$(jqr '.head_age_ms | tojson' "$S")"))"
  elif [ "$age" -le "$MAX_HEAD_AGE_MS" ]; then check ok "head age" "$age ms (limit $MAX_HEAD_AGE_MS ms)"
  else check fail "head age" "$age ms, over the $MAX_HEAD_AGE_MS ms limit"; fi
  active=$(num "$S" active_validators)
  if [ -z "$active" ]; then check fail "active validators" "/api/status has no active_validators"
  elif [ "$active" -ge "$EXPECT_VALIDATORS" ]; then check ok "active validators" "$active (at least $EXPECT_VALIDATORS expected)"
  else check fail "active validators" "$active, expected at least $EXPECT_VALIDATORS"; fi
fi

fetch validators /api/validators
if [ -n "$PROBLEM" ]; then
  check fail "jailed or tombstoned" "/api/validators: $PROBLEM"
elif [ "$CODE" != 200 ] || [ "$(jqr '(.jailed | type) == "array" and (.tombstoned | type) == "array"' validators)" != true ]; then
  check fail "jailed or tombstoned" "/api/validators: HTTP $CODE, without its jailed and tombstoned lists"
else
  jailed=$(jqr '.jailed | length' validators)
  tombstoned=$(jqr '.tombstoned | length' validators)
  if [ "$jailed" = 0 ] && [ "$tombstoned" = 0 ]; then
    check ok "jailed or tombstoned" "0 jailed, 0 tombstoned"
  else
    who=$(jqr 'def who: (.validator? | if type == "object" then .short // .address else . end) // "?" | tostring;
      [(.jailed[] | "\(who) jailed (\(.jailed.reason? // "no reason given")) until height \(.jailed.until? // "?")"),
       (.tombstoned[] | "\(who) tombstoned at height \(.tombstoned_at? // "?")")] | join("; ")' validators)
    check fail "jailed or tombstoned" "$jailed jailed, $tombstoned tombstoned: $(clean "$who" 600)"
  fi
fi

echo "Testnet watch of $RPC (expecting $CHAIN_ID) at $NOW"
printf '%s' "$REPORT"

# ---- the alert issue ----

# delivery_failed WHY: the alert can't be delivered; a failed run is the signal left. (It may run
# while a gh call's output goes to a file: the annotation goes to the log, fd 3.)
delivery_failed() {
  echo "✗ alert delivery failed: $1" >&2
  if [ "${GITHUB_ACTIONS:-}" = true ]; then echo "::error title=Testnet watch::alert delivery failed; the report is in this run's log" >&3; fi
  exit 1
}
# gh_run ARGS…: $GH ARGS, its output on stdout; a failure ends the run (delivery_failed). With
# DRY_RUN it prints the call (and any body it would post) instead, and the lookup finds nothing.
gh_run() {
  local a prev=""
  if [ "$DRY_RUN" = true ]; then
    {
      printf 'DRY_RUN: gh'
      for a in "$@"; do case $a in *[[:space:]]* | "") printf ' "%s"' "$a" ;; *) printf ' %s' "$a" ;; esac; done
      echo
      for a in "$@"; do
        if [ "$prev" = --body-file ]; then sed 's/^/  | /' "$a"; fi
        prev=$a
      done
    } >&3
    if [ "$1 $2" = "issue list" ]; then echo '[]'; fi
    if [ "$1 $2" = "issue view" ]; then echo '{"body":"","comments":[]}'; fi
    return 0
  fi
  if ! "$GH" "$@" 2> "$T/gh.err"; then delivery_failed "gh $1 $2: $(tail -n 3 "$T/gh.err" | tr '\n' ' ')"; fi
}
said() { if [ "$DRY_RUN" = true ]; then echo "→ DRY_RUN, nothing sent: would have $1"; else echo "→ $1"; fi; }
# The report as the issue shows it, then the time, the run and the node.
facts() {
  printf '```text\n%s```\n\n' "$REPORT"
  echo "- Time: $NOW"
  if [ -n "$RUN_URL" ]; then echo "- Run: $RUN_URL"; fi
  echo "- Node: $RPC (expecting \`$CHAIN_ID\`)"
  echo "<!-- testnet-watch failing=$SIG at=$NOW_S -->"
}

# The alert issue: the oldest open one labeled testnet-alert, if any.
gh_run issue list --label "$LABEL" --state open --json number --limit 100 > "$T/open.json"
if ! OPEN=$(jq -r 'map(.number) | min // empty' "$T/open.json" 2> /dev/null); then
  delivery_failed "gh issue list didn't answer with a list of issues: $(head -c 300 "$T/open.json")"
fi
# Which checks fail, as one comparable word (the names, sorted, joined by commas).
SIG=$(printf '%s' "$REPORT" | sed -n 's/^✗ \([^:]*\):.*/\1/p' | tr ' ' '_' | sort | paste -sd, -)
TEST_LEAD="this is a **test of alert delivery**, from a manual run of \`testnet-watch.yml\` with \`test_alert\`"
FAILING="**$FIRST_NAME** is the first failing check."
if [ "$TEST_ALERT" = true ]; then FAILING+=" This run was also a test of alert delivery (\`test_alert\`)."; fi

if [ "$FAILURES" -gt 0 ] || [ "$TEST_ALERT" = true ]; then
  if [ -n "$OPEN" ] && [ "$TEST_ALERT" != true ]; then
    # The newest report this watch left on the issue: the same checks failing, less than
    # REMIND_SECS ago, needs no new comment (every comment notifies the maintainer).
    gh_run issue view "$OPEN" --json body,comments > "$T/issue.json"
    last=$(jq -r '[.body // "", (.comments // [] | .[].body // "")] | map(capture("<!-- testnet-watch failing=(?<sig>[^ ]*) at=(?<at>[0-9]+) -->")) | last // empty | "\(.sig) \(.at)"' "$T/issue.json" 2> /dev/null || true)
    if [ -n "$last" ] && [ "${last% *}" = "$SIG" ] && [ $((NOW_S - ${last##* })) -lt "$REMIND_SECS" ]; then
      echo "→ still failing ($SIG), as reported on alert issue #$OPEN under ${REMIND_SECS} s ago: no new comment"
      exit 0
    fi
  fi
  if [ -n "$OPEN" ]; then
    if [ "$FAILURES" = 0 ]; then
      intro="$MENTION $TEST_LEAD: every check passes. If this comment reached you as an email or a push notification, testnet alerts reach you. The next scheduled run that passes every check closes this issue."
    elif [ "$TEST_ALERT" = true ]; then
      intro="$MENTION failing: $FAILING"
    else
      intro="Failing: $FAILING"
    fi
    { echo "$intro"; echo; facts; } > "$T/body.md"
    gh_run issue comment "$OPEN" --body-file "$T/body.md" > /dev/null
    said "commented on alert issue #$OPEN"
  else
    if [ "$FAILURES" = 0 ]; then
      title="Testnet alert: test of alert delivery"
      intro="$MENTION $TEST_LEAD. Every check passes; nothing needs doing. If this reached you as an email or a push notification, testnet alerts reach you."
      outro="The next scheduled run that passes every check comments \"Recovered\" and closes this issue (which tests the recovery path too)."
    else
      title="Testnet alert: $(clean "$FIRST")"
      intro="$MENTION the external watch found the public testnet failing: $FAILING"
      outro="While a check fails, each run (about every 10 minutes) adds its report here as a comment; the first run that passes every check comments \"Recovered\" and closes this issue. Runbooks: \`docs/OPERATIONS.md\` \"Runbooks\"; the testnet's day-2 tasks: \`deploy/README.md\` \"Day 2\"."
    fi
    { echo "$intro"; echo; facts; echo; echo "$outro"; } > "$T/body.md"
    gh_run label create "$LABEL" --color B60205 --description "Opened and closed by the external testnet watch (testnet-watch.yml)" --force > /dev/null
    gh_run issue create --title "$title" --label "$LABEL" --body-file "$T/body.md" > "$T/created"
    said "opened an alert issue$(sed 's/^/: /' "$T/created")"
  fi
elif [ -n "$OPEN" ]; then
  { echo "Recovered: every check passes."; echo; facts; } > "$T/body.md"
  gh_run issue comment "$OPEN" --body-file "$T/body.md" > /dev/null
  gh_run issue close "$OPEN" > /dev/null
  said "recovered: commented on and closed alert issue #$OPEN"
else
  echo "→ every check passes; no alert issue is open"
fi
