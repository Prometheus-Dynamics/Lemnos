# V1g, detached (sourced by lemnos-check.sh run). Settings:
#   STEPS   "4 5 6 3 1 2" (default, in this order) or any of them
# Every V1g check that changes the gadget cuts the link, lemnosd stopped or not:
#   4  read only (the two processes and their capabilities); first, while nothing is changed
#   5  the helper killed by its recorded PID five times (the gadget stays up), and the gadget's
#      device powered off and on (the link goes for that time); lemnosd stays up
#   6  `systemctl stop lemnosd`, then start: one lemnosd down window
#   3  lemnosd stopped, configfs unmounted, lemnosd started (a down window), then configfs mounted
#      and the gadget's own retry timed
#   1  lemnosd stopped, the controller's driver unbound, lemnosd started (a down window), then the
#      driver bound again and the uevent's attempt timed. This is the restart form of "boot with the
#      controller held back": no reboot is allowed.
#   2  only where the controller has a usb_role switch: role host, then device, timed
# Windows: DOWN_BOUND_S around each lemnosd stop; a longer one around each whole check (60 s for 5,
# 40 s for 3, 1 and 2) that also mounts configfs, binds the controller and sets the role back.

STEPS=${STEPS:-4 5 6 3 1 2}
V1G_LONG=40
V1G_HELPER_BOUND=60

CHECK_HYPOTHESIS="V1g on the board. 4: lemnosd-gsuper (lemnosd's child) and lemnosd-gadget (its child) run as uid 0; the helper's CapEff and CapPrm are 0x200000, NoNewPrivs 1, Seccomp 2; the parent's CapEff 0x200100, Seccomp 2; lemnosd's CapEff 0x20000. 5: a killed helper is forked again after 1 s, then 2, 4, 8 and 8 s; the gadget stays up; a power off and on is served by the new helper, which adopts the tree. 6: stop removes the tree and unmounts FunctionFS; start makes them again. 3: with configfs unmounted the gadget fails with its reason and lemnosd stays up; mounted again, the gadget is up on the driver's retry (at most 8 s). 1: with the controller unbound the gadget fails; bound again, it is up within 8 s, on the uevent. 2: as 1, across a role switch. Each lemnosd down window stays under $DOWN_BOUND_S s."

# children PPID COMM: the pids of PPID's children called COMM.
children() { ps -o pid=,comm= --ppid "$1" 2>/dev/null | awk -v c="$2" '$2 == c { print $1 }'; }
gsuper_pid() { children "$(main_pid "$LEMNOSD_UNIT")" lemnosd-gsuper | head -n1; }
helper_pid() { local g; g=$(gsuper_pid) && [ -n "$g" ] && children "$g" lemnosd-gadget | head -n1; }
status_field() { awk -v f="$2:" '$1 == f { print $2 }' "/proc/$1/status" 2>/dev/null; }

check_precheck() {
  GDEV=$(gadget_device)
  [ -n "$GDEV" ] || { log precheck "no usb-gadget device in lemnos-ctl list"; return 1; }
  UDC=$(gadget_udc)
  [ -n "$UDC" ] || { log precheck "the gadget is not bound"; return 1; }
  [ -n "$(helper_pid)" ] || { log precheck "no lemnosd-gadget helper under lemnosd-gsuper under lemnosd"; return 1; }
  log precheck "gadget '$GDEV' on '$UDC'; lemnosd $(main_pid "$LEMNOSD_UNIT"), gsuper $(gsuper_pid), helper $(helper_pid)"
}

wait_gadget_up() { # STEP SECONDS -> prints seconds taken, or nothing
  local t0 n=0 lim=$(($2 * 10))
  t0=$(mono)
  while [ "$n" -lt "$lim" ]; do
    if [ "$(gadget_udc)" = "$UDC" ] && device_running "$GDEV"; then since "$t0"; return 0; fi
    sleep 0.1
    n=$((n + 1))
  done
  return 1
}

