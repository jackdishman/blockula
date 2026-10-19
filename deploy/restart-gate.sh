# shellcheck shell=bash
# The participation gate for restarting validators: sourced by deploy/launch.sh and
# deploy/set-github-token.sh; deploy/test-deploy.sh tests it against a stand-in fly, and
# scripts/rehearse-testnet.sh runs it against local nodes.
#
# A restarted validator isn't back once its node serves new finalized blocks: it fetches those over
# HTTP before consensus starts, without signing, a p2p link or a proposal of its own. Restarting
# the next one then took a second validator of four out, below quorum (the blockula-testnet-2
# launch: 86 s and 56 s without a block, and a validator jailed). So `gate_check` passes only when
# one round of reads shows all of this:
#   - the public node answers for this genesis, its newest block is at most HEAD_MAX_SECS old, and
#     its last PACE_BLOCKS blocks took at most PACE_SECS: full pace (20 blocks at 1 s blocks take
#     ~19 s, so one view timing out, 3-4 s, still passes; a leader whose proposals can't reach a
#     quorum times out every lap of four views, ~40 s);
#   - every validator's node, asked on its own machine (`fly ssh console … blockula status`: the
#     image has no curl), answers for this genesis, is in the active set, signs (slashing
#     protection no longer watches the network after its start), isn't catching up, and is linked
#     over p2p to every other active validator (proposals and votes travel over direct links only;
#     `p2p_active_peers` counts only those links, where `p2p_peers` also counts the last epoch's
#     set, candidates and bootstrappers);
#   - the active set is exactly the N validators read: one this script doesn't run would go
#     unread, and the next restart could leave the rest short of a quorum;
#   - each validator just restarted runs a new process (its start time moved) and has proposed a
#     block since that process started.
# Nothing that doesn't answer counts as passing. `gate_wait` repeats a check until it passes or
# GATE_SECS go by (at once for what waiting can't fix: a jailed validator, another genesis, a
# blockula without `status`), and its caller then stops, leaving the validators it hasn't
# restarted untouched.
#
# Reads PREFIX and N (validators $PREFIX-v0 …), RPC_URL (the public node), HTTP_PORT (validators'
# HTTP port on their machines), RPC_APP and RPC_PORT (the public node's app and its port there, for
# its own restart) and GENESIS_HASH; optionally PROGRESS_SECS (between checks, 5), GATE_SECS (600:
# a restart passes in 15-35 s under public-nat, but with Commonware's `recommended` reconnection
# limits, public-nat's until they were shortened, a restarted validator took up to ~3 minutes to
# sign, link and propose again, and in one local run nearly 9 to link to all three peers),
# GATE_ROUNDS (at most this many checks; tests), PACE_BLOCKS (20), PACE_SECS (25) and
# HEAD_MAX_SECS (10).

PROGRESS_SECS=${PROGRESS_SECS:-5}
GATE_SECS=${GATE_SECS:-600}
PACE_BLOCKS=${PACE_BLOCKS:-20}
PACE_SECS=${PACE_SECS:-25}
HEAD_MAX_SECS=${HEAD_MAX_SECS:-10}
GATE_STARTED=()      # each validator's process start time (unix seconds) as last read, by index
GATE_RPC_STARTED=""  # the public node's
GATE_WHY=""          # why the last check didn't pass
GATE_OK=""           # what the last check that passed saw
GATE_PUB=""          # the public node's /api/status
GATE_STATUS=""       # the last node's `blockula status`

