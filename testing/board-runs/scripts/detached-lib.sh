# Shared parts of the detached board checks (sourced by lemnos-check.sh; not run on its own).
#
# On an image where lemnosd owns the USB gadget and the comm core serves the login, stopping either
# one cuts the runner's own session. A check that stops them therefore runs detached on the board,
# as root, in its own transient unit, and arms a timed restore in a second unit BEFORE it stops
# anything. Everything is written to a log on the data partition; the runner reads it after the
# link is back (`lemnos-check.sh collect ID`).
#
# The watchdog. lemnosd's hw-watchdog driver is the only feeder of /dev/watchdog once slot-init has
# exec'd away. It feeds every half period (drivers/hw-watchdog/src/tests.rs,
# the_feed_is_every_half_the_timeout_in_force) and never writes the magic close. When lemnosd closes
# the device without the magic close, the kernel pings once and logs `watchdog did not stop!`
# (drivers/watchdog/watchdog_dev.c, watchdog_release), and the board resets one period after that
# ping unless a new opener feeds. The bcm2835 driver's period is below its hardware heartbeat, so the
# kernel's worker does not feed an open-then-closed device. Two budgets follow:
#   - from the close ping: one period (15 s on the reference board);
#   - with no close ping (a stop that hangs before it drops the driver): half a period from the
#     stop, the worst case of the last feed.
# Every lemnosd down window here is bounded by DOWN_BOUND_S, which must sit below half the period
# with room for a start (checked against the device's own timeout before anything stops). Nothing
# here touches config.txt, the default slot, `mem=`, `.reboot-ok` or the watchdog itself.
#
# Paths take CHECK_ROOT as a prefix so the PC rehearsal can run every line against a stand-in tree.
# On a board CHECK_ROOT is empty.

CHECK_ROOT=${CHECK_ROOT:-}
CHECKS_DIR=${CHECKS_DIR:-$CHECK_ROOT/data/board/checks}
CHECK_LOCKS=${CHECK_LOCKS:-"$CHECK_ROOT/run/lock/raze-gpu.lock $CHECK_ROOT/run/lock/board-gpu.lock"}
LEMNOSD_UNIT=${LEMNOSD_UNIT:-lemnosd.service}
BRIDGE_UNIT=${BRIDGE_UNIT:-lemnos-orion.service}
# The comm core's unit as the image installs it (Orion's packaging/systemd at the pinned rev).
COMM_UNIT=${COMM_UNIT-orion-comm.service}
LEMNOSD_BIN=${LEMNOSD_BIN:-/usr/bin/lemnosd}
LEMNOSD_SOCKET=${LEMNOSD_SOCKET:-$CHECK_ROOT/run/lemnos/lemnosd.sock}
SYSFS=${SYSFS:-$CHECK_ROOT/sys}
PROC_MOUNTS=${PROC_MOUNTS:-$CHECK_ROOT/proc/mounts}
BOOT_ID_FILE=${BOOT_ID_FILE:-$CHECK_ROOT/proc/sys/kernel/random/boot_id}
DROPIN_DIR=${DROPIN_DIR:-$CHECK_ROOT/run/systemd/system/lemnosd.service.d}
LINK_IF=${LINK_IF:-lemnos0}
WATCHDOG_SYS=${WATCHDOG_SYS:-$SYSFS/class/watchdog/watchdog0}
# The longest lemnosd may be down in one window, from the stop to the timed restore firing.
DOWN_BOUND_S=${DOWN_BOUND_S:-5}
# Room left for lemnosd's own start (to READY and the first feed) inside half a period.
START_ALLOW_S=${START_ALLOW_S:-2}

# The settings a runner may pass to `start`, and nothing else.
SETTING_NAMES="COMM_UNIT CHECK_LOCKS STEPS ALT_BOARD ECHO_BIN WINDOW_S PLUG_WAIT_S HOLD_S H4A_MODE ORION_CLI ORION_ADDRESS DOWN_BOUND_S LINK_IF"

export TZ=UTC

# --- time and the log ------------------------------------------------------------------------

