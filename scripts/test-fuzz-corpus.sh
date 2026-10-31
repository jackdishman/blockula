#!/usr/bin/env bash
# Tests scripts/fuzz-bound-corpus.sh and the way .github/workflows/fuzz.yml uses it (the nightly
# corpus is bounded so replaying it can't eat the time budget; scripts/check-fuzz-workflow.sh holds
# the workflow to it).
#   bash scripts/test-fuzz-corpus.sh
set -euo pipefail
cd "$(dirname "$0")/.."
BOUND=$PWD/scripts/fuzz-bound-corpus.sh
CHECK=$PWD/scripts/check-fuzz-workflow.sh
WF=.github/workflows/fuzz.yml
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
pass=0
fail() { echo "✗ $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "✓ $1"; }

# A corpus of N units whose modification times ascend with their names' numbers.
corpus() {
  rm -rf "$T/c" && mkdir -p "$T/c"
  for i in $(seq 1 "$1"); do
    printf 'unit %s' "$i" > "$T/c/u$(printf '%04d' "$i")"
    touch -t "2026010100$(printf '%02d' $(( i % 60 )))" "$T/c/u$(printf '%04d' "$i")"
  done
}
# The stamps above wrap at 60 files; give the newest ten a later hour so "newest" is exact.
newest_later() {
  for i in $(seq $(( $1 - 9 )) "$1"); do touch -t "202601020000" "$T/c/u$(printf '%04d' "$i")"; done
}

corpus 30
out=$("$BOUND" "$T/c" 50)
[ "$(ls "$T/c" | wc -l | tr -d ' ')" = 30 ] || fail "a corpus within the bound lost files"
printf '%s' "$out" | grep -qF "within the bound of 50" || fail "within the bound: said $out"
ok "within the bound: nothing is removed"

corpus 30; newest_later 30
out=$("$BOUND" "$T/c" 10)
[ "$(ls "$T/c" | wc -l | tr -d ' ')" = 10 ] || fail "30 units cut to 10 left $(ls "$T/c" | wc -l)"
for i in $(seq 21 30); do [ -e "$T/c/u$(printf '%04d' "$i")" ] || fail "the newest unit $i was cut"; done
printf '%s' "$out" | grep -qF "30 units cut to the newest 10" || fail "cut: said $out"
if printf '%s' "$out" | grep -q "u00"; then fail "the output names a unit: $out"; fi
ok "30 units cut to the newest 10, and the line names none"

"$BOUND" "$T/none" 10 | grep -qF "none yet" || fail "a missing directory isn't an empty corpus"
if "$BOUND" "$T/c" 0 > /dev/null 2>&1; then fail "a bound of 0 was accepted"; fi
if "$BOUND" "$T/c" many > /dev/null 2>&1; then fail "a bound that isn't a number was accepted"; fi
if "$BOUND" "$T/c" > /dev/null 2>&1; then fail "a missing bound was accepted"; fi
ok "missing directory, bad bounds"

# The workflow: the committed one passes; each way back to an unbounded corpus fails.
"$CHECK" "$WF" > /dev/null 2>&1 || fail "check-fuzz-workflow.sh refuses the committed $WF"
variant() {
  local name=$1 want=$2 expr=$3
  sed -E "$expr" "$WF" > "$T/wf.yml"
  if cmp -s "$WF" "$T/wf.yml"; then fail "$name: the edit ($expr) no longer changes $WF"; fi
  local out
  if out=$("$CHECK" "$T/wf.yml" 2>&1); then fail "$name: check-fuzz-workflow.sh accepts it"; fi
  printf '%s' "$out" | grep -qF -- "$want" || fail "$name: expected \"$want\" in: $out"
}
variant "no bound before the node job's runs" "is not bounded" 's#(^| )\.\./scripts/fuzz-bound-corpus\.sh [^#]*#true#'
variant "no bound before the programs engine's runs" "is not bounded" 's#(^| )\.\./\.\./scripts/fuzz-bound-corpus\.sh [^#]*#true#'
variant "a bound that isn't the variable" "is not bounded" 's#(fuzz-bound-corpus\.sh "[^"]*") "\$CORPUS_MAX_UNITS"#\1 "50000"#'
variant "no bound set" "CORPUS_MAX_UNITS" 's/^( +)CORPUS_MAX_UNITS: .*/\1UNUSED: 1/'
variant "a bound too large to help" "CORPUS_MAX_UNITS" 's/CORPUS_MAX_UNITS: "?[0-9]+"?/CORPUS_MAX_UNITS: 100000/'
variant "no length control" "-len_control=0" 's/-len_control=0//'
variant "no max_len" "-max_len" 's/-max_len=[0-9]+//'
variant "the programs state machine left out" "state_machine_programs" 's/, state_machine_programs//'
ok "check-fuzz-workflow.sh: refuses an unbounded corpus, a missing bound, no length control, a missing target"
echo "✓ fuzz corpus bound: $pass cases"