# What one validator's `blockula status` (the input) says against the public node's status
# ($pub), its recent blocks ($blocks: [{h, t, p}]) and, for a validator just restarted, its
# process start time before ($start, else null): "ok", "ok #<its first block since>", or why not
# ("!…": waiting won't help).
GATE_JQ='
. as $st
| ($pub.active_validators // 0) as $n
| ($st.validator // "") as $me
| if $me == "" then "reports no validator of its own (not a validator node?)"
  elif $st.jailed == true then "!is jailed: it leaves the active set at the next epoch (unjail it once its jail is over: deploy/README.md, Day 2)"
  elif $start != null and ($st.started_at // 0) <= $start then "still runs the process it ran before the restart"
  elif ([$pub.validators[]? | if type == "string" then . else .address end] | map(select(. == $me)) | length) == 0 then "is not in the active set (\($n) validators)"
  elif $st.voting != true then "is not voting (not in the set of the epoch it has reached)"
  elif $st.abstaining != false then "is not signing yet (slashing protection watches the network after a start)"
  elif $n > 1 and ($st.p2p_active_peers // -1) < $n - 1 then "is linked to \($st.p2p_active_peers // 0) of the \($n - 1) other active validators"
  elif $st.syncing != false then "is catching up (at #\($st.height))"
  elif $start == null then "ok"
  else ([$blocks[] | select(.p == $me and .t >= $st.started_at * 1000) | .h] | min) as $first
    | if $first == null then "has not proposed a block since its restart" else "ok #\($first)" end
  end'

# Milliseconds as seconds, one decimal.
gate_secs() { printf '%d.%d' $(($1 / 1000)) $((($1 % 1000) / 100)); }

# Whether $1 reads as a process start time (unix seconds, as jq prints them).
gate_is_time() { [[ ${1:-} =~ ^[0-9]+(\.[0-9]+)?$ ]]; }

# Whether status JSON $2 (from $1) is for this genesis: 1 without a genesis hash, 2 for another.
gate_genesis() { # who json
  local g=""
  [ -n "${GENESIS_HASH:-}" ] || { GATE_WHY="no genesis hash to check $1 against"; return 2; }
  g=$(jq -r '.genesis_hash // ""' 2>/dev/null <<<"$2") || g=""
  case "$g" in
    "$GENESIS_HASH") return 0 ;;
    "") GATE_WHY="$1 reports no genesis hash"; return 1 ;;
    *) GATE_WHY="$1 serves another genesis ($g), not $GENESIS_HASH"; return 2 ;;
  esac
}

# The public node's /api/status into GATE_PUB: 1 when it doesn't answer, 2 for another genesis.
gate_public_status() {
  local out=""
  GATE_PUB=""
  if ! out=$(curl -fsS -m 10 "$RPC_URL/api/status" 2>/dev/null) || ! jq -e 'type == "object"' >/dev/null 2>&1 <<<"$out"; then
    GATE_WHY="the public node ($RPC_URL) doesn't answer /api/status"
    return 1
  fi
  gate_genesis "the public node ($RPC_URL)" "$out" || return $?
  GATE_PUB=$out
}

# Whether GATE_PUB's newest block is fresh.
gate_head_fresh() {
  local why
  why=$(jq -r --argjson max "$((HEAD_MAX_SECS * 1000))" 'if (.head_age_ms | type) != "number" then "the public node holds no block yet" elif .head_age_ms > $max then "the newest block on the public node is \(.head_age_ms / 1000 | floor) s old" else "" end' <<<"$GATE_PUB") || why="the public node's status can't be read"
  [ -z "$why" ] || { GATE_WHY=$why; return 1; }
}

# `blockula status` of the node on app $1's machine (its HTTP port $2) into GATE_STATUS: 1 when it
# doesn't answer, 2 for another genesis or a blockula without the command (waiting won't help).
gate_status() { # app port
  local out="" json="" last=""
  GATE_STATUS=""
  if out=$(fly ssh console -a "$1" -C "blockula status --rpc http://127.0.0.1:$2" 2>&1); then
    json=$(printf '%s\n' "$out" | tr -d '\r' | grep '^{' | tail -n 1) || json=""
  fi
  if [ -z "$json" ] || ! jq -e 'type == "object"' >/dev/null 2>&1 <<<"$json"; then
    case "$out" in
      *"unrecognized subcommand"*)
        GATE_WHY="$1 runs a blockula without \`blockula status\` (older than this script): deploy this checkout first (deploy/launch.sh)"
        return 2 ;;
    esac
    # The last line: the root cause of an error chain ("    3: Connection refused").
    last=$(printf '%s\n' "$out" | tr -d '\r' | sed '/^[[:space:]]*$/d' | tail -n 1 | sed -E 's/^[[:space:]]*([0-9]+: )?//') || last=""
    GATE_WHY="$1 doesn't answer \`blockula status\` on its machine (${last:-no output})"
    return 1
  fi
  gate_genesis "$1" "$json" || return $?
  GATE_STATUS=$json
}