# stamp: CLOCK_REALTIME, ISO 8601 with microseconds. bash 5's EPOCHREALTIME, else date (a date
# with no %N prints a literal N, which is cut back to whole seconds).
stamp() {
  local e=${EPOCHREALTIME:-}
  if [ -z "$e" ]; then
    e=$(date +%s.%N)
    case $e in *N) e=${e%%.*}.000000 ;; esac
  fi
  local s=${e%%.*} f=${e#*.}
  f=${f}000000
  printf '%(%Y-%m-%dT%H:%M:%S)T.%sZ' "$s" "${f:0:6}"
}

# epoch: CLOCK_REALTIME in whole seconds (for journalctl --since @N).
epoch() { printf '%(%s)T' -1; }

# mono: CLOCK_MONOTONIC-like seconds since boot, from /proc/uptime (10 ms). Durations use this.
mono() { local up _; read -r up _ </proc/uptime; printf '%s' "$up"; }

# since T0: seconds from a mono() reading to now, 2 decimals.
since() { awk -v a="$1" -v b="$(mono)" 'BEGIN { printf "%.2f", b - a }'; }

# sub A B: A - B, 2 decimals.
sub() { awk -v a="$1" -v b="$2" 'BEGIN { printf "%.2f", a - b }'; }

# le A B: true when A <= B (decimals).
le() { awk -v a="$1" -v b="$2" 'BEGIN { exit !(a <= b) }'; }

LOG=/dev/null
# log STEP TEXT...: one line, realtime stamp, mono seconds, step, text. Also on stderr (the unit's
# journal), never stdout: functions whose output is captured log too.
log() {
  local step=$1
  shift
  local line
  printf -v line '%s %10s %-10s %s' "$(stamp)" "$(mono)" "$step" "$*"
  printf '%s\n' "$line" >>"$LOG"
  printf '%s\n' "$line" >&2
}

# logout STEP CMD...: runs CMD (bounded to 10 s), each output line into the log under STEP.
logout() {
  local step=$1
  shift
  local out rc
  # A shell function bounds its own commands (timeout cannot run one).
  if declare -F "$1" >/dev/null; then out=$("$@" 2>&1); else out=$(bounded 10 "$@" 2>&1); fi
  rc=$?
  while IFS= read -r l; do log "$step" "| $l"; done <<<"$out"
  log "$step" "($* -> exit $rc)"
  return $rc
}

# result STEP VERDICT TEXT: VERDICT is PASS, FAIL, MEASURED (a number, no verdict asked), NOT-RUN
# (a skip: the run is INCOMPLETE) or SUPERSEDED (a step the current code has no state for).
result() {
  local step=$1 verdict=$2
  shift 2
  printf '%s %s %s %s\n' "$(stamp)" "$step" "$verdict" "$*" >>"$RUNDIR/result"
  log "$step" "RESULT $verdict $*"
}

# bounded SECS CMD...: runs CMD, killed (its own PID, which this shell started) after SECS.
bounded() {
  local s=$1
  shift
  if command -v timeout >/dev/null 2>&1; then
    timeout -s KILL "$s" "$@"
    return
  fi
  "$@" &
  local p=$!
  (
    sleep "$s"
    kill -KILL "$p" 2>/dev/null
  ) &
  local g=$!
  wait "$p"
  local rc=$?
  kill "$g" 2>/dev/null
  wait "$g" 2>/dev/null
  return $rc
}

# --- units -----------------------------------------------------------------------------------

unit_state() { bounded 5 systemctl is-active "$1" 2>/dev/null | head -n1; }
unit_active() { [ "$(unit_state "$1")" = active ]; }
main_pid() { bounded 5 systemctl show -p MainPID --value "$1" 2>/dev/null; }