v1g_4() {
  local lp gp hp ok=1 v
  lp=$(main_pid "$LEMNOSD_UNIT")
  gp=$(gsuper_pid)
  hp=$(helper_pid)
  logout c4 ps -o pid,ppid,user,comm -p "$lp,$gp,$hp"
  for p in "$lp" "$gp" "$hp"; do
    log c4 "pid $p $(cat "/proc/$p/comm" 2>/dev/null): Uid $(status_field "$p" Uid) CapEff $(status_field "$p" CapEff) CapPrm $(status_field "$p" CapPrm) NoNewPrivs $(status_field "$p" NoNewPrivs) Seccomp $(status_field "$p" Seccomp)"
  done
  v=$(status_field "$hp" CapEff); [ "$((16#${v:-0}))" = "$((16#200000))" ] || ok=0
  v=$(status_field "$hp" CapPrm); [ "$((16#${v:-0}))" = "$((16#200000))" ] || ok=0
  [ "$(status_field "$hp" NoNewPrivs)" = 1 ] || ok=0
  [ "$(status_field "$hp" Seccomp)" = 2 ] || ok=0
  v=$(status_field "$gp" CapEff); [ "$((16#${v:-0}))" = "$((16#200100))" ] || ok=0
  [ "$(status_field "$gp" Seccomp)" = 2 ] || ok=0
  v=$(status_field "$lp" CapEff); [ "$((16#${v:-0}))" = "$((16#20000))" ] || ok=0
  [ "$(status_field "$hp" Uid | cut -c1)" = 0 ] && [ "$(status_field "$gp" Uid | cut -c1)" = 0 ] || ok=0
  if [ "$ok" = 1 ]; then result c4 PASS "values as the record asks (lines above)"; else result c4 FAIL "values differ from the record (lines above)"; fi
}

# kill_helper K: kills the current helper by its recorded PID, after reading its cmdline and parent
# again and finding them unchanged. Prints the wait to the new helper. Sets KILLED_AT.
kill_helper() {
  local k=$1 hp gp cmd n=0 newp t0
  while [ -z "$(helper_pid)" ] && [ "$n" -lt 150 ]; do sleep 0.1; n=$((n + 1)); done
  hp=$(helper_pid)
  gp=$(gsuper_pid)
  [ -n "$hp" ] || { log c5 "kill $k: no helper to kill"; return 1; }
  cmd=$(tr '\0' ' ' <"/proc/$hp/cmdline" 2>/dev/null)
  printf 'pid %s ppid %s cmdline %s\n' "$hp" "$gp" "$cmd" >"$RUNDIR/c5-helper-$k.pid"
  if [ "$(tr '\0' ' ' <"/proc/$hp/cmdline" 2>/dev/null)" != "$cmd" ] || [ "$(awk '{ print $4 }' "/proc/$hp/stat" 2>/dev/null)" != "$gp" ]; then
    log c5 "kill $k: pid $hp changed between the two reads: not killed"
    return 1
  fi
  t0=$(mono)
  kill -KILL "$hp"
  log c5 "kill $k: SIGKILL to helper pid $hp (parent $gp, cmdline '$cmd')"
  n=0
  while [ "$n" -lt 150 ]; do
    newp=$(helper_pid)
    [ -n "$newp" ] && [ "$newp" != "$hp" ] && break
    newp=""
    sleep 0.1
    n=$((n + 1))
  done
  if [ -n "$newp" ]; then
    log c5 "kill $k: new helper pid $newp after $(since "$t0") s"
    since "$t0"
  else
    log c5 "kill $k: no new helper within 15 s"
  fi
}

