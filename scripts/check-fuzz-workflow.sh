#!/usr/bin/env bash
# The nightly fuzz job runs in a public repository, whose workflow logs and artifacts anyone can
# read, and a fuzzer crash is an undisclosed vulnerability (audit supply-chain-B-1). CI (`ops`)
# fails if the fuzz workflow or the fuzz crate stops keeping crashes private:
#   - every `cargo fuzz run` sends its output to a file (`> file 2>&1`), with -print_final_stats off;
#   - the fuzz workflow uploads nothing but sealed crash material (a path ending in .age);
#   - open findings' inputs never enter the tree: node/fuzz/regressions/ and node/vm/fuzz/regressions/
#     don't exist, and private/ (where they live on a maintainer's machine) is ignored by git in both
#     fuzz crates;
#   - a fuzz run is worth its time: the cached corpus is bounded before every run (libFuzzer replays
#     all of it before it fuzzes, and doesn't stop for -max_total_time), and the state machines'
#     runs start at their maximum length instead of the shortest seed's.
#   bash scripts/check-fuzz-workflow.sh [workflow file]
set -euo pipefail
cd "$(dirname "$0")/.."
wf=${1:-.github/workflows/fuzz.yml}
fail=0
bad() { echo "✗ $*" >&2; fail=1; }

# Lines joined across backslash continuations, so a command is one line.
joined=$(awk '{ if (sub(/\\$/, "")) printf "%s ", $0; else print }' "$wf")
runs=$(printf '%s\n' "$joined" | grep -E 'fuzz +run' || true)
[ -n "$runs" ] || bad "$wf has no \`cargo fuzz run\` to check"
while IFS= read -r line; do
  [ -n "$line" ] || continue
  [[ $line =~ \>[[:space:]]*\"?[^[:space:]]+[[:space:]]+2\>\&1 ]] || bad "a fuzz run prints into the public log (send it to a file: > file 2>&1): $(echo "$line" | sed -E 's/^ +//')"
  [[ $line == *-print_final_stats=0* ]] || bad "a fuzz run doesn't pass -print_final_stats=0: $(echo "$line" | sed -E 's/^ +//')"
done <<< "$runs"
grep -qE -- '-print_final_stats=1' "$wf" && bad "$wf turns -print_final_stats on"

# Every upload-artifact step's path ends in .age.
while IFS= read -r path; do
  case $path in
    *.age) ;;
    *) bad "$wf uploads $path: only sealed crash material (*.age) may leave the runner" ;;
  esac
done < <(awk '
  /^[[:space:]]*- / { up = 0 }
  /uses:[[:space:]]*actions\/upload-artifact@/ { up = 1 }
  up && /path:/ {
    p = $0; sub(/.*path:[[:space:]]*/, "", p)
    # The inline form, with: { path: x, name: y }: up to the next key or the closing brace.
    sub(/,[[:space:]]+[a-z-]+:.*$/, "", p); sub(/[[:space:]]+}[[:space:]]*$/, "", p)
    sub(/[[:space:]]+$/, "", p); gsub(/^["\047]|["\047]$/, "", p)
    print p; up = 0
  }
' "$wf")

# Findings in either fuzz crate (node/vm/fuzz's three targets run in the same sealed job).
for crate in node/fuzz node/vm/fuzz; do
  [ ! -e "$crate/regressions" ] || bad "$crate/regressions/ exists: open findings' inputs stay private (node/fuzz/README.md)"
  grep -qxE '/?private/?' "$crate/.gitignore" || bad "$crate/.gitignore must ignore private/ (open findings' inputs)"
  grep -qxE '/?coverage/?' "$crate/.gitignore" || bad "$crate/.gitignore must ignore coverage/ (coverage reports name the code a crash reaches)"
done

# The corpus is bounded before every run: CORPUS_MAX_UNITS is set to a bound that keeps the replay
# short, and each job that runs a fuzzer bounds the corpus (scripts/fuzz-bound-corpus.sh) in a step
# before the one that runs it.
max=$(sed -nE 's/^ +CORPUS_MAX_UNITS: "?([0-9]+)"?.*/\1/p' "$wf" | head -n 1)
{ [ -n "$max" ] && [ "$max" -ge 1 ] && [ "$max" -le 5000 ]; } || bad "$wf must set CORPUS_MAX_UNITS to a number up to 5000 (the bound on the cached corpus; replaying it counts against the run's time)"
bounded=$(awk '
  /^jobs:/ { injobs = 1 }
  injobs && /^  [a-z][a-z0-9-]*:[[:space:]]*$/ { job = $1; bound = 0 }
  /fuzz-bound-corpus\.sh[^#]*CORPUS_MAX_UNITS/ { bound = 1 }
  /fuzz +run/ { if (!bound) print job }
' "$wf")
[ -z "$bounded" ] || bad "a fuzz run's corpus is not bounded in job(s) $(echo $bounded): call scripts/fuzz-bound-corpus.sh with \$CORPUS_MAX_UNITS in a step before it"
[ -x scripts/fuzz-bound-corpus.sh ] || bad "scripts/fuzz-bound-corpus.sh is missing or not executable"

# The state machines' inputs are op streams, and libFuzzer's length control would hold them to the
# shortest seed's length: -len_control=0 and a -max_len for both targets.
grep -qE 'state_machine_programs\) *LENGTH=\(.*-len_control=0.*\)' "$wf" && grep -qE 'state_machine \| state_machine_programs\) *LENGTH=\(.*-max_len=[0-9]+' "$wf" || bad "$wf must run state_machine and state_machine_programs with -len_control=0 and a -max_len (a run seeded with short inputs never grows past them otherwise)"
grep -qE 'target: \[.*\bstate_machine_programs\b' "$wf" || bad "$wf's fuzz matrix is missing state_machine_programs"

[ $fail = 0 ] && echo "✓ fuzz: output to a file, only sealed crashes uploaded, no open findings in the tree"
exit $fail