# ensure_active WHO UNIT: starts UNIT until it is active. Never gives up: attempts start 1, 2, 4, 8,
# 8, ... s apart on the clock (the failure rule: backoff capped at 8 s). Each attempt is a
# non-blocking start, then a watch of the unit's state until the next attempt is due.
ensure_active() {
  local who=$1 unit=$2 delay=1 attempt=0 t0 ta state out rc
  t0=$(mono)
  while :; do
    if unit_active "$unit"; then
      [ "$attempt" -gt 0 ] && log "$who" "$unit active after $attempt start(s), $(since "$t0") s"
      return 0
    fi
    attempt=$((attempt + 1))
    ta=$(mono)
    out=$(bounded 10 systemctl start --no-block "$unit" 2>&1)
    rc=$?
    log "$who" "start $unit: attempt $attempt, exit $rc${out:+: $out}"
    while :; do
      state=$(unit_state "$unit")
      [ "$state" = active ] && break
      le "$delay" "$(since "$ta")" && break
      sleep 0.1
    done
    if [ "$state" = active ]; then
      log "$who" "$unit active after $attempt start(s), $(since "$t0") s"
      return 0
    fi
    log "$who" "$unit is '$state' $delay s after attempt $attempt: next attempt now"
    delay=$((delay * 2))
    [ "$delay" -gt 8 ] && delay=8
  done
}

# lemnosd_up: lemnosd active and serving (its socket answers `list`).
lemnosd_up() { unit_active "$LEMNOSD_UNIT" && bounded 5 lemnos-ctl list >/dev/null 2>&1; }

# watchdog_row: lemnos-ctl list's row for the hw-watchdog driver ("" when none).
watchdog_row() { bounded 5 lemnos-ctl list 2>/dev/null | awk '$2 == "hw-watchdog"' | head -n1; }
watchdog_ok() { watchdog_row | awk '$3 == "running" && $4 == "ok" { f = 1 } END { exit !f }'; }

# device_row ID
device_row() { bounded 5 lemnos-ctl list 2>/dev/null | awk -v id="$1" '$1 == id' | head -n1; }
device_running() { device_row "$1" | awk '$3 == "running" { f = 1 } END { exit !f }'; }

did_not_stop_count() { bounded 5 dmesg 2>/dev/null | grep -c 'did not stop' || true; }

journal_since() { # UNIT EPOCH
  bounded 10 journalctl -u "$1" --since "@$2" --no-pager -o short-iso-precise 2>&1
}

# --- the gadget ------------------------------------------------------------------------------

GADGETS=$SYSFS/kernel/config/usb_gadget
# gadget_dir: the lemnos-* gadget directory in configfs ("" when none).
gadget_dir() {
  local d
  for d in "$GADGETS"/lemnos-*; do
    [ -d "$d" ] && { printf '%s' "$d"; return 0; }
  done
  return 1
}
gadget_udc() { local d; d=$(gadget_dir) && cat "$d/UDC" 2>/dev/null; }
ffs_mounts() { awk '$3 == "functionfs"' "$PROC_MOUNTS" 2>/dev/null; }

# gadget_snapshot STEP: what configfs and the mounts show now.
gadget_snapshot() {
  local step=$1 d
  if d=$(gadget_dir); then
    log "$step" "gadget dir $d: UDC='$(cat "$d/UDC" 2>/dev/null)' configs=[$(ls "$d/configs" 2>/dev/null | tr '\n' ' ')] strings=[$(ls "$d/strings" 2>/dev/null | tr '\n' ' ')] functions=[$(ls "$d/functions" 2>/dev/null | tr '\n' ' ')]"
  else
    log "$step" "gadget dir: none under $GADGETS"
  fi
  log "$step" "functionfs mounts: [$(ffs_mounts | awk '{ print $1 " on " $2 }' | tr '\n' ' ')]"
}

# gadget_device: the board file's usb-gadget device id, from lemnos-ctl list.
gadget_device() { bounded 5 lemnos-ctl list 2>/dev/null | awk '$2 == "usb-gadget" { print $1; exit }'; }

# --- the undo journal ------------------------------------------------------------------------
# A step writes what it is about to change into $RUNDIR/undo/<kind> BEFORE it changes it, and
# removes the entry once it has put it back itself. restore_all undoes whatever is left.

undo_set() { mkdir -p "$RUNDIR/undo"; printf '%s\n' "$2" >"$RUNDIR/undo/$1"; log undo "journal $1: $2"; }
undo_clear() { rm -f "$RUNDIR/undo/$1"; }

