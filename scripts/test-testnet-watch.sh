#!/usr/bin/env bash
# Offline tests of scripts/testnet-watch.sh (CI `ops` job): its checks against a mock node
# (python3 -I serving canned JSON on a random localhost port, switched between healthy, stalled,
# wrong-chain, jailed, tombstoned, down, hanging, flaky and hostile answers) and its alert issue
# against a stand-in gh that records every call and the bodies it would post. Nothing here talks
# to the testnet or to GitHub.
#
#   bash scripts/test-testnet-watch.sh
set -euo pipefail
cd "$(dirname "$0")/.."
T=$(mktemp -d "${TMPDIR:-/tmp}/blockula-watch-test.XXXXXX")
MOCK=""
trap 'if [ -n "$MOCK" ]; then kill "$MOCK" 2> /dev/null || true; wait "$MOCK" 2> /dev/null || true; fi; rm -rf "$T"' EXIT
fails=0
ok() { echo "✓ $1"; }
bad() { echo "✗ $1" >&2; fails=$((fails + 1)); }
expect() { # what haystack needle
  if printf '%s\n' "$2" | grep -qF -- "$3"; then ok "$1"; else bad "$1: no \"$3\" in: $2"; fi
}
refute() { # what haystack needle
  if printf '%s\n' "$2" | grep -qF -- "$3"; then bad "$1: \"$3\" in: $2"; else ok "$1"; fi
}

# ---- the mock node: /api/health, /api/status and /api/validators as $T/state says ----

cat > "$T/mock.py" <<'PY'
import json, os, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE, PORT_FILE, LOG = sys.argv[1], sys.argv[2], sys.argv[3]
finalized = [1000]  # rises 3 on every /api/status read, unless stalled
reads = {}  # (state, path) -> how many reads
HOSTILE = "@everyone\n```\n[click](https://example.invalid)\n::error::injected"

def validator(short, **fields):
    v = {"validator": {"address": "tboo1" + "q" * 58, "name": "\U0001f9db validator", "short": short}, "status": "active", "jailed": None, "tombstoned_at": None}
    v.update(fields)
    return v

