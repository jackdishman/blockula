# shellcheck shell=bash
# Stand-ins for Fly and the network the deploy scripts talk to, sourced by deploy/test-deploy.sh:
# `write_standins DIR` writes `fly`, `curl` and `world`
# into DIR (put it first on PATH). They keep their state, and the log of every call, under
# $FAKE_FLY. Nothing here talks to Fly.
write_standins() {
  local bin=$1
  mkdir -p "$bin"
  # The stand-in fly keeps each app's secret names in $FAKE_FLY/<app>.secrets (never values),
  # staged ones in <app>.staged until the app's next deploy (FAKE_FLY_HIDE_STAGED: `secrets list`
  # doesn't show them), and logs every call; FAKE_FLY_BUILD_FAIL fails the image build,
  # FAKE_FLY_DEPLOY_FAIL=<app> that app's deploy (FAKE_FLY_NO_MACHINE=<app>: it succeeds, but
  # leaves no machine). Each deploy records whether the app held VALIDATOR_KEY_B64 once it applied
  # them. A deploy, or a secret change that isn't staged, restarts the app's node in the stand-in
  # network below; `ssh console -C "blockula status …"` asks it, and
  # `-C "test -f /data/home/.seeded"` fails for FAKE_UNSEEDED=<app> (its key isn't on its volume).
  # With FAKE_FLY_STATEFUL (for a launcher that reads back what exists) it also keeps apps
  # by org, volumes (with their creation time), IPs (dedicated IPv4s are 198.51.100.1, .2 … in
  # the order allocated), certificates and, once an app deployed, its machine (and the
  # configuration and arguments of its last deploy, <app>.toml and <app>.args; a second machine
  # when <app>.machine2 exists, as a scale or a clone would leave); and an image build keeps its
  # context and arguments in $FAKE_FLY/build. Without it apps, volumes and machines always read as
  # on the testnet's first launch.
  cat > "$bin/fly" <<'STUB'
#!/usr/bin/env bash
S=$FAKE_FLY
echo "$*" >> "$S/log"
app="" cfg="" cmd="" staged=""
args=("$@")
for ((k = 0; k < ${#args[@]}; k++)); do
  case ${args[$k]} in -a) app=${args[$((k + 1))]} ;; -c) cfg=${args[$((k + 1))]} ;; -C) cmd=${args[$((k + 1))]} ;; --stage) staged=1 ;; esac
done
secrets=$S/$app.secrets
if [ -n "${FAKE_FLY_STATEFUL:-}" ]; then
  org="" region="" size="" snaps="" shared=""
  for ((k = 0; k < ${#args[@]}; k++)); do
    case ${args[$k]} in --org) org=${args[$((k + 1))]} ;; -r) region=${args[$((k + 1))]} ;; -s) size=${args[$((k + 1))]} ;; --scheduled-snapshots=*) snaps=${args[$k]#*=} ;; --shared) shared=1 ;; esac
  done
  touch "$S/apps" "$S/$app.volumes" "$S/$app.ips" "$S/$app.certs"
  case "$1 ${2:-}" in
    "apps create") echo "$3 $org" >> "$S/apps"; exit 0 ;;
    "apps list") awk -v o="$org" '$2 == o { print $1 }' "$S/apps" | jq -Rn --arg o "$org" '[inputs | {Name: ., Organization: {Slug: $o}}]'; exit 0 ;;
    "volumes create")
      jq -cn --arg id "vol_${app}_$(($(wc -l < "$S/$app.volumes") + 1))" --arg r "$region" --argjson gb "$size" --arg s "$snaps" --arg t "$(date -u +%FT%T.000Z)" '{id: $id, name: "data", region: $r, size_gb: $gb, auto_backup_enabled: ($s != "false"), created_at: $t}' >> "$S/$app.volumes"; exit 0 ;;
    "volumes list") jq -s . "$S/$app.volumes"; exit 0 ;;
    "volumes update") jq -c --arg id "$3" --arg s "$snaps" 'if .id == $id and $s != "" then .auto_backup_enabled = ($s != "false") else . end' "$S/$app.volumes" > "$S/$app.v"; mv "$S/$app.v" "$S/$app.volumes"; exit 0 ;;
    "ips allocate-v4")
      if [ -n "$shared" ]; then echo "shared_v4 203.0.113.250" >> "$S/$app.ips"
      else n=$(($(cat "$S/ip-count" 2>/dev/null || echo 0) + 1)); echo "$n" > "$S/ip-count"; echo "v4 198.51.100.$n" >> "$S/$app.ips"; fi
      exit 0 ;;
    "ips allocate-v6") echo "v6 2001:db8::$(($(wc -l < "$S/$app.ips") + 1))" >> "$S/$app.ips"; exit 0 ;;
    "ips list") jq -Rn '[inputs | split(" ") | {Type: .[0], Address: .[1]}]' < "$S/$app.ips"; exit 0 ;;
    "machines list") for m in machine machine2; do [ ! -f "$S/$app.$m" ] || echo "{\"id\":\"m-$app-$m\"}"; done | jq -s .; exit 0 ;;
    "certs add") echo "$3" >> "$S/$app.certs"; exit 0 ;;
    "certs list") jq -Rn '[inputs | {Hostname: .}]' < "$S/$app.certs"; exit 0 ;;
  esac
