#!/usr/bin/env bash
# Clippy, the way CI runs it. Blocking: correctness, suspicious and perf lints, a few
# restriction lints that matter in a consensus node, and every rustc warning. Style and
# complexity lints are reported (count + list) but don't fail the build.
#   scripts/clippy.sh          blocking set, then the full report
#   scripts/clippy.sh --report full report only
set -euo pipefail
cd "$(dirname "$0")/.."
manifest=node/Cargo.toml

blocking=(
  -A clippy::all
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
  # No debugging leftovers or placeholder panics in shipped code.
  -D clippy::dbg_macro -D clippy::todo -D clippy::unimplemented -D clippy::mem_forget
  # Known debt (one or two sites each). Fix them and delete the line.
  -A clippy::map_entry -A clippy::cloned_ref_to_slice_refs -A clippy::unnecessary_get_then_check
  -D warnings
)

if [ "${1:-}" != --report ]; then
  cargo clippy --locked --manifest-path "$manifest" --all-targets -- "${blocking[@]}"
fi

# Everything else clippy warns about by default, deduplicated across the bin and test targets.
report=$(cargo clippy --locked --manifest-path "$manifest" --all-targets --message-format=json 2>/dev/null \
  | jq -r 'select(.reason == "compiler-message" and .message.level == "warning" and .message.code != null)
           | "\(.message.code.code) \(.message.spans[0].file_name):\(.message.spans[0].line_start)"' | sort -u)
count=$(printf '%s' "$report" | grep -c . || true)
echo "clippy (non-blocking): $count warning(s)"
printf '%s\n' "$report" | awk 'NF {print $1}' | sort | uniq -c | sort -rn
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  { echo "### Clippy: $count non-blocking warning(s)"; echo '```'; printf '%s\n' "$report"; echo '```'; } >> "$GITHUB_STEP_SUMMARY"
fi