class Node(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def answer(self, code, body, ctype="application/json"):
        data = (body if isinstance(body, str) else json.dumps(body)).encode()
        with open(LOG, "a") as f:  # what each read was answered, for the tests to check
            f.write("%s %s %d\n" % (self.state, self.path, code))
        try:
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except OSError:  # the client gave up (the timeout case)
            pass

    def do_GET(self):
        with open(STATE) as f:
            state = self.state = f.read().strip()
        path = self.path.split("?")[0]
        n = reads[(state, path)] = reads.get((state, path), 0) + 1
        if state == "hang":
            time.sleep(3)
        if state == "down":  # what Fly's proxy answers when the app has no machine running
            return self.answer(502, "<html><body><h1>502 Bad Gateway</h1></body></html>", "text/html")
        stalled = state == "stalled"
        age = 75000 if stalled else 800
        if path == "/api/health":
            if state == "flaky" and n % 2 == 1:  # every other read, a proxy error
                return self.answer(503, "<html>upstream connect error</html>", "text/html")
            sick = {"stalled": "head is 75s old", "hostile": HOSTILE}.get(state)
            return self.answer(503 if sick else 200, {"ok": not sick, "reason": sick, "halted": None, "height": finalized[0], "head_age_ms": age, "finalized_height": finalized[0]})
        if path == "/api/status":
            if not stalled:
                finalized[0] += 3
            chain = "blockula-testnet" if state == "wrong-chain" else "blockula-testnet-2"
            return self.answer(200, {"chain_id": chain, "height": finalized[0], "finalized_height": finalized[0], "head_age_ms": age, "active_validators": 4, "epoch": 0})
        if path == "/api/validators":
            jailed, tombstoned = [], []
            if state in ("jailed", "hostile"):
                reason = HOSTILE if state == "hostile" else "missed 51 of its last 100 leader slots"
                jailed = [validator("tboo1kmq79…xx25dx", jailed={"at": 891, "until": 4491, "reason": reason})]
            if state == "tombstoned":
                tombstoned = [validator("tboo1ds4xw…k2gdxh", status="tombstoned", tombstoned_at=1200)]
            return self.answer(200, {"epoch": 0, "active": [], "jailed": jailed, "tombstoned": tombstoned})
        return self.answer(404, {"error": "not found"})

server = ThreadingHTTPServer(("127.0.0.1", 0), Node)
server.daemon_threads = True
with open(PORT_FILE + ".tmp", "w") as f:
    f.write(str(server.server_address[1]))
os.rename(PORT_FILE + ".tmp", PORT_FILE)
server.serve_forever()
PY
echo healthy > "$T/state"
python3 -I "$T/mock.py" "$T/state" "$T/port" "$T/requests" 2> "$T/mock.err" &
MOCK=$!
for _ in $(seq 100); do [ -s "$T/port" ] && break; sleep 0.1; done
[ -s "$T/port" ] || { echo "✗ the mock node didn't start: $(cat "$T/mock.err")" >&2; exit 1; }
RPC=http://127.0.0.1:$(cat "$T/port")
# A port nothing listens on (bound, then released).
DEAD=$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')

# ---- the stand-in gh ----

# It logs each call (its arguments on a line, then a --body-file's text indented by "  | ") and
# answers like gh: FAKE_GH_OPEN lists the open alert issues' numbers, FAKE_GH_FAIL names the call
# that fails ("issue create", "issue list", …).
cat > "$T/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >> "$FAKE_GH_LOG"
echo "${GH_REPO:-}" > "$FAKE_GH_LOG.repo"
prev=""
for a in "$@"; do
  if [ "$prev" = --body-file ]; then sed 's/^/  | /' "$a" >> "$FAKE_GH_LOG"; fi
  prev=$a
done
if [ "$1 ${2:-}" = "${FAKE_GH_FAIL:-}" ]; then echo "HTTP 403: Resource not accessible by integration" >&2; exit 1; fi
case "$1 ${2:-}" in
  "issue list") jq -cn --arg open "${FAKE_GH_OPEN:-}" '[$open | splits(" +") | select(length > 0) | {number: tonumber}]' ;;
  "issue create") echo "https://github.com/jackdishman/blockula/issues/42" ;;
  "issue comment") echo "https://github.com/jackdishman/blockula/issues/$3#issuecomment-1" ;;
  "issue view") printf '%s\n' "${FAKE_GH_VIEW:-{\"body\":\"\",\"comments\":[]}}" ;;
  "issue close" | "label create") ;;
  *) echo "unexpected gh call: $*" >&2; exit 9 ;;
esac
STUB
# The real gh must never run here.
mkdir -p "$T/bin"
printf '#!/bin/sh\necho "the real gh was called: $*" >&2\nexit 99\n' > "$T/bin/gh"
chmod +x "$T/gh" "$T/bin/gh"

TOKEN=fake-watch-token-never-printed-0123456789  # not GitHub's format, so no secret scanner flags it
# watch STATE [VAR=value …]: one run of the watch against the mock in STATE and the stand-in gh;
# prints its exit code. Its output is in $T/out, the gh calls and bodies in $T/gh.log (and every
# run's, for the last test, in $T/all).
watch() {
  local rc=0
  echo "$1" > "$T/state"; shift
  : > "$T/gh.log"
  env -i PATH="$T/bin:$PATH" HOME="$T" RPC="$RPC" GH="$T/gh" FAKE_GH_LOG="$T/gh.log" WAIT_SECS=0 CURL_RETRIES=0 \
    GITHUB_SERVER_URL=https://github.com GITHUB_REPOSITORY=jackdishman/blockula GITHUB_RUN_ID=4242 GH_TOKEN="$TOKEN" "$@" \
    bash scripts/testnet-watch.sh > "$T/out" 2>&1 || rc=$?
  cat "$T/out" "$T/gh.log" >> "$T/all"
  echo "$rc"
}
out() { cat "$T/out"; }
calls() { grep -v '^  | ' "$T/gh.log" || true; }
body() { sed -n 's/^  | //p' "$T/gh.log"; }
LOOKUP="issue list --label testnet-alert --state open --json number --limit 100"
# A failure must be a line of the report, never the script falling over.
sound() { # what
  if grep -qE 'unbound variable|syntax error|command not found|parse error|jq: error|Traceback' "$T/out"; then bad "$1: the script fell over: $(out)"
  elif [ "$(grep -cE '^(✓|✗) ' "$T/out")" != 6 ]; then bad "$1: not six report lines: $(out)"
  else ok "$1: all six checks reported, no crash"; fi
}
# The gh calls in order, each as its first two words, joined by commas.
order() { calls | awk '{print $1, $2}' | tr '\n' ',' | sed 's/,$//'; }