# One round of reads: 0 when the network is at full participation and every validator named
# INDEX=START (its process start time before its restart) is back; else 1 (2: waiting can't help),
# with GATE_WHY. Records every validator's process start time in GATE_STARTED.
gate_check() { # [INDEX=START …]
  local blocks="" span n i a start order="" why proposed="" mine="" others=""
  GATE_WHY="" GATE_OK=""
  for a in "$@"; do
    gate_is_time "${a#*=}" || { GATE_WHY="no process start time for $PREFIX-v${a%%=*} from before its restart"; return 2; }
  done
  gate_public_status || return $?
  gate_head_fresh || return 1
  # The active set, as a count that matches its list: what the link check counts against.
  n=$(jq -r 'if (.active_validators | type) == "number" and (.validators | type) == "array" and (.validators | length) == .active_validators then .active_validators else "" end' <<<"$GATE_PUB") || n=""
  case "$n" in "" | *[!0-9]*) GATE_WHY="the public node's status doesn't list the active set"; return 1 ;; esac
  if [ "$n" -gt "$N" ]; then
    GATE_WHY="the active set has $n validators, more than the $N this script runs: it can't tell whether the others sign or are linked, so it won't restart anything (restart the validators together instead: deploy/README.md, Restarting validators)"
    return 2
  fi
  if ! blocks=$(curl -fsS -m 10 "$RPC_URL/api/blocks?limit=$PACE_BLOCKS" 2>/dev/null) \
    || ! blocks=$(jq -c '[.[] | {h: .height, t: .timestamp_ms, p: (.proposer | if type == "string" then . else .address end)}]' 2>/dev/null <<<"$blocks"); then
    GATE_WHY="the public node ($RPC_URL) doesn't answer /api/blocks"
    return 1
  fi
  span=$(jq -r --argjson want "$PACE_BLOCKS" 'if length < $want then -1 else (map(.t) | max - min | floor) end' 2>/dev/null <<<"$blocks") || span=""
  case "$span" in "" | *[!0-9-]* | ?*-*) GATE_WHY="the public node's recent blocks carry no times the gate can read"; return 1 ;; esac
  if [ "$span" -lt 0 ]; then GATE_WHY="the public node holds fewer than $PACE_BLOCKS blocks"; return 1; fi
  if [ "$span" -gt $((PACE_SECS * 1000)) ]; then
    GATE_WHY="the last $PACE_BLOCKS blocks took $(gate_secs "$span") s, not full pace (at most $PACE_SECS s)"
    return 1
  fi
  # The validators just restarted first, then the others.
  for a in "$@"; do order="$order ${a%%=*}"; done
  for i in $(seq 0 $((N - 1))); do
    case " $order " in *" $i "*) ;; *) order="$order $i" ;; esac
  done
  for i in $order; do
    start=null
    for a in "$@"; do [ "${a%%=*}" != "$i" ] || start=${a#*=}; done
    gate_status "$PREFIX-v$i" "$HTTP_PORT" || return $?
    GATE_STARTED[i]=$(jq -r '.started_at // ""' <<<"$GATE_STATUS")
    mine="$mine $(jq -r '.validator // ""' <<<"$GATE_STATUS")"
    why=$(jq -r --argjson pub "$GATE_PUB" --argjson blocks "$blocks" --argjson start "$start" "$GATE_JQ" <<<"$GATE_STATUS") || why="gave a status the gate can't read"
    case "$why" in
      ok) ;;
      "ok #"*) proposed="$proposed; $PREFIX-v$i proposed ${why#ok } since its restart" ;;
      "!"*) GATE_WHY="$PREFIX-v$i ${why#!}"; return 2 ;;
      *) GATE_WHY="$PREFIX-v$i $why"; return 1 ;;
    esac
  done
  # Each of ours is in the active set and it has at most N members, so it's ours alone, unless two
  # of our apps report one validator (one key on two machines): then someone else's is unread.
  others=$(jq -r --arg mine "$mine" '[.validators[]? | if type == "string" then . else .address end] - ($mine | split(" ")) | join(", ")' <<<"$GATE_PUB") || others="?"
  if [ -n "$others" ]; then
    GATE_WHY="the active set also has validators this script doesn't run ($others), and two of its apps report the same validator: it won't restart anything"
    return 2
  fi
  GATE_OK="all $N validators sign, each linked to the $((n - 1)) other active ones; the last $PACE_BLOCKS blocks took $(gate_secs "$span") s$proposed"
}