fi
case "$1 ${2:-}" in
  "auth whoami") echo tester ;;
  "apps list") echo '[]' ;;
  "machines list") echo "[{\"id\":\"m-$app\"}]" ;;
  "volumes list") echo '[]' ;;
  "ips list") echo '[{"Type":"v4","Address":"203.0.113.7"}]' ;;
  "secrets list")
    touch "$secrets" "$S/$app.staged"
    { cat "$secrets"; [ -n "${FAKE_FLY_HIDE_STAGED:-}" ] || cat "$S/$app.staged"; } | jq -Rn '[inputs | select(length > 0) | {name: .}]' ;;
  "secrets import")
    to=$secrets; [ -z "$staged" ] || to=$S/$app.staged
    touch "$to"; while IFS='=' read -r k _; do [ -z "$k" ] || grep -qx "$k" "$to" || echo "$k" >> "$to"; done
    [ -n "$staged" ] || world restart "$app" secrets ;;
  "secrets unset")
    for f in "$secrets" "$S/$app.staged"; do touch "$f"; { grep -vx "$3" "$f" || true; } > "$f.tmp"; mv "$f.tmp" "$f"; done
    [ -n "$staged" ] || world restart "$app" secrets ;;
  "ssh console")
    case $cmd in
      "blockula status "*) exec world status "$app" ;;
      "test -f /data/home/.seeded") [ "${FAKE_UNSEEDED:-}" != "$app" ] || exit 1 ;;
    esac ;;
  deploy*)
    if printf '%s\n' "$*" | grep -q -- --build-only; then
      [ -z "${FAKE_FLY_BUILD_FAIL:-}" ] || exit 1
      if [ -n "${FAKE_FLY_STATEFUL:-}" ]; then rm -rf "$S/build"; mkdir -p "$S/build"; cp -R "$2" "$S/build/context"; printf '%s\n' "$@" > "$S/build/args"; fi
      exit 0
    fi
    app=$(sed -nE 's/^app = "(.*)"/\1/p' "$cfg")
    touch "$S/$app.secrets" "$S/$app.staged"
    sort -u "$S/$app.secrets" "$S/$app.staged" > "$S/$app.tmp"; mv "$S/$app.tmp" "$S/$app.secrets"; : > "$S/$app.staged"
    echo "$app key=$(grep -qx VALIDATOR_KEY_B64 "$S/$app.secrets" && echo yes || echo no)" >> "$S/deploys"
    [ "${FAKE_FLY_DEPLOY_FAIL:-}" != "$app" ] || exit 1
    [ -z "${FAKE_FLY_STATEFUL:-}" ] || { [ "${FAKE_FLY_NO_MACHINE:-}" = "$app" ] || touch "$S/$app.machine"; cp "$cfg" "$S/$app.toml"; printf '%s\n' "$@" > "$S/$app.args"; }
    world restart "$app" deploy ;;
esac
exit 0
STUB
  # The stand-in curl: the public node's /api/status and /api/blocks from the stand-in network;
  # GitHub accepts.
  cat > "$bin/curl" <<'STUB'
#!/usr/bin/env bash
for a; do url=$a; done
case $url in
  */api/status) exec world pub-status ;;
  */api/blocks*) exec world blocks "$(printf '%s' "$url" | sed -n 's/.*limit=\([0-9]*\).*/\1/p')" ;;
  */api/health) echo '{}' ;;
  https://api.github.com/*) cat > /dev/null; printf 'HTTP/2 200 \r\ncontent-type: application/json\r\n\r\n' ;;
  *) exit 7 ;;
