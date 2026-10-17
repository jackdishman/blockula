#!/usr/bin/env bash
# Applies the repository rulesets in this directory (ruleset-*.json) to GitHub, matching each to the
# live ruleset of the same name (audit supply-chain-B-3; docs/OPERATIONS.md "Repository settings").
# By default it only shows what would change: the live ruleset next to the file, as a diff. With
# --apply it writes them (PUT for an existing ruleset, POST for a new one). Rulesets on GitHub that
# have no file here are left alone.
#   deploy/github/apply-rulesets.sh             dry run: show the differences, change nothing
#   deploy/github/apply-rulesets.sh --apply     make GitHub match the files
# Needs gh logged in as a repository admin, and jq. BLOCKULA_REPO overrides the repository.
# Only the maintainer runs this, deliberately: CI never does, and no workflow token can.
set -euo pipefail
cd "$(dirname "$0")"
repo=${BLOCKULA_REPO:-jackdishman/blockula}
apply=0
case "${1:-}" in
  "") ;;
  --apply) apply=1 ;;
  *) echo "usage: deploy/github/apply-rulesets.sh [--apply]" >&2; exit 2 ;;
esac
command -v gh > /dev/null || { echo "Needs the GitHub CLI (gh), logged in as a repository admin." >&2; exit 1; }
command -v jq > /dev/null || { echo "Needs jq." >&2; exit 1; }

# The fields a ruleset is made of: what GitHub returns minus ids, links and timestamps. Arrays of
# rules and checks keep their order (GitHub keeps it too); keys are sorted.
norm='{name, target, enforcement, bypass_actors: (.bypass_actors // []), conditions, rules: (.rules // [])}'

shopt -s nullglob
files=(ruleset-*.json)
[ ${#files[@]} -gt 0 ] || { echo "No ruleset-*.json here." >&2; exit 1; }
# --paginate prints one JSON array per page: merge them.
live_list=$(gh api --paginate "repos/$repo/rulesets?includes_parents=false" | jq -s 'add // []')
changed=0
for f in "${files[@]}"; do
  jq -e 'has("name") and has("target") and has("rules")' "$f" > /dev/null || { echo "✗ $f isn't a ruleset (name, target, rules)" >&2; exit 1; }
  name=$(jq -r .name "$f")
  id=$(printf '%s' "$live_list" | jq -r --arg n "$name" '[.[] | select(.name == $n)] | if length > 1 then error("two rulesets named \($n)") else (.[0].id // "") end')
  want=$(jq -S "$norm" "$f")
  if [ -n "$id" ]; then
    have=$(gh api "repos/$repo/rulesets/$id" | jq -S "$norm")
    if [ "$have" = "$want" ]; then
      echo "✓ $name (ruleset $id) matches $f"
      continue
    fi
    echo "≠ $name (ruleset $id) differs from $f (- live, + file):"
    diff -u <(printf '%s\n' "$have") <(printf '%s\n' "$want") | tail -n +3 || true
  else
    echo "+ $name: no such ruleset on $repo yet; $f would create it:"
    printf '%s\n' "$want"
  fi
  changed=1
  if [ $apply = 1 ]; then
    if [ -n "$id" ]; then gh api --method PUT "repos/$repo/rulesets/$id" --input "$f" > /dev/null
    else gh api --method POST "repos/$repo/rulesets" --input "$f" > /dev/null; fi
    echo "  → applied"
  fi
done
if [ $changed = 1 ] && [ $apply = 0 ]; then
  echo "Dry run: nothing changed. Re-run with --apply to make $repo match these files."
fi
