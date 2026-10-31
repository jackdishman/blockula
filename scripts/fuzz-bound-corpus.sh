#!/usr/bin/env bash
# Bounds a cached fuzz corpus so that replaying it can't eat the nightly job's time.
#
# libFuzzer replays every file of the corpus directories it is given before it fuzzes, and it does
# not stop that replay for -max_total_time: a cached corpus that grows each night (a state-machine
# unit costs tens of milliseconds, and the corpus gains hundreds a night) would one day take the whole
# time budget to replay, and the job would run green having fuzzed nothing new. So before the run,
# the corpus is cut to its newest MAX units (by modification time, which the cache restore keeps):
# the coverage the oldest ones found is found again, and the seeds (a second directory) always stay.
#   scripts/fuzz-bound-corpus.sh <corpus dir> <max units>
# Prints one line with the counts, never a file name or content (the corpus is public, but the
# habit is the workflow's: its log says only what it must). A missing directory is an empty corpus.
set -euo pipefail
[ $# -eq 2 ] || { echo "usage: scripts/fuzz-bound-corpus.sh <corpus dir> <max units>" >&2; exit 2; }
dir=$1 max=$2
[[ $max =~ ^[0-9]+$ ]] && [ "$max" -ge 1 ] || { echo "max units must be a positive number, not '$max'" >&2; exit 2; }
if [ ! -d "$dir" ]; then echo "corpus: none yet"; exit 0; fi
# Files only, newest first (ls -t sorts by modification time; names are libFuzzer's content hashes).
count=$(find "$dir" -maxdepth 1 -type f | wc -l | tr -d ' ')
if [ "$count" -le "$max" ]; then echo "corpus: $count units, within the bound of $max"; exit 0; fi
# The newest $max stay; the rest go. Names are hex hashes (no spaces or newlines), so `ls` is safe here.
(cd "$dir" && ls -t -p | grep -v / | tail -n +"$((max + 1))" | xargs rm -f --)
echo "corpus: $count units cut to the newest $max"