esac
STUB
  # The stand-in network: four validators (blockula-v0 …, FAKE_N) and the public node, which move
  # only when read. Each read of the public node's status adds 3 blocks (none with FAKE_STALL, or
  # after the first restart with FAKE_STALL_AFTER_RESTART), proposed in turn by the validators that
  # can, 1 s apart (3 s for the FAKE_SLOW_BLOCKS after each restart). A restarted validator gets a
  # new process start time, abstains for its first FAKE_ABSTAIN_READS status reads and proposes only
  # once it signs and has been read FAKE_PROPOSE_AFTER times; FAKE_UNLINKED reports a peer fewer for
  # FAKE_UNLINKED_READS reads after another validator's restart (FAKE_CANDIDATE_LINKS: every validator
  # is also linked to that many peers outside the active set, which `p2p_peers` counts and
  # `p2p_active_peers` doesn't; FAKE_SYNCING_NULL=<app> reports no `syncing`); FAKE_DOWN_AFTER_RESTART doesn't
  # answer once restarted, FAKE_LOCAL_DOWN never; FAKE_OTHER_GENESIS=<app> reports another genesis
  # and FAKE_NO_STATUS runs a blockula without `status`; with FAKE_RESTART_LAG a restart takes effect
  # only at the validator's Nth status read after it (its old process answers, and proposes, until
  # then: Fly's command can return before the machine restarts); FAKE_NO_START=<app> reports no
  # process start time, FAKE_NO_VALIDATOR=<app> no validator of its own (its key isn't on the chain
  # it follows). Validators no script restarted: FAKE_ABSTAINING abstains for its first
  # FAKE_ABSTAINING_READS reads (it restarted on its own), FAKE_SYNCING catches up for its first
  # FAKE_SYNCING_READS reads (behind the chain, linked, its slashing protection open: it can't vote
  # at the tip), FAKE_OUT_OF_SET isn't in the active set (jailed), FAKE_NOT_VOTING is by the chain's
  # count but its node says it doesn't vote (behind, in an epoch it isn't in), FAKE_JAILED is jailed
  # (still in the set until the epoch ends), FAKE_OUTSIDER puts a validator no script runs in the
  # active set (tboo1outsider), which with FAKE_OUTSIDER_PROPOSES leads a turn of the rotation and
  # is linked to ours (unless FAKE_OUTSIDER_DOWN: it never proposes). FAKE_PREFIX names the
  # validators' apps (blockula). The public node: FAKE_ACTIVE_UNLISTED (no `active_validators`), FAKE_PUBLIC_DOWN,
  # FAKE_PUBLIC_DOWN_AFTER (its first N status reads answer), FAKE_PUBLIC_FAIL_EVERY (every Nth
  # fails), FAKE_PUBLIC_EMPTY (no block yet), FAKE_FEW_BLOCKS (it holds only that many),
  # FAKE_BLOCKS_DOWN (/api/blocks fails), FAKE_NO_TIMES (blocks without times), FAKE_GENESIS; after
  # its own restart, for its first FAKE_RPC_SLOW_READS reads, FAKE_RPC_SLOW=lag (its old process
  # still answers on its machine), syncing (its new one catches up), down (RPC_URL doesn't answer) or
  # stale (RPC_URL serves a minute-old head). Its reads are logged: `status blockula-testnet
  # process=old|new syncing=…` on its machine, `pub-status age=<ms>|down` at RPC_URL. Every restart
  # is logged with the validators that weren't back yet: signing, linked and proposed since their
  # own restart (what the network knows, whatever a script saw).
  #   world init | restart <app> <why> | status <app> | pub-status | blocks <limit>
  cat > "$bin/world" <<'STUB'