# ---- healthy ----

code=$(watch healthy)
[ "$code" = 0 ] && ok "healthy: exit 0" || bad "healthy: exited $code: $(out)"
missing=""
for line in "✓ health: HTTP 200, ok" "✓ chain id: blockula-testnet-2" "✓ finality: finalized height" "✓ head age: 800 ms" "✓ active validators: 4" "✓ jailed or tombstoned: 0 jailed, 0 tombstoned"; do
  grep -qF -- "$line" "$T/out" || missing+=" [$line]"
done
[ -z "$missing" ] && [ "$(grep -c '^✗ ' "$T/out")" = 0 ] && ok "healthy: six named checks, all passing" || bad "healthy: missing$missing in: $(out)"
[ "$(calls)" = "$LOOKUP" ] && ok "healthy, no alert open: the lookup is its only gh call (nothing written)" || bad "healthy: gh calls: $(calls)"
expect "healthy: says nothing is open" "$(out)" "no alert issue is open"
[ "$(cat "$T/gh.log.repo")" = jackdishman/blockula ] && ok "gh acts on GITHUB_REPOSITORY (GH_REPO)" || bad "GH_REPO: $(cat "$T/gh.log.repo")"

# ---- a failure, nothing open: the label, then a new issue that mentions the maintainer ----

code=$(watch stalled)
[ "$code" = 0 ] && ok "stalled: exit 0 once the alert is delivered" || bad "stalled: exited $code: $(out)"
expect "stalled: health fails, with the node's reason" "$(out)" "✗ health: HTTP 503, not ok: head is 75s old"
expect "stalled: finality fails" "$(out)" "in 0 s: not advancing"
expect "stalled: head age fails" "$(out)" "✗ head age: 75000 ms, over the 60000 ms limit"
[ "$(order)" = "issue list,label create,issue create" ] && ok "stalled: lookup, label ensured, issue opened (in that order)" || bad "stalled: gh calls: $(calls)"
expect "stalled: the label is created if missing" "$(calls)" "label create testnet-alert --color B60205 --description"
expect "stalled: the issue is titled by the first failing check, labeled" "$(calls)" "issue create --title Testnet alert: health: HTTP 503, not ok: head is 75s old --label testnet-alert --body-file"
b=$(body)
expect "stalled: the body @-mentions @jackdishman" "$b" "@jackdishman the external watch found the public testnet failing: **health** is the first failing check."
expect "stalled: the body has the report" "$b" "✗ head age: 75000 ms, over the 60000 ms limit"
expect "stalled: the body has the run's URL" "$b" "- Run: https://github.com/jackdishman/blockula/actions/runs/4242"
printf '%s\n' "$b" | grep -qE '^- Time: [0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2} UTC$' && ok "stalled: the body has the time (UTC)" || bad "stalled: no UTC time in: $b"
expect "stalled: the log says what it did" "$(out)" "→ opened an alert issue: https://github.com/jackdishman/blockula/issues/42"

# ---- a failure with an alert open: a comment there, no second issue ----