# dropin_install NAME TEXT: a runtime drop-in for lemnosd (under /run: gone at the next boot).
dropin_install() {
  local path=$DROPIN_DIR/90-lemnos-check-$1.conf
  undo_set dropin "$path"
  mkdir -p "$DROPIN_DIR"
  printf '%s\n' "$2" >"$path.tmp" && mv "$path.tmp" "$path"
  log dropin "installed $path:"
  while IFS= read -r l; do log dropin "| $l"; done <"$path"
  bounded 30 systemctl daemon-reload
  log dropin "daemon-reload exit $?"
}

dropin_remove() {
  local path
  [ -f "$RUNDIR/undo/dropin" ] || return 0
  path=$(cat "$RUNDIR/undo/dropin")
  rm -f "$path"
  bounded 30 systemctl daemon-reload
  log dropin "removed $path, daemon-reload exit $?"
  [ -e "$path" ] && { log dropin "STILL THERE: $path"; return 1; }
  undo_clear dropin
}

configfs_mounted() { awk -v m="$SYSFS/kernel/config" '$2 == m && $3 == "configfs" { f = 1 } END { exit !f }' "$PROC_MOUNTS"; }
configfs_mount() {
  configfs_mounted && return 0
  if bounded 5 systemctl cat sys-kernel-config.mount >/dev/null 2>&1; then
    bounded 10 systemctl start sys-kernel-config.mount
  else
    bounded 10 mount -t configfs configfs "$SYSFS/kernel/config"
  fi
  configfs_mounted
}

# restore_all WHO: undoes what the journal holds, then brings the units back. Idempotent: the
# check runs it on its own path, and the timed restore runs it when it fires. Never gives up.
restore_all() {
  local who=$1 restart=0 e
  log "$who" "restore: journal [$(ls "$RUNDIR/undo" 2>/dev/null | tr '\n' ' ')]"
  if [ -f "$RUNDIR/undo/dropin" ]; then
    dropin_remove && restart=1
  fi
  if [ -f "$RUNDIR/undo/configfs" ]; then
    if configfs_mount; then undo_clear configfs; log "$who" "configfs mounted again"; else log "$who" "configfs mount FAILED"; fi
  fi
  if [ -f "$RUNDIR/undo/udc-bind" ]; then
    local drv dev
    read -r drv dev <"$RUNDIR/undo/udc-bind"
    if [ ! -e "$drv/$dev" ]; then
      printf '%s' "$dev" >"$drv/bind" 2>/dev/null
      log "$who" "bound $dev to $drv: $([ -e "$drv/$dev" ] && echo ok || echo FAILED)"
    fi
    [ -e "$drv/$dev" ] && undo_clear udc-bind
  fi
  if [ -f "$RUNDIR/undo/role" ]; then
    local path value
    read -r path value <"$RUNDIR/undo/role"
    printf '%s' "$value" >"$path" 2>/dev/null
    log "$who" "role $path set back to '$value': now '$(cat "$path" 2>/dev/null)'"
    [ "$(cat "$path" 2>/dev/null)" = "$value" ] && undo_clear role
  fi
  # A board file changed under a running lemnosd needs a restart to take the image's file again.
  if [ "$restart" = 1 ] && unit_active "$LEMNOSD_UNIT"; then
    local t0 out rc
    t0=$(mono)
    out=$(bounded 40 systemctl restart "$LEMNOSD_UNIT" 2>&1)
    rc=$?
    log "$who" "restart $LEMNOSD_UNIT onto the image's settings: exit $rc in $(since "$t0") s${out:+: $out}"
  fi
  ensure_active "$who" "$LEMNOSD_UNIT"
  if [ -f "$RUNDIR/undo/gadget-off" ]; then
    e=$(cat "$RUNDIR/undo/gadget-off")
    local n=0 d=1
    until bounded 10 lemnos-ctl power on "$e" >/dev/null 2>&1; do
      n=$((n + 1))
      log "$who" "power on $e failed ($n); again in $d s"
      sleep "$d"
      d=$((d * 2))
      [ "$d" -gt 8 ] && d=8
    done
    log "$who" "power on $e: ok"
    undo_clear gadget-off
  fi
  [ -n "$COMM_UNIT" ] && ensure_active "$who" "$COMM_UNIT"
  if [ "${CHECK_ID:-}" = B20 ]; then ensure_active "$who" "$BRIDGE_UNIT"; fi
  log "$who" "restore: done"
}