#!/usr/bin/env bash
set -u
W=$FAKE_FLY/world
N=${FAKE_N:-4}
GEN=${FAKE_GENESIS:-$(printf 'ab%.0s' $(seq 32))}
get() { local v=${2:-0}; [ ! -f "$W/$1" ] || read -r v < "$W/$1"; echo "$v"; }
put() { echo "$2" > "$W/$1"; }
idx() { case $1 in *-v[0-9]*) echo "${1##*-v}" ;; *) echo rpc ;; esac; }
name() { echo "${FAKE_PREFIX:-blockula}-v$1"; }
LEADS=0; [ -z "${FAKE_OUTSIDER_PROPOSES:-}" ] || LEADS=1 # the outsider's turn in the rotation: index N
restarted() { [ "$(get "v$1.restarted")" = 1 ]; }
down() { restarted "$1" && [ "${FAKE_DOWN_AFTER_RESTART:-}" = "$(name "$1")" ]; }
out() { [ "${FAKE_OUT_OF_SET:-}" = "$(name "$1")" ] || [ "${FAKE_NOT_VOTING:-}" = "$(name "$1")" ] || [ "${FAKE_NO_VALIDATOR:-}" = "$(name "$1")" ]; }
abstains() { # index: watching the network after a start (its own, or one no script made)
  local r; r=$(get "v$1.reads")
  { restarted "$1" && [ "$r" -le "${FAKE_ABSTAIN_READS:-0}" ]; } || { [ "${FAKE_ABSTAINING:-}" = "$(name "$1")" ] && [ "$r" -le "${FAKE_ABSTAINING_READS:-0}" ]; }
}
syncs() { [ "${FAKE_SYNCING:-}" = "$(name "$1")" ] && [ "$(get "v$1.reads")" -le "${FAKE_SYNCING_READS:-0}" ]; }
signs() { ! out "$1" && ! down "$1" && ! abstains "$1" && ! syncs "$1"; }
jailed() { [ "${FAKE_JAILED:-}" = "$(name "$1")" ]; }
pending() { [ "$(get "v$1.pending")" -gt 0 ]; }
back() { ! pending "$1" && ! jailed "$1" && signs "$1" && { ! restarted "$1" || [ "$(get "v$1.proposed")" = 1 ]; }; }
can_propose() { signs "$1" && { ! restarted "$1" || [ "$(get "v$1.reads")" -ge "${FAKE_PROPOSE_AFTER:-0}" ]; }; }
stalled() { [ -n "${FAKE_STALL:-}" ] || [ "$(get stalled)" = 1 ]; }
init() {
  rm -rf "$W"; mkdir -p "$W"; : > "$W/blocks"; : > "$W/log"
  put clock 1792000000000; put height 100; put rr 0; put pubcalls 0; put slow 0; put stalled 0; put unlinked 0
  put rpc.start 1791990000; put rpc.pending 0; put rpc.new 0; put rpc.slow 0
  local i h t=1791999981000
  for i in $(seq 0 $((N - 1))); do put "v$i.start" 1791990000; put "v$i.reads" 0; put "v$i.restarted" 0; put "v$i.proposed" 1; put "v$i.pending" 0; done
  for h in $(seq 81 100); do echo "$h $t $((h % N))" >> "$W/blocks"; t=$((t + 1000)); done
}
advance() { # blocks
  local k j c p h ms slow m=$((N + LEADS))
  ! stalled || return 0
  for k in $(seq "$1"); do
    p=""
    for j in $(seq 0 $((m - 1))); do
      c=$((($(get rr) + j) % m))
      if [ "$c" = "$N" ]; then [ -n "${FAKE_OUTSIDER_DOWN:-}" ] || { p=$c; break; }; elif can_propose "$c"; then p=$c; break; fi
    done
    [ -n "$p" ] || return 0
    put rr $(((p + 1) % m))
    ms=1000; slow=$(get slow)
    if [ "$slow" -gt 0 ]; then ms=3000; put slow $((slow - 1)); fi
    c=$(($(get clock) + ms)); put clock "$c"
    h=$(($(get height) + 1)); put height "$h"
    echo "$h $c $p" >> "$W/blocks"
    [ "$p" = "$N" ] || ! restarted "$p" || put "v$p.proposed" 1
  done
}
restart() { # app why
  local i j late=""
  i=$(idx "$1")
  for j in $(seq 0 $((N - 1))); do [ "$j" = "$i" ] || back "$j" || late="$late $(name "$j")"; done
  [ "$(get unlinked)" = 0 ] || late="$late (a link of ${FAKE_UNLINKED:-})"
  echo "restart $1 ($2)${late:+ while not back:$late}" >> "$W/log"
  if [ "$i" = rpc ] && [ "${FAKE_RPC_SLOW:-}" = lag ]; then put rpc.pending "${FAKE_RPC_SLOW_READS:-1}"
  elif [ "$i" != rpc ] && [ "${FAKE_RESTART_LAG:-0}" -gt 0 ]; then put "v$i.pending" "$FAKE_RESTART_LAG"
  else apply "$i"; fi
}
apply() { # index or rpc: the new process starts
  local i=$1 s key=v$1.start
  [ "$i" != rpc ] || key=rpc.start
  s=$(($(get clock) / 1000 + 1)); [ "$s" -gt "$(get "$key")" ] || s=$(($(get "$key") + 1))
  put "$key" "$s"
  if [ "$i" = rpc ]; then put rpc.new 1; put rpc.slow "${FAKE_RPC_SLOW_READS:-0}"; return 0; fi
  put "v$i.reads" 0; put "v$i.restarted" 1; put "v$i.proposed" 0; put slow "${FAKE_SLOW_BLOCKS:-0}"
  if [ -n "${FAKE_UNLINKED:-}" ] && [ "$FAKE_UNLINKED" != "$(name "$i")" ]; then put unlinked "${FAKE_UNLINKED_READS:-0}"; fi
  [ -z "${FAKE_STALL_AFTER_RESTART:-}" ] || put stalled 1
}
status() { # app
  local i g=$GEN h r abstaining=false voting=true syncing=false peers=$((N - 1 + LEADS)) u st
  i=$(idx "$1")
  if [ "${FAKE_LOCAL_DOWN:-}" = "$1" ]; then echo "Error: ssh: no route to $1" >&2; return 1; fi
  if [ -n "${FAKE_NO_STATUS:-}" ]; then echo "error: unrecognized subcommand 'status'" >&2; return 2; fi
  [ "${FAKE_OTHER_GENESIS:-}" != "$1" ] || g=$(printf 'cd%.0s' $(seq 32))
  h=$(get height)
  if [ "$i" = rpc ]; then
    u=$(get rpc.pending); if [ "$u" -gt 0 ]; then put rpc.pending $((u - 1)); [ "$u" -gt 1 ] || apply rpc; fi
    u=$(get rpc.slow); if [ "${FAKE_RPC_SLOW:-}" = syncing ] && [ "$u" -gt 0 ]; then syncing=true; put rpc.slow $((u - 1)); fi
    st=$(get rpc.start); [ "${FAKE_NO_START:-}" != "$1" ] || st=null
    echo "status $1 process=$([ "$(get rpc.new)" = 1 ] && echo new || echo old) syncing=$syncing" >> "$W/log"
    echo "{\"genesis_hash\":\"$g\",\"role\":\"full_node\",\"height\":$h,\"syncing\":$syncing,\"head_age_ms\":500,\"validator\":null,\"started_at\":$st,\"p2p_peers\":null,\"p2p_active_peers\":null,\"voting\":null,\"abstaining\":null,\"signing\":null}"
    return 0
  fi
  if pending "$i"; then u=$(($(get "v$i.pending") - 1)); put "v$i.pending" "$u"; [ "$u" -gt 0 ] || apply "$i"; fi
  if down "$i"; then echo "Error: no started machine in $1" >&2; return 1; fi
  r=$(($(get "v$i.reads") + 1)); put "v$i.reads" "$r"
  if out "$i"; then voting=false; elif abstains "$i"; then abstaining=true; fi
  ! syncs "$i" || syncing=true
  [ "${FAKE_SYNCING_NULL:-}" != "$1" ] || syncing=null
  if [ "${FAKE_UNLINKED:-}" = "$1" ]; then u=$(get unlinked); if [ "$u" -gt 0 ]; then peers=$((peers - 1)); put unlinked $((u - 1)); fi; fi
  st=$(get "v$i.start"); [ "${FAKE_NO_START:-}" != "$1" ] || st=null
  me="\"tboo1fake$i\""; [ "${FAKE_NO_VALIDATOR:-}" != "$1" ] || me=null
  echo "status $1 abstaining=$abstaining peers=$peers syncing=$syncing" >> "$W/log"
  echo "{\"chain_id\":\"blockula-testnet-2\",\"genesis_hash\":\"$g\",\"role\":\"validator\",\"height\":$h,\"finalized_height\":$h,\"syncing\":$syncing,\"epoch\":0,\"active_validators\":$(active),\"head_age_ms\":500,\"validator\":$me,\"started_at\":$st,\"p2p_peers\":$((peers + ${FAKE_CANDIDATE_LINKS:-0})),\"p2p_active_peers\":$peers,\"voting\":$voting,\"abstaining\":$abstaining,\"signing\":$(signs "$i" && echo true || echo false),\"missed_slots\":0,\"jailed\":$(jailed "$i" && echo true || echo false),\"halted\":false}"
}
public_down() { # read number
  [ -z "${FAKE_PUBLIC_DOWN:-}" ] || return 0
  [ -z "${FAKE_PUBLIC_DOWN_AFTER:-}" ] || [ "$1" -le "$FAKE_PUBLIC_DOWN_AFTER" ] || return 0
  [ -z "${FAKE_PUBLIC_FAIL_EVERY:-}" ] || [ $(($1 % FAKE_PUBLIC_FAIL_EVERY)) != 0 ] || return 0
  return 1
}
active() { local n=$N; [ -z "${FAKE_OUT_OF_SET:-}" ] || n=$((n - 1)); [ -z "${FAKE_OUTSIDER:-}" ] || n=$((n + 1)); echo "$n"; }
listed() { [ "${FAKE_OUT_OF_SET:-}" != "$(name "$1")" ]; }
pub_status() {
  local n vs="" j age=500 slow
  n=$(($(get pubcalls) + 1)); put pubcalls "$n"
  slow=$(get rpc.slow)
  if public_down "$n" || { [ "${FAKE_RPC_SLOW:-}" = down ] && [ "$slow" -gt 0 ]; }; then
    [ "${FAKE_RPC_SLOW:-}" != down ] || [ "$slow" = 0 ] || put rpc.slow $((slow - 1))
    echo "pub-status down" >> "$W/log"; echo "curl: (22) The requested URL returned error: 503" >&2; return 22
  fi
  for j in $(seq 0 $((N - 1))); do ! listed "$j" || vs="$vs${vs:+,}{\"address\":\"tboo1fake$j\"}"; done
  [ -z "${FAKE_OUTSIDER:-}" ] || vs="$vs,{\"address\":\"tboo1outsider\"}"
  if [ -n "${FAKE_ACTIVE_UNLISTED:-}" ]; then
    advance 3; echo "pub-status age=500" >> "$W/log"
    echo "{\"height\":$(get height),\"genesis_hash\":\"$GEN\",\"validators\":[$vs],\"head_age_ms\":500}"; return 0
  fi
  if [ -n "${FAKE_PUBLIC_EMPTY:-}" ]; then echo "{\"height\":0,\"genesis_hash\":\"$GEN\",\"active_validators\":$(active),\"validators\":[$vs],\"head_age_ms\":null}"; return 0; fi
  advance 3
  ! stalled || age=60000
  if [ "${FAKE_RPC_SLOW:-}" = stale ] && [ "$slow" -gt 0 ]; then age=60000; put rpc.slow $((slow - 1)); fi
  echo "pub-status age=$age" >> "$W/log"
  echo "{\"height\":$(get height),\"genesis_hash\":\"$GEN\",\"active_validators\":$(active),\"validators\":[$vs],\"head_age_ms\":$age}"
}
blocks() { # limit
  if [ -n "${FAKE_BLOCKS_DOWN:-}" ] || public_down "$(get pubcalls)"; then return 22; fi
  if [ -n "${FAKE_PUBLIC_EMPTY:-}" ]; then echo '[]'; return 0; fi
  tail -n "${FAKE_FEW_BLOCKS:-${1:-20}}" "$W/blocks" | awk -v t="${FAKE_NO_TIMES:+x}" -v o="$((LEADS ? N : -1))" '{ a[NR] = $0 } END { printf "["; for (i = NR; i >= 1; i--) { split(a[i], f, " "); printf "%s{\"height\":%s,\"timestamp_ms\":%s,\"proposer\":{\"address\":\"%s\"}}", (i < NR ? "," : ""), f[1], (t ? "null" : f[2]), (f[3] == o ? "tboo1outsider" : "tboo1fake" f[3]) } print "]" }'
}
case "${1:-}" in
  init) init ;;
  restart) restart "$2" "$3" ;;
  status) status "$2" ;;
  pub-status) pub_status ;;
  blocks) blocks "${2:-}" ;;
  *) echo "world: $*?" >&2; exit 64 ;;
esac
STUB
  chmod +x "$bin/fly" "$bin/curl" "$bin/world"
}