code=$(watch wrong-chain FAKE_GH_OPEN="9 7")
[ "$code" = 0 ] && ok "wrong chain: exit 0" || bad "wrong chain: exited $code: $(out)"
expect "wrong chain: the chain id check fails" "$(out)" "✗ chain id: blockula-testnet, expected blockula-testnet-2"
[ "$(order)" = "issue list,issue view,issue comment" ] && ok "wrong chain, alert open: reads its reports, then a comment only (no new issue, no label call, no close)" || bad "wrong chain: gh calls: $(calls)"
expect "wrong chain: on the oldest open alert issue" "$(calls)" "issue comment 7 --body-file"
expect "wrong chain: the comment has the report" "$(body)" "Failing: **chain id** is the first failing check."
refute "wrong chain: the comment doesn't mention again (the issue's thread already notifies)" "$(body)" "@jackdishman"

# ---- the same failure again: a comment only when it changed or an hour passed ----

now=$(date -u +%s)
seen() { printf '{"body":"<!-- testnet-watch failing=%s at=%s -->","comments":[{"body":"Failing.\\n<!-- testnet-watch failing=%s at=%s -->"}]}' "$1" "$2" "$3" "$4"; }
code=$(watch wrong-chain FAKE_GH_OPEN=7 FAKE_GH_VIEW="$(seen health $((now - 9000)) chain_id $((now - 600)))")
[ "$code" = 0 ] && [ "$(order)" = "issue list,issue view" ] && expect "same checks failing, reported 10 min ago: no new comment" "$(out)" "no new comment" || bad "same failure, recent: exited $code, gh calls: $(calls)"
code=$(watch wrong-chain FAKE_GH_OPEN=7 FAKE_GH_VIEW="$(seen chain_id $((now - 9000)) chain_id $((now - 4000)))")
[ "$code" = 0 ] && [ "$(order)" = "issue list,issue view,issue comment" ] && ok "same checks failing, last report over an hour old: a reminder comment" || bad "same failure, old: exited $code, gh calls: $(calls)"
code=$(watch wrong-chain FAKE_GH_OPEN=7 FAKE_GH_VIEW="$(seen chain_id $((now - 900)) health $((now - 300)))")
[ "$code" = 0 ] && [ "$(order)" = "issue list,issue view,issue comment" ] && ok "other checks failing than last reported: a comment" || bad "changed failure: exited $code, gh calls: $(calls)"
expect "a report carries its marker" "$(body)" "<!-- testnet-watch failing=chain_id at="
code=$(watch wrong-chain FAKE_GH_OPEN=7 FAKE_GH_VIEW='not json at all')
[ "$code" = 0 ] && [ "$(order)" = "issue list,issue view,issue comment" ] && ok "an unreadable issue: a comment (fails toward notifying)" || bad "unreadable issue: exited $code, gh calls: $(calls)"

# ---- every check passes with an alert open: "Recovered", then closed ----

code=$(watch healthy FAKE_GH_OPEN=7)
[ "$code" = 0 ] && ok "recovery: exit 0" || bad "recovery: exited $code: $(out)"
[ "$(order)" = "issue list,issue comment,issue close" ] && ok "recovery: comments, then closes" || bad "recovery: gh calls: $(calls)"
expect "recovery: closes the alert issue" "$(calls)" "issue close 7"
expect "recovery: the comment says Recovered, with the report" "$(body)" "Recovered: every check passes."
expect "recovery: and the passing checks" "$(body)" "✓ jailed or tombstoned: 0 jailed, 0 tombstoned"

# ---- TEST_ALERT=true ----