# --- windows: a timed restore around anything that stops lemnosd or the comm core -------------

# launch_detached UNIT CMD...: starts CMD so it outlives the session and the caller. A transient
# systemd unit where the image has systemd-run (its own cgroup: killing the check's unit does not
# touch it), else setsid + nohup.
launch_detached() {
  local unit=$1
  shift
  if command -v systemd-run >/dev/null 2>&1; then
    systemd-run --unit="$unit" --collect --quiet -p KillMode=control-group "$@"
  else
    setsid nohup "$@" >/dev/null 2>&1 </dev/null &
  fi
}

# window_open NAME BOUND_S: arms the timed restore NAME, which fires BOUND_S s from now unless
# window_close NAME runs first. Returns non-zero, having stopped nothing, if the restore is not
# seen running within 5 s.
window_open() {
  local name=$1 bound=$2 n=0
  rm -f "$RUNDIR/restore.$name".*
  launch_detached "lemnos-restore-$CHECK_ID-$name" bash "$RUNDIR/bin/lemnos-check.sh" restore "$CHECK_ID" "$RUNDIR" "$name" "$bound"
  while [ ! -s "$RUNDIR/restore.$name.alive" ] && [ "$n" -lt 50 ]; do
    sleep 0.1
    n=$((n + 1))
  done
  if [ ! -s "$RUNDIR/restore.$name.alive" ]; then
    log "$name" "ARMING FAILED: the timed restore did not start; nothing is stopped"
    return 1
  fi
  mkdir -p "$RUNDIR/open"
  : >"$RUNDIR/open/$name"
  log "$name" "armed: timed restore pid $(cat "$RUNDIR/restore.$name.alive"), fires in $bound s"
}

# window_close NAME: cancels restore NAME (after the check has restored by itself).
window_close() {
  local name=$1 n=0
  : >"$RUNDIR/restore.$name.cancel"
  while [ ! -e "$RUNDIR/restore.$name.end" ] && [ "$n" -lt 30 ]; do
    sleep 0.1
    n=$((n + 1))
  done
  rm -f "$RUNDIR/open/$name"
  log "$name" "closed: restore $(cat "$RUNDIR/restore.$name.end" 2>/dev/null || echo 'did not answer the cancel')"
}

# down_budget: checks DOWN_BOUND_S against the watchdog's period. Prints the period.
down_budget() {
  local t
  t=$(cat "$WATCHDOG_SYS/timeout" 2>/dev/null)
  case $t in '' | *[!0-9]*) return 1 ;; esac
  printf '%s' "$t"
  awk -v b="$DOWN_BOUND_S" -v s="$START_ALLOW_S" -v t="$t" 'BEGIN { exit !(b + s <= t / 2) }'
}

# watchdog_account STEP T_STOP T_ACTIVE DNS_BEFORE: what the watchdog saw of one down window.
watchdog_account() {
  local step=$1 t0=$2 t1=$3 before=$4 after down
  after=$(did_not_stop_count)
  down=$(sub "$t1" "$t0")
  log "$step" "watchdog: lemnosd down $down s from the stop to active; period ${WD_PERIOD:-?} s; close ping $([ "$after" -gt "$before" ] && echo "seen (dmesg 'did not stop' $before -> $after): budget is one period from the close" || echo "NOT seen (dmesg 'did not stop' $before -> $after): budget is half a period from the stop")"
  result "$step-down" MEASURED "lemnosd down ${down} s (bound $DOWN_BOUND_S s, period ${WD_PERIOD:-?} s)"
}

