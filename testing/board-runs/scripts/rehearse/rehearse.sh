#!/usr/bin/env bash
# Rehearses one detached check on the PC against the stand-in board (world.sh). No root, no real
# unit, no device, no network.
#
#   rehearse.sh ID CASE WORKDIR LOGFILE
#
# CASE:
#   normal              the check runs to its end
#   killed              the check's process group is killed (SIGKILL, by the PID this rehearsal's
#                       stand-in started) right after it stops or restarts lemnosd
#   hang                the check is frozen (SIGSTOP) at the same point: the restore must fire
#   restore-fails       killed at that point, and the next 2 starts of lemnosd fail: the restore
#                       retries 1, 2 s apart and the board stays up
#   restore-fails-long  as restore-fails with 5 failed starts: the backoff (1, 2, 4, 8 s) runs past
#                       the watchdog period and the stand-in resets the board, as the board would
# The point is exact: the stand-in systemctl freezes the check right after its Nth stop of lemnosd
# (REHEARSE_STOP_N, default 1; a restart counts). REHEARSE_AT=REGEX uses the first log line
# matching REGEX instead.
#   link-back           the normal run, judged on the link: networkd puts the address back
#   link-missing        after the first stop networkd no longer puts the address back: the end
#                       check re-adds it and the run must be FAILED
#   comm-fails          (B20 step 8) killed after the comm core restart, its next 6 starts fail:
#                       the restore's backoff reaches the 8 s cap and keeps going
set -u
id=$1 case=$2 work=$3 out=$4
here=$(cd "$(dirname "$0")" && pwd)
scripts=$(cd "$here/.." && pwd)
rm -rf "$work"
mkdir -p "$work"
export CHECK_ROOT=$work
export PATH=$here/bin:$PATH
. "$here/world.sh"

# The stand-in board: the image's files and state.
mkdir -p "$W"/{units,journal,fail,loaded-dropins/lemnosd} "$CR"/{dev,run/lock,run/lemnos,data/board/checks,etc/lemnos,proc/sys/kernel/random} \
  "$CR/sys/class/udc/$UDC_NAME" "$CR/sys/kernel/config/usb_gadget" "$CR/sys/class/watchdog/watchdog0" \
  "$CR/run/systemd/system/lemnosd.service.d" "$CR/sys/bus/platform/drivers/dwc2" "$CR/sys/bus/usb/devices"
: >"$W/calls.log"; : >"$W/dmesg.log"; : >"$W/events.log"; : >"$W/pgids"
cat /proc/sys/kernel/random/uuid >"$CR/proc/sys/kernel/random/boot_id"
echo 15 >"$CR/sys/class/watchdog/watchdog0/timeout"
echo active >"$CR/sys/class/watchdog/watchdog0/state"
printf 'configfs %s/sys/kernel/config configfs rw 0 0\n' "$CR" >"$CR/proc/mounts"
printf '[[devices]]\nid = "watchdog"\ndriver = "hw-watchdog"\n[[devices]]\nid = "gadget"\ndriver = "usb-gadget"\n' >"$CR/etc/lemnos/board.toml"
# The controller: a platform device bound to its driver (bind/unbind are files the stand-in reads).
mkdir -p "$CR/sys/devices/platform/$UDC_NAME"
ln -s "$CR/sys/devices/platform/$UDC_NAME" "$CR/sys/class/udc/$UDC_NAME/device"
ln -s "$CR/sys/bus/platform/drivers/dwc2" "$CR/sys/devices/platform/$UDC_NAME/driver"
ln -s "$CR/sys/devices/platform/$UDC_NAME" "$CR/sys/bus/platform/drivers/dwc2/$UDC_NAME"
feed

hw_pid=""
cleanup() {
  [ -n "$hw_pid" ] && kill "$hw_pid" 2>/dev/null
  for g in $(cat "$W/pgids" 2>/dev/null); do is_group "$g" && kill -KILL -- "-$g" 2>/dev/null; done
  [ -n "${drv_pid:-}" ] && kill "$drv_pid" 2>/dev/null
}
trap cleanup EXIT
"$here/bin/standin-hardware" &
hw_pid=$!

