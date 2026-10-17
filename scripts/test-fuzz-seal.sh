#!/usr/bin/env bash
# Tests scripts/fuzz-seal-crash.sh (audit supply-chain-B-1): crash material leaves the runner only
# encrypted to the maintainers' age keys, the plaintext is always deleted, and nothing about the
# crash reaches the output. With `age` and `age-keygen` on PATH it checks a real round trip (CI
# installs them; REQUIRE_AGE=1 makes their absence a failure), otherwise only the refusals.
#   bash scripts/test-fuzz-seal.sh
set -euo pipefail
cd "$(dirname "$0")/.."
SEAL=$PWD/scripts/fuzz-seal-crash.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
SECRET="crash-input-$RANDOM$RANDOM-do-not-print"
pass=0
fail() { echo "✗ $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "✓ $1"; }

# material: what the fuzz job gathers (a crash file and the fuzzer's log, which names the input).
material() {
  rm -rf "$T/m" && mkdir -p "$T/m/artifacts"
  printf '%s' "$SECRET" > "$T/m/artifacts/crash-0123456789abcdef"
  printf 'thread panicked at src/types.rs:1:1\nBase64: %s\n' "$SECRET" > "$T/m/fuzz.log"
}
# seal NAME EXPECTED_EXIT [VAR=value…]: runs the script, checks the exit code, that the plaintext
# is gone and that the output doesn't carry the crash.
seal() {
  local name=$1 want=$2 code=0 out; shift 2
  material
  rm -rf "$T/out" && : > "$T/gh-output"
  out=$(env "$@" GITHUB_OUTPUT="$T/gh-output" "$SEAL" state_machine "$T/m" "$T/out" 2>&1) || code=$?
  [ "$code" = "$want" ] || fail "$name: exit $code, expected $want: $out"
  [ ! -e "$T/m" ] || fail "$name: the plaintext material is still there"
  if printf '%s' "$out" | grep -qF "$SECRET"; then fail "$name: the output carries the crash: $out"; fi
  LAST_OUT=$out
}

seal "no recipients: nothing kept" 0 RECIPIENTS=
[ ! -e "$T/out/fuzz-crash-state_machine.tar.age" ] || fail "no recipients, but something was sealed"
grep -qx "sealed=0" "$T/gh-output" || fail "no recipients: sealed=0 not written"
printf '%s' "$LAST_OUT" | grep -qF "nothing is uploaded" || fail "no recipients: no explanation"
ok "no recipients: material deleted, nothing sealed, the run says why"

seal "a malformed recipient" 1 RECIPIENTS="age1notarecipient"
[ ! -e "$T/out/fuzz-crash-state_machine.tar.age" ] || fail "a malformed recipient, but something was sealed"
grep -qx "sealed=0" "$T/gh-output" || fail "a malformed recipient: sealed=0 not written"
ok "a malformed recipient: refused, material deleted"

# An age that fails half-way: no partial ciphertext is left to upload.
mkdir -p "$T/badage"
printf '#!/bin/sh\nwhile [ $# -gt 0 ]; do [ "$1" = -o ] && echo partial > "$2"; shift; done\nexit 1\n' > "$T/badage/age"
chmod +x "$T/badage/age"
seal "a failing age" 1 PATH="$T/badage:$PATH" RECIPIENTS="age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p"
[ ! -e "$T/out/fuzz-crash-state_machine.tar.age" ] || fail "a failing age left a partial ciphertext"
if printf '%s' "$LAST_OUT" | grep -qF "isn't an age"; then fail "a failing age: the recipient was refused instead, so age never ran"; fi
ok "age failing: no partial ciphertext, material deleted"

material
if "$SEAL" state_machine / "$T/out" > /dev/null 2>&1; then fail "it agreed to seal (and delete) /"; fi
ok "refuses to treat / as the material directory"

# scripts/check-fuzz-workflow.sh: the committed workflow passes; each way back to publishing a
# crash (the pre-fix workflow's three) fails. Variants are edits of the real file, and each edit
# must change it, so the cases can't silently go stale.
CHECK=$PWD/scripts/check-fuzz-workflow.sh
WF=.github/workflows/fuzz.yml
"$CHECK" "$WF" > /dev/null 2>&1 || fail "check-fuzz-workflow.sh refuses the committed $WF"
variant() {
  local name=$1 want=$2 expr=$3
  sed -E "$expr" "$WF" > "$T/wf.yml"
  if cmp -s "$WF" "$T/wf.yml"; then fail "$name: the edit ($expr) no longer changes $WF"; fi
  local out
  if out=$("$CHECK" "$T/wf.yml" 2>&1); then fail "$name: check-fuzz-workflow.sh accepts it"; fi
  printf '%s' "$out" | grep -qF -- "$want" || fail "$name: expected \"$want\" in: $out"
}
variant "a plain crash upload" "only sealed crash material" 's#path: \$\{\{ runner.temp \}\}/sealed/fuzz-crash-\$\{\{ matrix.target \}\}.tar.age#path: node/fuzz/artifacts/${{ matrix.target }}#'
variant "an inline plain upload" "only sealed crash material" 's#^( +)path: \$\{\{ runner.temp \}\}/sealed/.*#\1path: "${{ runner.temp }}/crash"#'
variant "fuzzer output in the log" "prints into the public log" 's# > "\$RUNNER_TEMP/crash/\$TARGET/fuzz.log" 2>&1##'
variant "final stats on" "print_final_stats" 's#-print_final_stats=0#-print_final_stats=1#'
variant "final stats not turned off" "print_final_stats=0" 's# -print_final_stats=0##'
ok "check-fuzz-workflow.sh: passes $WF, refuses a plain upload, output in the log, final stats"

if command -v age > /dev/null && command -v age-keygen > /dev/null; then
  age-keygen -o "$T/id1" 2> /dev/null
  age-keygen -o "$T/id2" 2> /dev/null
  r1=$(age-keygen -y "$T/id1")
  r2=$(age-keygen -y "$T/id2")
  seal "two maintainers" 0 RECIPIENTS="$r1, $r2"
  grep -qx "sealed=1" "$T/gh-output" || fail "sealed=1 not written"
  sealed=$T/out/fuzz-crash-state_machine.tar.age
  [ -s "$sealed" ] || fail "nothing sealed"
  if grep -qaF "$SECRET" "$sealed"; then fail "the sealed file carries the crash in the clear"; fi
  for id in "$T/id1" "$T/id2"; do
    rm -rf "$T/open" && mkdir "$T/open"
    age -d -i "$id" "$sealed" | tar -C "$T/open" -xf -
    [ "$(cat "$T/open/artifacts/crash-0123456789abcdef")" = "$SECRET" ] || fail "decrypting with $(basename "$id") doesn't give back the crash"
    grep -qF "$SECRET" "$T/open/fuzz.log" || fail "decrypting with $(basename "$id") doesn't give back the log"
  done
  age-keygen -o "$T/id3" 2> /dev/null
  if age -d -i "$T/id3" "$sealed" > /dev/null 2>&1; then fail "a key that isn't a recipient decrypts it"; fi
  ok "real age: each maintainer decrypts the crash and the log; nobody else can"
elif [ "${REQUIRE_AGE:-}" = 1 ]; then
  fail "REQUIRE_AGE=1 but age/age-keygen aren't on PATH"
else
  echo "- age isn't installed: skipped the real round trip (CI runs it)"
fi
echo "✓ fuzz-seal-crash.sh: $pass cases"
