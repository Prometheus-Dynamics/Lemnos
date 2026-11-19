# B20 steps 8 and 9, detached (sourced by lemnos-check.sh run). Settings:
#   COMM_UNIT      the comm core's unit (default orion-comm.service, as the image installs it)
#   STEPS          "8 9" (default) or one of them
#   ORION_CLI      Orion's command line (default /usr/bin/orion); ORION_ADDRESS its --address
# The command line is the one at the Orion rev the image pins (455a956e, crates/client/ctl):
# `orion [--address A] [--timeout MS] resource list [PREFIX]` prints one row per resource,
# `NAME  KIND  STATE  on NODE`; `orion topic read TOPIC` prints `FIELD = VALUE` lines, then
# `(its publisher is gone; these are the last values)` when the publisher has gone, or only
# `nobody has declared TOPIC` (exit 1) when the topic is withdrawn. Its own timeout is 5000 ms by
# default: --timeout 2000 keeps each call inside this script's 3 s bound, so a call that cannot
# reach the comm core fails by itself instead of being killed.
# Step 8 restarts the comm core (the login goes with it); lemnosd stays up, the watchdog is not
# involved. Step 9 stops lemnosd for one bounded window (DOWN_BOUND_S, default 5 s).

STEPS=${STEPS:-8 9}
ORION_CLI=${ORION_CLI:-/usr/bin/orion}
ORION_ADDRESS=${ORION_ADDRESS:-}
# Step 8: the record's 10 s for the resources to come back, the restart itself and margin.
B20_S8_BOUND=25

CHECK_HYPOTHESIS="B20 on the board. Step 8: the comm core restarted while lemnos-orion runs; within 10 s every lemnos. resource is back and the bridge logs 'connected to Orion node <name> again'. Step 9: lemnosd stopped; lemnos. resources are gone at once and a device's status topic says its publisher is gone, never running; within 30 s of lemnosd active again every device is back. lemnosd is down for at most $DOWN_BOUND_S s, inside half the watchdog period, so the board does not reset."

orion() {
  # shellcheck disable=SC2086
  bounded 3 "$ORION_CLI" ${ORION_ADDRESS:+--address "$ORION_ADDRESS"} --timeout 2000 "$@"
}

# resources: the number of lemnos. resources the comm core lists (-1 when the call fails).
resources() {
  local out
  out=$(orion resource list lemnos. 2>/dev/null) || { echo -1; return; }
  awk '$1 ~ /^lemnos\./ && $2 == "lemnos.device"' <<<"$out" | grep -c .
}

check_precheck() {
  local ok=0
  [ -n "$COMM_UNIT" ] || { log precheck "COMM_UNIT is empty: step 8 has no comm core to restart"; ok=1; }
  unit_active "$BRIDGE_UNIT" || { log precheck "$BRIDGE_UNIT is not active"; ok=1; }
  command -v "$ORION_CLI" >/dev/null 2>&1 || { log precheck "no '$ORION_CLI' command on the board"; ok=1; }
  [ "$ok" = 0 ] || return 1
  EXPECT=$(bounded 5 lemnos-ctl list 2>/dev/null | grep -c .)
  FIRST=$(bounded 5 lemnos-ctl list 2>/dev/null | awk 'NR == 1 { print $1 }')
  logout precheck orion resource list lemnos.
  log precheck "devices in lemnos-ctl list: $EXPECT; resources now: $(resources); status topic checked on '$FIRST'"
  [ "$(resources)" = "$EXPECT" ] || { log precheck "the resources do not match the devices before anything is touched"; return 1; }
}

b20_step8() {
  local t0 e0 back="" n=0 r
  window_open s8 "$B20_S8_BOUND" || { result s8 NOT-RUN "the timed restore could not be armed"; return; }
  e0=$(epoch)
  t0=$(mono)
  log s8 "restarting $COMM_UNIT (the runner's login ends here)"
  bounded 20 systemctl restart "$COMM_UNIT"
  log s8 "restart exit $? in $(since "$t0") s; $COMM_UNIT '$(unit_state "$COMM_UNIT")'"
  while [ "$n" -lt 60 ]; do
    r=$(resources)
    if [ "$r" = "$EXPECT" ]; then
      back=$(since "$t0")
      break
    fi
    sleep 0.25
    n=$((n + 1))
  done
  logout s8 orion resource list lemnos.
  restore_all s8
  window_close s8
  local again
  again=$(journal_since "$BRIDGE_UNIT" "$e0" | grep -E 'connected to Orion node .* again' | head -n1)
  log s8 "bridge log since the restart: ${again:-no 'again' line}"
  if [ -n "$back" ] && le "$back" 10 && [ -n "$again" ]; then
    result s8 PASS "all $EXPECT resources back $back s after the restart; '$again'"
  else
    result s8 FAIL "resources back after: ${back:-never within 15 s} (want <= 10 s); again line: ${again:-missing}"
  fi
}

b20_step9() {
  local dns gone topic t_active back="" n=0 r
  window_open s9 "$DOWN_BOUND_S" || { result s9 NOT-RUN "the timed restore could not be armed"; return; }
  dns=$(did_not_stop_count)
  stop_lemnosd s9
  gone=$(resources)
  topic=$(orion topic read "lemnos.$FIRST.status" 2>&1)
  log s9 "while down: resources $gone; topic read lemnos.$FIRST.status:"
  while IFS= read -r l; do log s9 "| $l"; done <<<"$topic"
  start_lemnosd s9
  window_close s9
  watchdog_account s9 "$STOP_T" "$ACTIVE_T" "$dns"
  t_active=$ACTIVE_T
  while [ "$n" -lt 120 ]; do
    r=$(resources)
    if [ "$r" = "$EXPECT" ]; then
      back=$(since "$t_active")
      break
    fi
    sleep 0.25
    n=$((n + 1))
  done
  logout s9 orion resource list lemnos.
  # The topic is gone in any of the three forms the pinned command line prints: withdrawn
  # (`nobody has declared`), its publisher gone, or the bridge's own `state = gone`. A
  # `state = running` line is a value from before: the record's "nothing shows running".
  local ok=1 topic_gone=no
  [ "$gone" = 0 ] || ok=0
  grep -Eq 'nobody has declared|its publisher is gone|^state = gone$' <<<"$topic" && topic_gone=yes
  grep -Eq '^state = running$' <<<"$topic" && topic_gone="no (shows running)"
  [ "$topic_gone" = yes ] || ok=0
  [ -n "$back" ] || ok=0
  if [ "$ok" = 1 ]; then
    result s9 PASS "resources while down: 0; status topic gone; all $EXPECT back $back s after lemnosd active"
  else
    result s9 FAIL "resources while down: $gone (want 0); status topic gone: $topic_gone; back after active: ${back:-not within 30 s}"
  fi
}

check_steps() {
  local s
  for s in $STEPS; do
    case $s in
    8) b20_step8 ;;
    9) b20_step9 ;;
    *) log run "unknown step '$s'" ;;
    esac
  done
}