# The public node answers for this genesis, and so does each node named (INDEX: a validator, rpc:
# the public node) on its own machine. Records their process start times (GATE_STARTED,
# GATE_RPC_STARTED): what a restart's gate tells their new processes from the old ones by.
gate_answering() { # INDEX|rpc …
  local x t app
  GATE_WHY="" GATE_OK=""
  gate_public_status || return $?
  for x in "$@"; do
    if [ "$x" = rpc ]; then gate_status "$RPC_APP" "$RPC_PORT" || return $?; else gate_status "$PREFIX-v$x" "$HTTP_PORT" || return $?; fi
    t=$(jq -r '.started_at // ""' <<<"$GATE_STATUS")
    if [ "$x" = rpc ]; then app=$RPC_APP; else app=$PREFIX-v$x; fi
    gate_is_time "$t" || { GATE_WHY="$app reports no process start time"; return 1; }
    if [ "$x" = rpc ]; then GATE_RPC_STARTED=$t; else GATE_STARTED[x]=$t; fi
  done
  GATE_OK="the public node answers, and so do the $# nodes to restart"
}

# The public node after its own restart (START: its process start time before): its status on its
# machine shows a new process that isn't catching up, and it answers at RPC_URL for this genesis
# with a fresh head.
gate_public_back() { # START
  local why
  GATE_WHY="" GATE_OK=""
  gate_is_time "${1:-}" || { GATE_WHY="no process start time for $RPC_APP from before its restart"; return 2; }
  gate_status "$RPC_APP" "$RPC_PORT" || return $?
  why=$(jq -r --argjson start "$1" 'if (.started_at // 0) <= $start then "still runs the process it ran before the restart" elif .syncing != false then "is catching up (at #\(.height))" else "" end' <<<"$GATE_STATUS") || why="gave a status the gate can't read"
  [ -z "$why" ] || { GATE_WHY="$RPC_APP $why"; return 1; }
  gate_public_status || return $?
  gate_head_fresh || return 1
  GATE_OK="$RPC_APP runs a new process and serves #$(jq -r '.height' <<<"$GATE_PUB") at $RPC_URL"
}

# Runs CHECK (gate_check, gate_answering or gate_public_back) with ARGS every PROGRESS_SECS until
# it passes: 0. Gives up after GATE_SECS (or GATE_ROUNDS checks), or at once when waiting can't
# help: 1, with GATE_WHY. Says what it's waiting for when that changes, and every 30 s.
gate_wait() { # WHAT CHECK [ARGS…]
  local what=$1 check=$2 t0=$SECONDS rounds=0 last="" said=$SECONDS kind rc v
  shift 2
  # Whole numbers only: a bad GATE_SECS would never time out, a bad N would read no validators.
  for v in N GATE_SECS PROGRESS_SECS PACE_BLOCKS PACE_SECS HEAD_MAX_SECS; do
    [[ ${!v:-} =~ ^[0-9]+$ ]] || { GATE_WHY="$v must be a whole number (got '${!v:-}')"; return 1; }
  done
  [ "$N" -gt 0 ] || { GATE_WHY="N must be at least 1"; return 1; }
  [[ ${GATE_ROUNDS:-0} =~ ^[0-9]+$ ]] || { GATE_WHY="GATE_ROUNDS must be a whole number (got '$GATE_ROUNDS')"; return 1; }
  while :; do
    rc=0
    "$check" "$@" || rc=$?
    if [ "$rc" = 0 ]; then
      echo "   ✓ $what: $GATE_OK ($((SECONDS - t0)) s)"
      return 0
    fi
    [ "$rc" != 2 ] || return 1
    rounds=$((rounds + 1))
    kind=$(printf '%s' "$GATE_WHY" | tr -d '0-9.')
    if [ "$kind" != "$last" ] || [ $((SECONDS - said)) -ge 30 ]; then
      echo "   … $what: $GATE_WHY ($((SECONDS - t0)) s)"
      last=$kind said=$SECONDS
    fi
    if [ $((SECONDS - t0)) -ge "$GATE_SECS" ] || { [ -n "${GATE_ROUNDS:-}" ] && [ "$rounds" -ge "$GATE_ROUNDS" ]; }; then
      GATE_WHY="$GATE_WHY (still, after $((SECONDS - t0)) s)"
      return 1
    fi
    sleep "$PROGRESS_SECS"
  done
}