v1g_5() {
  local e0 waits=() w k up
  window_open c5 "$V1G_HELPER_BOUND" || { result c5 NOT-RUN "the timed restore could not be armed"; return; }
  e0=$(epoch)
  w=$(kill_helper 1)
  waits+=("${w:-none}")
  logout c5 lemnos-ctl list
  # A power off and on of the gadget's device is served by the new helper (the link goes now).
  undo_set gadget-off "$GDEV"
  logout c5 lemnos-ctl power off "$GDEV"
  sleep 1
  logout c5 lemnos-ctl power on "$GDEV" && undo_clear gadget-off
  up=$(wait_gadget_up c5 15)
  log c5 "gadget up again after the power cycle: ${up:-NOT within 15 s}"
  for k in 2 3 4 5; do
    w=$(kill_helper "$k")
    waits+=("${w:-none}")
  done
  restore_all c5
  window_close c5
  local j
  j=$(journal_since "$LEMNOSD_UNIT" "$e0")
  grep -E 'gadget helper|adopted' <<<"$j" | while IFS= read -r l; do log c5 "| $l"; done
  log c5 "waits to a new helper: ${waits[*]} s (record: 1, 2, 4, 8, 8)"
  local ok=1 want=(1 2 4 8 8) i
  for i in 0 1 2 3 4; do
    w=${waits[$i]}
    [ "$w" != none ] && awk -v a="$w" -v b="${want[$i]}" 'BEGIN { exit !(a >= b - 0.2 && a <= b + 1) }' || ok=0
  done
  [ -n "$up" ] || ok=0
  grep -q 'adopted' <<<"$j" || ok=0
  if [ "$ok" = 1 ]; then result c5 PASS "waits ${waits[*]} s; power cycle served, tree adopted"; else result c5 FAIL "waits ${waits[*]} s (want 1 2 4 8 8); power cycle up: ${up:-no}; adopted line: $(grep -c adopted <<<"$j")"; fi
}

v1g_6() {
  local dns tree=present mount=present up
  window_open c6 "$DOWN_BOUND_S" || { result c6 NOT-RUN "the timed restore could not be armed"; return; }
  dns=$(did_not_stop_count)
  stop_lemnosd c6
  gadget_snapshot c6
  gadget_dir >/dev/null || tree=gone
  [ -z "$(ffs_mounts)" ] && mount=gone
  start_lemnosd c6
  window_close c6
  watchdog_account c6 "$STOP_T" "$ACTIVE_T" "$dns"
  up=$(wait_gadget_up c6 15)
  gadget_snapshot c6-after
  if [ "$tree" = gone ] && [ "$mount" = gone ] && [ -n "$up" ]; then
    result c6 PASS "stop: tree gone, FunctionFS unmounted; start: up again after $up s"
  else
    result c6 FAIL "stop: tree $tree, FunctionFS $mount; up again: ${up:-no}"
  fi
}

# down_change STEP DESCRIPTION CMD...: stops lemnosd, runs CMD (the change), starts lemnosd, inside
# one DOWN_BOUND_S window. Returns CMD's status.
down_change() {
  local step=$1 what=$2 dns rc
  shift 2
  window_open "$step-down" "$DOWN_BOUND_S" || return 99
  dns=$(did_not_stop_count)
  stop_lemnosd "$step"
  "$@"
  rc=$?
  log "$step" "$what: exit $rc"
  start_lemnosd "$step"
  window_close "$step-down"
  watchdog_account "$step" "$STOP_T" "$ACTIVE_T" "$dns"
  return $rc
}

umount_configfs() { bounded 5 umount "$SYSFS/kernel/config"; }

v1g_3() {
  local e0 t0 up rc
  window_open c3 "$V1G_LONG" || { result c3 NOT-RUN "the timed restore could not be armed"; return; }
  e0=$(epoch)
  undo_set configfs mounted
  down_change c3 "umount configfs" umount_configfs
  rc=$?
  if [ "$rc" = 99 ] || configfs_mounted; then
    log c3 "configfs is still mounted (or the window was not armed): check 3 not run"
    restore_all c3
    window_close c3
    result c3 NOT-RUN "configfs could not be unmounted with lemnosd stopped"
    return
  fi
  sleep 3
  logout c3 lemnos-ctl list
  logout c3 journal_since "$LEMNOSD_UNIT" "$e0"
  local failed=no
  device_running "$GDEV" || failed=yes
  t0=$(mono)
  configfs_mount
  log c3 "configfs mounted again: $(configfs_mounted && echo yes || echo NO)"
  configfs_mounted && undo_clear configfs
  up=$(wait_gadget_up c3 20)
  restore_all c3
  window_close c3
  if [ "$failed" = yes ] && [ -n "$up" ] && le "$up" 9; then
    result c3 PASS "gadget failed while configfs was missing; up $up s after the mount, no restart"
  else
    result c3 FAIL "gadget failed while missing: $failed; up after the mount: ${up:-not within 20 s} (want <= 8 s + a look)"
  fi
}