# A unbind/bind driver for the stand-in controller: writes to dwc2/unbind and bind move the udc.
(
  d=$CR/sys/bus/platform/drivers/dwc2
  while :; do
    if [ -s "$d/unbind" ]; then rm -f "$d/$(cat "$d/unbind")" "$CR/sys/class/udc/$UDC_NAME/device"; rmdir "$CR/sys/class/udc/$UDC_NAME" 2>/dev/null || rm -rf "$CR/sys/class/udc/$UDC_NAME"; : >"$d/unbind"; fi
    if [ -s "$d/bind" ]; then dev=$(cat "$d/bind"); ln -s "$CR/sys/devices/platform/$dev" "$d/$dev"; mkdir -p "$CR/sys/class/udc/$UDC_NAME"; ln -s "$CR/sys/devices/platform/$dev" "$CR/sys/class/udc/$UDC_NAME/device"; : >"$d/bind"; fi
    sleep 0.05
  done
) &
drv_pid=$!

for u in lemnosd orion-comm lemnos-orion; do systemctl start "$u" >/dev/null || { echo "stand-in $u did not start"; exit 1; }; done
for _ in $(seq 50); do [ -s "$GADGET_DIR/UDC" ] && break; sleep 0.1; done
# As a runner would: start only once the bridge has published (B20's precheck refuses otherwise).
for _ in $(seq 100); do [ -n "$(orion resource list lemnos. 2>/dev/null)" ] && break; sleep 0.1; done
# The point the faults are injected at: right after the Nth stop of lemnosd (REHEARSE_STOP_N).
case $case in killed | hang | restore-fails | restore-fails-long)
  [ -z "${REHEARSE_AT:-}" ] && echo "${REHEARSE_STOP_N:-1}" >"$W/freeze-after-stop" ;;
esac

# COMM_UNIT is left to its default (orion-comm.service), which the stand-in installs.
settings=()
case $id in
B20) settings+=(ORION_CLI=orion)
  if [ "$case" = comm-fails ]; then settings+=(STEPS=8); elif [ "$case" != normal ]; then settings+=(STEPS=9); fi ;;
E25) [ "$case" != normal ] && [ "$case" != link-back ] && settings+=(STEPS=stop) ;;
E36)
  printf '[[devices]]\nid = "watchdog"\ndriver = "hw-watchdog"\n[[devices]]\nid = "gadget"\ndriver = "usb-gadget"\nconfig = { serial = "E36-0001" }\n' >"$CR/data/board/checks/e36-board.toml"
  settings+=(ALT_BOARD="$CR/data/board/checks/e36-board.toml" ECHO_BIN="$here/bin/lemnos-usb-echo" WINDOW_S=30) ;;
H4a)
  { cat "$CR/etc/lemnos/board.toml"; printf '[[each]]\nid = "camera"\ndriver = "uvc"\nmatch = "usb:class:0e"\n'; } >"$CR/data/board/checks/h4a-board.toml"
  settings+=(ALT_BOARD="$CR/data/board/checks/h4a-board.toml" PLUG_WAIT_S=30)
  # The person's plug: a camera's interface appears 4 s after the check says PLUG NOW.
  (
    for _ in $(seq 600); do grep -q 'PLUG NOW' "$CR/data/board/checks/H4a/latest/log" 2>/dev/null && break; sleep 0.1; done
    sleep 4
    mkdir -p "$CR/sys/bus/usb/devices/1-1.3:1.0"; echo 0e >"$CR/sys/bus/usb/devices/1-1.3:1.0/bInterfaceClass"
  ) &
  ;;
V1g) [ "$case" != normal ] && settings+=(STEPS="6 3") ;;
esac

{
  echo "== rehearsal $id $case ($(date -u +%FT%TZ)); stand-in board at $CR"
  echo "== start"
  bash "$scripts/lemnos-check.sh" start "$id" "${settings[@]}" 2>&1
} >"$out"
rundir=$(readlink -f "$CR/data/board/checks/$id/latest")
pid=$(cat "$rundir/check.pid")
# The check's group, for the stand-in's freeze (bin/systemctl, group_is_check): the only group it may stop.
echo "$pid" >"$W/check.group"

