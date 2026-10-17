#!/usr/bin/env bash
# The nightly fuzz job runs in a public repository, whose workflow logs and artifacts anyone can
# read, and a fuzzer crash is an undisclosed vulnerability (audit supply-chain-B-1). CI (`ops`)
# fails if the fuzz workflow or the fuzz crate stops keeping crashes private:
#   - every `cargo fuzz run` sends its output to a file (`> file 2>&1`), with -print_final_stats off;
#   - the fuzz workflow uploads nothing but sealed crash material (a path ending in .age);
#   - open findings' inputs never enter the tree: node/fuzz/regressions/ doesn't exist, and
#     node/fuzz/private/ (where they live on a maintainer's machine) is ignored by git.
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

[ ! -e node/fuzz/regressions ] || bad "node/fuzz/regressions/ exists: open findings' inputs stay private (node/fuzz/README.md)"
grep -qxE '/?private/?' node/fuzz/.gitignore || bad "node/fuzz/.gitignore must ignore private/ (open findings' inputs)"

[ $fail = 0 ] && echo "✓ fuzz: output to a file, only sealed crashes uploaded, no open findings in the tree"
exit $fail