v1g_1() {
  local dev drv e0 t0 up rc
  dev=$(basename "$(readlink -f "$SYSFS/class/udc/$UDC/device")")
  drv=$(readlink -f "$SYSFS/class/udc/$UDC/device/driver")
  if [ -z "$dev" ] || [ ! -d "$drv" ]; then
    result c1 NOT-RUN "no driver found for the controller $UDC"
    return
  fi
  window_open c1 "$V1G_LONG" || { result c1 NOT-RUN "the timed restore could not be armed"; return; }
  e0=$(epoch)
  undo_set udc-bind "$drv $dev"
  log c1 "controller device $dev, driver $drv (an unbind: reported)"
  down_change c1 "unbind $dev from $drv" sh -c 'printf "%s" "$1" >"$2/unbind"' sh "$dev" "$drv"
  rc=$?
  if [ "$rc" = 99 ] || [ -e "$drv/$dev" ]; then
    restore_all c1
    window_close c1
    result c1 NOT-RUN "the controller could not be unbound (exit $rc)"
    return
  fi
  sleep 3
  logout c1 lemnos-ctl list
  logout c1 journal_since "$LEMNOSD_UNIT" "$e0"
  local failed=no
  device_running "$GDEV" || failed=yes
  t0=$(mono)
  printf '%s' "$dev" >"$drv/bind"
  log c1 "bound $dev again: $([ -e "$drv/$dev" ] && echo yes || echo NO)"
  [ -e "$drv/$dev" ] && undo_clear udc-bind
  up=$(wait_gadget_up c1 20)
  restore_all c1
  window_close c1
  if [ "$failed" = yes ] && [ -n "$up" ] && le "$up" 8; then
    result c1 PASS "gadget failed while the controller was away; up $up s after the bind, no restart"
  else
    result c1 FAIL "gadget failed while away: $failed; up after the bind: ${up:-not within 20 s} (want <= 8 s)"
  fi
}

v1g_2() {
  local ctl r role="" was up
  ctl=$(readlink -f "$SYSFS/class/udc/$UDC/device")
  for r in "$SYSFS"/class/usb_role/*; do
    [ -e "$r/role" ] || continue
    case $(readlink -f "$r/device") in "$ctl"*) role=$r/role ;; esac
    case $ctl in "$(readlink -f "$r/device")"*) role=$r/role ;; esac
  done
  if [ -z "$role" ]; then
    result c2 NOT-RUN "the controller $UDC has no usb_role switch"
    return
  fi
  was=$(cat "$role")
  window_open c2 "$V1G_LONG" || { result c2 NOT-RUN "the timed restore could not be armed"; return; }
  undo_set role "$role $was"
  printf 'host' >"$role"
  log c2 "role $role: '$was' -> '$(cat "$role")'"
  sleep 3
  logout c2 lemnos-ctl list
  printf 'device' >"$role"
  log c2 "role -> '$(cat "$role")'"
  up=$(wait_gadget_up c2 20)
  [ "$(cat "$role")" = "$was" ] && undo_clear role
  restore_all c2
  window_close c2
  if [ -n "$up" ] && le "$up" 8; then result c2 PASS "up $up s after the switch to device"; else result c2 FAIL "up after the switch: ${up:-not within 20 s}"; fi
}

check_steps() {
  local s
  for s in $STEPS; do
    case $s in
    4) v1g_4 ;;
    5) v1g_5 ;;
    6) v1g_6 ;;
    3) v1g_3 ;;
    1) v1g_1 ;;
    2) v1g_2 ;;
    *) log run "unknown step '$s'" ;;
    esac
  done
}