case $case in
link-missing)
  for _ in $(seq 1200); do grep -Eq 'stop exit|restart: exit|stop queued|SIGKILL to' "$rundir/log" 2>/dev/null && break; sleep 0.05; done
  : >"$W/networkd.broken"
  echo "== rehearsal: networkd will not apply the address again (after: $(grep -E 'stop exit|restart: exit|stop queued|SIGKILL to' "$rundir/log" | head -n1))" >>"$out"
  ;;
killed | hang | restore-fails | restore-fails-long | comm-fails)
  pat='stop exit|restart: exit|restart onto the run.s board file: exit|stop queued|SIGKILL to'
  [ "$case" = comm-fails ] && pat='restart exit'
  # REHEARSE_AT picks another point (E36's 4.5: 'stop queued').
  [ -n "${REHEARSE_AT:-}" ] && pat=$REHEARSE_AT
  if [ -e "$W/freeze-after-stop" ] || [ -e "$W/frozen" ]; then
    for _ in $(seq 1200); do [ -s "$W/frozen" ] && break; sleep 0.05; done
    line="the stand-in froze the check inside its systemctl stop of lemnosd (stop number ${REHEARSE_STOP_N:-1})"
  else
    for _ in $(seq 1200); do grep -Eq "$pat" "$rundir/log" 2>/dev/null && break; sleep 0.05; done
    line=$(grep -E "$pat" "$rundir/log" | head -n1)
  fi
  case $case in
  restore-fails) echo 2 >"$W/fail/lemnosd" ;;
  restore-fails-long) echo 5 >"$W/fail/lemnosd" ;;
  comm-fails) echo 6 >"$W/fail/orion-comm"; systemctl stop orion-comm >/dev/null ;;
  esac
  if [ "$case" = hang ]; then
    { is_group "$pid" || { echo "== rehearsal: check.pid holds no group id ($pid): STOP" >>"$out"; exit 1; }; } && kill -STOP -- "-$pid"
    echo "== rehearsal: froze the check's process group $pid (SIGSTOP) after: $line" >>"$out"
  else
    { is_group "$pid" || { echo "== rehearsal: check.pid holds no group id ($pid): STOP" >>"$out"; exit 1; }; } && kill -KILL -- "-$pid"
    echo "== rehearsal: killed the check's process group $pid (SIGKILL) after: $line" >>"$out"
  fi
  [ "$case" = restore-fails ] || [ "$case" = restore-fails-long ] && echo "== rehearsal: the next $(cat "$W/fail/lemnosd") starts of lemnosd fail" >>"$out"
  [ "$case" = comm-fails ] && echo "== rehearsal: comm core stopped; its next 6 starts fail" >>"$out"
  ;;
esac

# Wait for the end: the check's done mark, a restore's end, or a reset (whose restore never ends).
limit=${REHEARSE_LIMIT:-400}
t=0
while [ "$t" -lt $((limit * 10)) ]; do
  if [ -f "$rundir/done" ] && [ -z "$(ls "$rundir/open" 2>/dev/null)" ]; then
    ls "$rundir"/restore.*.alive >/dev/null 2>&1 || break
    all=1
    for a in "$rundir"/restore.*.alive; do n=${a%.alive}; [ -e "$n.end" ] || all=0; done
    [ "$all" = 1 ] && break
  fi
  if grep -q BOOTED "$W/events.log"; then sleep 3; break; fi
  sleep 0.1
  t=$((t + 1))
done
[ "$t" -ge $((limit * 10)) ] && echo "== rehearsal: TIMED OUT waiting for the end" >>"$out"
sleep 1
{
  echo "== collect"
  bash "$scripts/lemnos-check.sh" collect "$id" 2>&1
  echo "== stand-in events (watchdog resets)"
  cat "$W/events.log"
  echo "== stand-in kernel log"
  cat "$W/dmesg.log"
  echo "== stand-in calls to systemctl start/stop/restart/kill, systemd-run, mount, umount, power"
  grep -E 'systemctl (start|stop|restart|kill|daemon-reload)|systemd-run|mount|lemnos-ctl power' "$W/calls.log"
  echo "== stand-in journal (lemnosd)"
  cat "$W/journal/lemnosd.log"
} >>"$out"