code=$(watch healthy TEST_ALERT=true)
[ "$code" = 0 ] && ok "test alert: exit 0" || bad "test alert: exited $code: $(out)"
[ "$(order)" = "issue list,label create,issue create" ] && ok "test alert, nothing open: label, then a new issue (left open)" || bad "test alert: gh calls: $(calls)"
expect "test alert: titled as a test" "$(calls)" "issue create --title Testnet alert: test of alert delivery --label testnet-alert"
expect "test alert: says clearly it's a test, and mentions @jackdishman" "$(body)" "@jackdishman this is a **test of alert delivery**"
expect "test alert: says the next healthy run closes it" "$(body)" "The next scheduled run that passes every check comments \"Recovered\" and closes this issue"
code=$(watch healthy FAKE_GH_OPEN=42)
[ "$code" = 0 ] && [ "$(order)" = "issue list,issue comment,issue close" ] && expect "test alert: the next healthy run closes it" "$(calls)" "issue close 42" || bad "test alert follow-up: exited $code, gh calls: $(calls)"
code=$(watch healthy TEST_ALERT=true FAKE_GH_OPEN=7)
[ "$code" = 0 ] && [ "$(order)" = "issue list,issue comment" ] && ok "test alert, alert open: a comment, and it stays open" || bad "test alert, open: exited $code, gh calls: $(calls)"
expect "test alert, alert open: the comment mentions @jackdishman as a test" "$(body)" "@jackdishman this is a **test of alert delivery**"
code=$(watch stalled TEST_ALERT=true)
expect "test alert while failing: titled by the failure" "$(calls)" "issue create --title Testnet alert: health:"
expect "test alert while failing: says it was also a test" "$(body)" "This run was also a test of alert delivery"

# ---- each check on its own ----

code=$(watch jailed)
[ "$code" = 0 ] && expect "jailed: fails with the count, who and why" "$(out)" "✗ jailed or tombstoned: 1 jailed, 0 tombstoned: tboo1kmq79…xx25dx jailed (missed 51 of its last 100 leader slots) until height 4491" || bad "jailed: exited $code: $(out)"
[ "$(grep -c '^✗ ' "$T/out")" = 1 ] && expect "jailed: the only failure, and it opens the alert" "$(calls)" "issue create --title Testnet alert: jailed or tombstoned: 1 jailed, 0 tombstoned" || bad "jailed: $(out)"
code=$(watch tombstoned)
expect "tombstoned: fails with the count" "$(out)" "✗ jailed or tombstoned: 0 jailed, 1 tombstoned: tboo1ds4xw…k2gdxh tombstoned at height 1200"
code=$(watch healthy EXPECT_VALIDATORS=5)
[ "$(grep -c '^✗ ' "$T/out")" = 1 ] && expect "EXPECT_VALIDATORS=5: only the validator count fails" "$(out)" "✗ active validators: 4, expected at least 5" || bad "EXPECT_VALIDATORS: $(out)"
code=$(watch healthy MAX_HEAD_AGE_MS=500)
[ "$(grep -c '^✗ ' "$T/out")" = 1 ] && expect "MAX_HEAD_AGE_MS=500: only the head age fails" "$(out)" "✗ head age: 800 ms, over the 500 ms limit" || bad "MAX_HEAD_AGE_MS: $(out)"

# ---- the node down, hanging or flaky: failures in the report, never a crash ----

code=$(watch down)
[ "$code" = 0 ] && expect "down (502 from the proxy): an alert" "$(calls)" "issue create --title Testnet alert: health: HTTP 502, not a JSON answer" || bad "down: exited $code: $(out)"
sound "down"
code=$(watch healthy RPC="http://127.0.0.1:$DEAD")
[ "$code" = 0 ] && expect "unreachable (connection refused): an alert" "$(calls)" "issue create --title Testnet alert: health: no answer (curl exit 7" || bad "unreachable: exited $code: $(out)"
sound "unreachable"
expect "unreachable: every check says why" "$(out)" "✗ jailed or tombstoned: /api/validators: no answer (curl exit 7"
code=$(watch hang CURL_TIMEOUT=1)
[ "$code" = 0 ] && expect "hanging (timeouts): an alert" "$(calls)" "issue create --title Testnet alert: health: no answer (curl exit 28" || bad "hanging: exited $code: $(out)"
sound "hanging"
code=$(watch flaky CURL_RETRIES=1)
[ "$(grep '^flaky /api/health ' "$T/requests" | tr '\n' ,)" = "flaky /api/health 503,flaky /api/health 200," ] && ok "flaky: the 503 from /api/health was retried" || bad "flaky: health reads: $(grep '^flaky' "$T/requests")"
[ "$code" = 0 ] && [ "$(calls)" = "$LOOKUP" ] && expect "flaky: and isn't an alert once it answers (the retry's answer is read alone)" "$(out)" "✓ health: HTTP 200, ok" || bad "flaky: exited $code: $(out) $(calls)"