# stop_lemnosd STEP: stops lemnosd, prints the mono time the stop was issued.
stop_lemnosd() {
  local step=$1 t0 out rc
  t0=$(mono)
  log "$step" "stopping $LEMNOSD_UNIT"
  out=$(bounded 40 systemctl stop "$LEMNOSD_UNIT" 2>&1)
  rc=$?
  log "$step" "stop exit $rc in $(since "$t0") s${out:+: $out}; state '$(unit_state "$LEMNOSD_UNIT")'"
  STOP_T=$t0
}

# start_lemnosd STEP: the check's own restore of lemnosd (with the backoff). Sets ACTIVE_T.
start_lemnosd() {
  ensure_active "$1" "$LEMNOSD_UNIT"
  ACTIVE_T=$(mono)
}

# --- baseline and the end ----------------------------------------------------------------------

baseline() {
  local b=$RUNDIR/baseline
  {
    echo "boot_id $(cat "$BOOT_ID_FILE" 2>/dev/null)"
    echo "lemnosd $(unit_state "$LEMNOSD_UNIT") pid $(main_pid "$LEMNOSD_UNIT")"
    [ -n "$COMM_UNIT" ] && echo "comm $COMM_UNIT $(unit_state "$COMM_UNIT")"
    echo "bridge $BRIDGE_UNIT $(unit_state "$BRIDGE_UNIT")"
    echo "watchdog_row $(watchdog_row)"
    echo "watchdog_sys timeout=$(cat "$WATCHDOG_SYS/timeout" 2>/dev/null) state=$(cat "$WATCHDOG_SYS/state" 2>/dev/null) identity=$(cat "$WATCHDOG_SYS/identity" 2>/dev/null)"
    echo "did_not_stop $(did_not_stop_count)"
    echo "gadget_dir $(gadget_dir) udc $(gadget_udc)"
    echo "dropins $(ls "$DROPIN_DIR" 2>/dev/null | tr '\n' ' ')"
    echo "link $(link_addrs | tr '\n' ' ')"
    echo "--- lemnos-ctl list"
    bounded 5 lemnos-ctl list 2>&1
  } >"$b"
  while IFS= read -r l; do log baseline "$l"; done <"$b"
}

# link_addrs: the global addresses on the link interface, one per line.
link_addrs() {
  command -v ip >/dev/null 2>&1 || return 0
  bounded 5 ip -o addr show dev "$LINK_IF" scope global 2>/dev/null | awk '{ print $4 }'
}

