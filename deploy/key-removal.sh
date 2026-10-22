# shellcheck shell=bash
# Removing each validator's consensus key from Fly once it's on the validator's volume: sourced by
# deploy/launch.sh (the testnet) and deploy/mainnet/launch-mainnet.sh, after their first deploy of
# the validators. deploy/test-deploy.sh and deploy/mainnet/test-launch-mainnet.sh test it against a
# stand-in fly.
#
# A consensus key reaches its validator as the Fly secret VALIDATOR_KEY_B64, which the entrypoint
# writes onto an empty volume once (deploy/entrypoint.sh). While the secret exists, any second
# machine started in that app (a scale, a clone, a replacement with an empty volume) would come up
# with the same key and double-sign: so the secret goes as soon as the key is on the volume, which
# restarts the validator without it.
#
# Not staged, so every validator still holding its key restarts without the variable: all of them
# together, once. One at a time is what nearly stalled the blockula-testnet-2 launch (each restart
# waited out its peers' reconnection cooldowns, minutes before public-nat's were shortened, and a
# gate that didn't wait for links took two of four validators out at once: no quorum, then a crawl
# that jailed one). Even gated, it would cost a minute or two and a few missed slots per
# validator, with the keys left in Fly until the last restart. Together, on a chain nobody uses yet,
# the chain pauses once (slashing protection signs again once it sees the network standing still),
# each validator misses about a slot, and they relink in one round. (Mainnet's launcher does it
# before genesis time, while there's no chain to pause.) Before it, the public node and each of
# them must answer (their process start times tell the gate their new processes from the old
# ones), or nothing restarts and the keys stay for a rerun; after it, the participation gate
# (deploy/restart-gate.sh, sourced first).
#
# Reads PREFIX and N (validators $PREFIX-v0 …), KEY_RERUN (the command that finishes the job after
# a failure, for the messages) and the gate's settings.

KEY_RERUN=${KEY_RERUN:-deploy/launch.sh (without RESET)}
SEEDING=() # validators (by index) that still hold VALIDATOR_KEY_B64 in Fly

# Whether app $1 has the Fly secret $2.
has_secret() { fly secrets list -a "$1" --json | jq -e --arg s "$2" 'map(.name // .Name) | index($s)' >/dev/null; }

# Which validators (by index) still hold their key in Fly, into SEEDING. Ask once the deploys
# applied the staged secrets (a staged secret may not be listed before).
keys_in_fly() {
  local i
  SEEDING=()
  for i in $(seq 0 $((N - 1))); do
    if has_secret "$PREFIX-v$i" VALIDATOR_KEY_B64; then SEEDING+=("$i"); fi
  done
}

# Removes VALIDATOR_KEY_B64 from each validator in SEEDING whose volume shows its key seeded,
# restarting them together, through the gate before and after. 0 when done (or nothing to do);
# else 1, having said why and what's left.
remove_seeded_keys() {
  local i k names="" failed="" REMOVE=() STARTS=() PIDS=()
  for i in ${SEEDING[@]+"${SEEDING[@]}"}; do
    if fly ssh console -a "$PREFIX-v$i" -C "test -f /data/home/.seeded" >/dev/null 2>&1; then
      REMOVE+=("$i")
    else
      echo "⚠️  $PREFIX-v$i: couldn't confirm the key was seeded; VALIDATOR_KEY_B64 left in place" >&2
    fi
  done
  [ ${#REMOVE[@]} -gt 0 ] || return 0
  for i in "${REMOVE[@]}"; do names="$names $PREFIX-v$i"; done
  echo "🔒 Removing the keys seeded on their volumes from Fly, restarting$names together…"
  gate_wait "before the restart" gate_answering "${REMOVE[@]}" \
    || { echo "❌ $GATE_WHY: nothing restarted, and the keys stay in Fly. Rerun $KEY_RERUN to remove them." >&2; return 1; }
  for i in "${REMOVE[@]}"; do STARTS+=("$i=${GATE_STARTED[$i]}"); done
  for i in "${REMOVE[@]}"; do fly secrets unset VALIDATOR_KEY_B64 -a "$PREFIX-v$i" >/dev/null & PIDS+=($!); done
  for k in $(seq 0 $((${#PIDS[@]} - 1))); do wait "${PIDS[$k]}" || failed="$failed $PREFIX-v${REMOVE[$k]}"; done
  [ -z "$failed" ] || { echo "❌ removing VALIDATOR_KEY_B64 from$failed failed (fly logs -a <app>): rerun $KEY_RERUN" >&2; return 1; }
  echo "🔒$names: VALIDATOR_KEY_B64 removed from Fly secrets (restarted)"
  gate_wait "after the restart" gate_check "${STARTS[@]}" \
    || { echo "❌ the validators aren't back at full participation after the key removal: $GATE_WHY. The keys are off Fly; check fly logs -a <app> before anything else restarts a validator." >&2; return 1; }
}