# ---- the node's text can't mention anyone, break out of the report or command the runner ----

code=$(watch hostile)
[ "$code" = 0 ] || bad "hostile: exited $code: $(out)"
sound "hostile"
refute "hostile: no @-mention from the node's text (title or body)" "$(cat "$T/gh.log")" "@everyone"
expect "hostile: its @ is defused" "$(body)" "(at)everyone"
[ "$(body | grep -c '^```')" = 2 ] && ok "hostile: the report's code block stays one block" || bad "hostile: fences: $(body)"
refute "hostile: no backtick from the node's text in the title" "$(calls)" '`'
grep -q '^::' "$T/out" && bad "hostile: a line of the log starts a workflow command: $(out)" || ok "hostile: no workflow command in the log"

# ---- a broken alert path fails the run ----

code=$(watch stalled FAKE_GH_FAIL="issue create" GITHUB_ACTIONS=true)
[ "$code" = 1 ] && ok "gh issue create fails: exit 1" || bad "gh issue create fails: exited $code: $(out)"
expect "gh issue create fails: says so" "$(out)" "✗ alert delivery failed: gh issue create: HTTP 403: Resource not accessible by integration"
expect "gh issue create fails: an error annotation in Actions" "$(out)" "::error title=Testnet watch::alert delivery failed"
expect "gh issue create fails: the report is still in the log" "$(out)" "✗ health: HTTP 503"
code=$(watch healthy FAKE_GH_FAIL="issue list")
[ "$code" = 1 ] && ok "gh issue list fails (even with every check passing): exit 1" || bad "gh issue list fails: exited $code: $(out)"
code=$(watch stalled FAKE_GH_FAIL="label create")
[ "$code" = 1 ] && refute "gh label create fails: exit 1, no issue attempted" "$(calls)" "issue create" || bad "gh label create fails: exited $code: $(out)"
code=$(watch healthy FAKE_GH_OPEN=7 FAKE_GH_FAIL="issue close")
[ "$code" = 1 ] && ok "gh issue close fails: exit 1" || bad "gh issue close fails: exited $code: $(out)"
code=$(watch stalled GH="$T/no-such-gh")
[ "$code" = 1 ] && expect "no gh at all: exit 1" "$(out)" "alert delivery failed" || bad "no gh: exited $code: $(out)"

# ---- DRY_RUN and bad input ----

code=$(watch stalled DRY_RUN=true GH="$T/no-such-gh")
[ "$code" = 0 ] && [ ! -s "$T/gh.log" ] && ok "DRY_RUN: exit 0 and no gh call" || bad "DRY_RUN: exited $code, gh calls: $(calls) $(out)"
expect "DRY_RUN: prints the label call" "$(out)" "DRY_RUN: gh label create testnet-alert"
expect "DRY_RUN: prints the issue it would open" "$(out)" 'DRY_RUN: gh issue create --title "Testnet alert: health: HTTP 503, not ok: head is 75s old" --label testnet-alert'
expect "DRY_RUN: and its body" "$(out)" "  | @jackdishman the external watch found the public testnet failing"
code=$(watch healthy EXPECT_VALIDATORS=four)
[ "$code" = 2 ] && [ ! -s "$T/gh.log" ] && expect "bad input: exit 2, nothing called" "$(out)" "EXPECT_VALIDATORS must be a whole number" || bad "bad input: exited $code: $(out)"
code=$(watch healthy TEST_ALERT=yes)
[ "$code" = 2 ] && ok "TEST_ALERT must be true or false" || bad "TEST_ALERT=yes: exited $code: $(out)"

refute "the token (GH_TOKEN) is never printed, nor passed to gh as an argument, in any run" "$(cat "$T/all")" "$TOKEN"

if [ $fails -gt 0 ]; then echo "✗ $fails failed" >&2; exit 1; fi
echo "✓ testnet-watch.sh behaves"