# end_check WHO: is the board as found? lemnosd active, the watchdog driver running ok, the comm
# core and the bridge as before, the gadget bound, no drop-in of ours, the link's addresses back.
end_check() {
  local who=$1 n=0 ok=1 a
  while [ "$n" -lt 300 ]; do
    unit_active "$LEMNOSD_UNIT" && watchdog_ok && break
    sleep 0.1
    n=$((n + 1))
  done
  log "$who" "end: lemnosd '$(unit_state "$LEMNOSD_UNIT")' pid $(main_pid "$LEMNOSD_UNIT"); watchdog row: $(watchdog_row)"
  log "$who" "end: watchdog sysfs state=$(cat "$WATCHDOG_SYS/state" 2>/dev/null) timeout=$(cat "$WATCHDOG_SYS/timeout" 2>/dev/null)"
  unit_active "$LEMNOSD_UNIT" || ok=0
  watchdog_ok || ok=0
  if [ -n "$COMM_UNIT" ]; then
    log "$who" "end: $COMM_UNIT '$(unit_state "$COMM_UNIT")'"
    unit_active "$COMM_UNIT" || ok=0
  fi
  local bridge_before
  bridge_before=$(awk '$1 == "bridge" { print $3 }' "$RUNDIR/baseline")
  log "$who" "end: $BRIDGE_UNIT '$(unit_state "$BRIDGE_UNIT")' (before: '$bridge_before')"
  [ "$bridge_before" = active ] && ! unit_active "$BRIDGE_UNIT" && ok=0
  local udc_before
  udc_before=$(awk '$1 == "gadget_dir" { print $4 }' "$RUNDIR/baseline")
  n=0
  while [ -n "$udc_before" ] && [ "$(gadget_udc)" != "$udc_before" ] && [ "$n" -lt 100 ]; do
    sleep 0.1
    n=$((n + 1))
  done
  log "$who" "end: gadget UDC '$(gadget_udc)' (before: '$udc_before')"
  [ "$(gadget_udc)" = "$udc_before" ] || ok=0
  if ls "$DROPIN_DIR"/90-lemnos-check-* >/dev/null 2>&1; then
    log "$who" "end: a drop-in of ours is still there: $(ls "$DROPIN_DIR"/90-lemnos-check-*)"
    ok=0
  fi
  [ -n "$(ls "$RUNDIR/undo" 2>/dev/null)" ] && { log "$who" "end: undo journal not empty: $(ls "$RUNDIR/undo")"; ok=0; }
  # The link's addresses belong to systemd-networkd: a runtime .network file written at boot,
  # applied again by networkd when the interface comes back. Waited for 30 s. An address still
  # missing is put back as it was, so the board stays reachable, and the RUN FAILS: a recovery
  # never lets a run pass.
  local want
  want=$(awk '$1 == "link" { $1 = ""; print }' "$RUNDIR/baseline")
  for a in $want; do
    n=0
    while ! link_addrs | grep -qx "$a" && [ "$n" -lt 300 ]; do
      sleep 0.1
      n=$((n + 1))
    done
    if link_addrs | grep -qx "$a"; then
      log "$who" "end: $LINK_IF has $a"
    else
      log "$who" "end: $LINK_IF LACKS $a 30 s after the gadget came back: networkd did not apply it again; adding it back by hand so the board stays reachable"
      bounded 5 ip addr add "$a" dev "$LINK_IF"
      log "$who" "end: ip addr add exit $?; now [$(link_addrs | tr '\n' ' ')]"
      result link FAIL "$LINK_IF lacked $a after 30 s; re-added by hand (the board is reachable only because of that)"
      ok=0
    fi
  done
  local boot
  boot=$(awk '$1 == "boot_id" { print $2 }' "$RUNDIR/baseline")
  [ "$(cat "$BOOT_ID_FILE" 2>/dev/null)" = "$boot" ] || { log "$who" "end: BOOT ID CHANGED: the board reset during the run"; ok=0; }
  if [ "$ok" = 1 ]; then
    log "$who" "BOARD AS FOUND: yes"
    printf 'as-found yes\n' >"$RUNDIR/end"
  else
    log "$who" "BOARD AS FOUND: NO (see the lines above)"
    printf 'as-found no\n' >"$RUNDIR/end"
  fi
}

# verdict: the run's one word. FAILED for any FAIL, a board not as found, a reset, or a check that
# did not finish; INCOMPLETE for a NOT-RUN (a skip is never a pass); PASSED otherwise.
verdict() {
  local why=""
  grep -q ' FAIL ' "$RUNDIR/result" 2>/dev/null && why="$why; a step FAILED: $(awk '$3 == "FAIL" { printf "%s ", $2 }' "$RUNDIR/result")"
  grep -q 'as-found no' "$RUNDIR/end" 2>/dev/null && why="$why; the board was not as found"
  [ -f "$RUNDIR/end" ] || why="$why; no end check recorded"
  local boot
  boot=$(awk '$1 == "boot_id" { print $2 }' "$RUNDIR/baseline" 2>/dev/null)
  [ -n "$boot" ] && [ "$boot" != "$(cat "$BOOT_ID_FILE" 2>/dev/null)" ] && why="$why; the board reset"
  case $(cat "$RUNDIR/done" 2>/dev/null) in
  "steps done"*) ;;
  refused*) why="$why; refused" ;;
  "") why="$why; the check did not finish" ;;
  *) why="$why; the check was killed by its restore" ;;
  esac
  if [ -n "$why" ]; then
    printf 'FAILED%s\n' "$why"
  elif grep -q ' NOT-RUN ' "$RUNDIR/result" 2>/dev/null; then
    printf 'INCOMPLETE; not run: %s\n' "$(awk '$3 == "NOT-RUN" { printf "%s ", $2 }' "$RUNDIR/result")"
  else
    printf 'PASSED\n'
  fi
}
