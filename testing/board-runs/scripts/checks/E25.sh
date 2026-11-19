# E25 section 4 item 6, detached (sourced by lemnos-check.sh run), on the image's own gadget.
#   STEPS   "stop kill" (default) or one of them
# Two lemnosd down windows, each bounded by DOWN_BOUND_S (default 5 s):
#   stop  `systemctl stop`: what the stop leaves in configfs and the mounts, then a start.
#   kill  SIGKILL to lemnosd's main process through systemd (exact unit name): what the kernel
#         does when ep0 closes with no teardown; the unit's Restart=always (RestartSec=1s) brings
#         lemnosd back, and the check starts it itself if that has not happened within 3 s.
# Item 7 (start without the FunctionFS mount) is not run: the gadget helper mounts FunctionFS
# itself at every setup (service/lemnosd/src/helper.rs `setup`), so "start without the mount"
# no longer exists as a state; V1g check 3 covers the start with configfs missing.

STEPS=${STEPS:-stop kill}

CHECK_HYPOTHESIS="E25 4.6 on the image's gadget. Stop: lemnosd's stop sends teardown for every gadget (service/lemnosd/src/main.rs), so UDC is unbound, configs/, strings/ and the functions are removed and FunctionFS is unmounted; the record's older text (function directory and mount left) is expected NOT to hold. SIGKILL: no teardown runs; closing ep0 makes the kernel unbind (UDC empty) while the tree and the mount stay; systemd starts lemnosd again after 1 s and the gadget is bound again with the leftovers adopted or replaced. Each lemnosd down window stays under $DOWN_BOUND_S s."

check_precheck() {
  GDEV=$(gadget_device)
  [ -n "$GDEV" ] || { log precheck "no usb-gadget device in lemnos-ctl list"; return 1; }
  UDC=$(gadget_udc)
  [ -n "$UDC" ] || { log precheck "the gadget is not bound (UDC empty)"; return 1; }
  log precheck "gadget device '$GDEV', $(gadget_dir), UDC '$UDC'"
  gadget_snapshot precheck
}

e25_stop() {
  local dns d n=0
  window_open stop "$DOWN_BOUND_S" || { result stop NOT-RUN "the timed restore could not be armed"; return; }
  dns=$(did_not_stop_count)
  stop_lemnosd stop
  gadget_snapshot stop
  local tree=present mount=present
  d=$(gadget_dir) || tree=gone
  [ "$tree" = present ] && [ -z "$(ls "$d/configs" 2>/dev/null)" ] && [ -z "$(cat "$d/UDC" 2>/dev/null)" ] && tree="unbound-and-empty"
  [ -z "$(ffs_mounts)" ] && mount=gone
  start_lemnosd stop
  window_close stop
  watchdog_account stop "$STOP_T" "$ACTIVE_T" "$dns"
  while [ "$(gadget_udc)" != "$UDC" ] && [ "$n" -lt 100 ]; do sleep 0.1; n=$((n + 1)); done
  gadget_snapshot stop-after
  local rebound=no
  [ "$(gadget_udc)" = "$UDC" ] && rebound="yes, $(since "$ACTIVE_T") s after active"
  if [ "$tree" != present ] && [ "$mount" = gone ] && [ "$rebound" != no ]; then
    result stop PASS "after stop: tree $tree, FunctionFS $mount (the record's 'directory and mount left' does not hold, as the code says); bound again: $rebound"
  else
    result stop FAIL "after stop: tree $tree, FunctionFS $mount; bound again: $rebound"
  fi
}

e25_kill() {
  local dns pid t0 n=0 empty_at="" new_pid="" new_at="" d
  window_open kill "$DOWN_BOUND_S" || { result kill NOT-RUN "the timed restore could not be armed"; return; }
  dns=$(did_not_stop_count)
  pid=$(main_pid "$LEMNOSD_UNIT")
  t0=$(mono)
  STOP_T=$t0
  log kill "SIGKILL to $LEMNOSD_UNIT's main process (pid $pid) through systemctl kill"
  bounded 5 systemctl kill --kill-whom=main --signal=SIGKILL "$LEMNOSD_UNIT"
  log kill "systemctl kill exit $?"
  # 50 ms samples for up to 3 s: the kernel's unbind, then systemd's restart.
  while [ "$n" -lt 60 ]; do
    if [ -z "$empty_at" ] && [ -z "$(gadget_udc)" ]; then
      empty_at=$(since "$t0")
      gadget_snapshot kill
    fi
    local p
    p=$(main_pid "$LEMNOSD_UNIT")
    if [ -z "$new_pid" ] && [ -n "$p" ] && [ "$p" != 0 ] && [ "$p" != "$pid" ]; then
      new_pid=$p
      new_at=$(since "$t0")
    fi
    [ -n "$new_pid" ] && unit_active "$LEMNOSD_UNIT" && break
    sleep 0.05
    n=$((n + 1))
  done
  [ -z "$empty_at" ] && gadget_snapshot kill-no-unbind-seen
  log kill "UDC empty at: ${empty_at:-never seen} s; new main pid ${new_pid:-none} at ${new_at:-?} s; state '$(unit_state "$LEMNOSD_UNIT")'"
  start_lemnosd kill
  window_close kill
  watchdog_account kill "$STOP_T" "$ACTIVE_T" "$dns"
  n=0
  while [ "$(gadget_udc)" != "$UDC" ] && [ "$n" -lt 100 ]; do sleep 0.1; n=$((n + 1)); done
  gadget_snapshot kill-after
  local rebound=no
  [ "$(gadget_udc)" = "$UDC" ] && rebound=yes
  d=$(gadget_dir) && log kill "tree after the restart: $d"
  if [ -n "$empty_at" ] && [ "$rebound" = yes ]; then
    result kill PASS "kernel unbound the gadget ${empty_at} s after SIGKILL; restarted by systemd ${new_at:-(by the check)} s; bound again"
  else
    result kill FAIL "UDC empty seen: ${empty_at:-no}; restarted by systemd: ${new_at:-no}; bound again: $rebound"
  fi
}

check_steps() {
  local s
  for s in $STEPS; do
    case $s in
    stop) e25_stop ;;
    kill) e25_kill ;;
    *) log run "unknown step '$s'" ;;
    esac
  done
  result 4.7 SUPERSEDED "no such state any more: the gadget helper mounts FunctionFS at every setup; see V1g check 3"
}
