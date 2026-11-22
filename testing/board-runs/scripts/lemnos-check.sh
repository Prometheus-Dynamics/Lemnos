#!/usr/bin/env bash
# is_group ID: true only for a plain number above 1 that is not this shell's own process group: the
# only ids a group kill here may take (never -1, -0 or this shell's group, whatever a pid file held).
is_group() {
  case "$1" in ''|*[!0-9]*) return 1 ;; esac
  [ "$1" -gt 1 ] || return 1
  [ "$1" != "$(sed 's/^.*) //' /proc/$$/stat | cut -d' ' -f3)" ]
}
# The detached form of the board checks that stop lemnosd or the comm core (B20, E25, E36, H4a, V1g).
# Run as root on the board. Each record's "Detached form" section gives the procedure.
#
#   lemnos-check.sh start ID [NAME=VALUE ...]   from the runner's session: copies the scripts into a
#                                               new run directory, starts the check detached, returns
#   lemnos-check.sh collect ID [RUN]            from a new session after the link is back: prints the
#                                               log, the results and the board's state now
#   lemnos-check.sh release ID                  ends a hold (H4a H4A_MODE=hold) early
#   lemnos-check.sh run ID RUNDIR               inside the check's own unit (start does this)
#   lemnos-check.sh restore ID RUNDIR NAME S    inside a timed restore's unit (the check does this)
#
# Settings for start (NAME=VALUE): COMM_UNIT (default orion-comm.service), CHECK_LOCKS, STEPS, ALT_BOARD, ECHO_BIN, WINDOW_S,
# PLUG_WAIT_S, HOLD_S, H4A_MODE, ORION_CLI, ORION_ADDRESS, DOWN_BOUND_S, LINK_IF. Each check file
# under checks/ says which it reads.
set -u

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=detached-lib.sh
. "$here/detached-lib.sh"

die() { printf 'lemnos-check: %s\n' "$*" >&2; exit 1; }

known_id() { case $1 in B20 | E25 | E36 | H4a | V1g) return 0 ;; *) return 1 ;; esac; }

cmd=${1:-}
CHECK_ID=${2:-}
[ -n "$cmd" ] && [ -n "$CHECK_ID" ] || die "usage: lemnos-check.sh start|collect|release|run|restore ID ..."
known_id "$CHECK_ID" || die "unknown check '$CHECK_ID' (B20 E25 E36 H4a V1g)"

# The settings of a run are read back from its directory by the check and by every restore.
load_settings() { [ -f "$RUNDIR/settings" ] && . "$RUNDIR/settings"; }

case $cmd in
start)
  shift 2
  [ -z "$CHECK_ROOT" ] && [ "$(id -u)" != 0 ] && die "run as root"
  run=$(date -u +%Y%m%dT%H%M%SZ)
  RUNDIR=$CHECKS_DIR/$CHECK_ID/$run
  if command -v systemd-run >/dev/null 2>&1 && [ "$(unit_state "lemnos-check-$CHECK_ID.service")" = active ]; then
    die "lemnos-check-$CHECK_ID is already running"
  fi
  mkdir -p "$RUNDIR/bin/checks" || die "cannot make $RUNDIR"
  # A copy per run: the running files are never the ones a runner uploads again.
  cp "$here/lemnos-check.sh" "$here/detached-lib.sh" "$RUNDIR/bin/" || die "copy failed"
  cp "$here/checks/$CHECK_ID.sh" "$RUNDIR/bin/checks/" || die "copy failed"
  : >"$RUNDIR/settings"
  for kv in "$@"; do
    name=${kv%%=*}
    value=${kv#*=}
    case " $SETTING_NAMES " in *" $name "*) ;; *) die "unknown setting '$name'" ;; esac
    printf '%s=%q\n' "$name" "$value" >>"$RUNDIR/settings"
  done
  ln -sfn "$run" "$CHECKS_DIR/$CHECK_ID/latest"
  launch_detached "lemnos-check-$CHECK_ID" bash "$RUNDIR/bin/lemnos-check.sh" run "$CHECK_ID" "$RUNDIR"
  n=0
  while [ ! -s "$RUNDIR/check.pid" ] && [ "$n" -lt 100 ]; do
    sleep 0.1
    n=$((n + 1))
  done
  [ -s "$RUNDIR/check.pid" ] || die "the detached check did not start (see journalctl -u lemnos-check-$CHECK_ID)"
  printf 'started %s: pid %s, log %s/log\n' "$CHECK_ID" "$(cat "$RUNDIR/check.pid")" "$RUNDIR"
  printf 'read it back later with: bash %s/bin/lemnos-check.sh collect %s %s\n' "$RUNDIR" "$CHECK_ID" "$run"
  ;;

collect)
  run=${3:-latest}
  RUNDIR=$CHECKS_DIR/$CHECK_ID/$run
  [ -d "$RUNDIR" ] || die "no run $RUNDIR"
  RUNDIR=$(cd "$RUNDIR" && pwd -P)
  load_settings
  echo "== RUN $(verdict)"
  echo "== run $RUNDIR"
  echo "== settings"
  cat "$RUNDIR/settings"
  echo "== log"
  cat "$RUNDIR/log" 2>/dev/null
  for f in "$RUNDIR"/restore.*.log; do
    [ -e "$f" ] || continue
    echo "== $(basename "$f")"
    cat "$f"
  done
  echo "== results"
  cat "$RUNDIR/result" 2>/dev/null
  echo "== state now"
  pid=$(cat "$RUNDIR/check.pid" 2>/dev/null)
  if [ -f "$RUNDIR/done" ]; then
    echo "check: finished: $(cat "$RUNDIR/done")"
  elif [ -n "$pid" ] && [ -d "/proc/$pid" ] && grep -q 'lemnos-check' "/proc/$pid/cmdline" 2>/dev/null; then
    echo "check: STILL RUNNING (pid $pid)"
  else
    echo "check: ENDED WITHOUT FINISHING (killed, or the board reset)"
  fi
  echo "end: $(cat "$RUNDIR/end" 2>/dev/null || echo 'no end check recorded')"
  for f in "$RUNDIR"/restore.*.end; do
    [ -e "$f" ] && echo "$(basename "$f" .end): $(cat "$f")"
  done
  ls "$RUNDIR/open" 2>/dev/null | sed 's/^/window still open: /'
  boot=$(awk '$1 == "boot_id" { print $2 }' "$RUNDIR/baseline" 2>/dev/null)
  now=$(cat "$BOOT_ID_FILE" 2>/dev/null)
  if [ -n "$boot" ] && [ "$boot" != "$now" ]; then
    echo "boot: THE BOARD RESET since the baseline (boot id $boot -> $now)"
  else
    echo "boot: same boot as the baseline"
  fi
  echo "lemnosd: $(unit_state "$LEMNOSD_UNIT"); watchdog row: $(watchdog_row)"
  [ -n "$COMM_UNIT" ] && echo "comm core $COMM_UNIT: $(unit_state "$COMM_UNIT")"
  echo "drop-ins of ours: $(ls "$DROPIN_DIR"/90-lemnos-check-* 2>/dev/null || echo none)"
  echo "== RUN $(verdict)"
  ;;

release)
  RUNDIR=$CHECKS_DIR/$CHECK_ID/latest
  [ -d "$RUNDIR" ] || die "no run"
  : >"$RUNDIR/release"
  echo "release asked: $RUNDIR/release"
  ;;

run)
  RUNDIR=${3:?run directory}
  LOG=$RUNDIR/log
  load_settings
  printf '%s\n' "$$" >"$RUNDIR/check.pid"
  # shellcheck source=/dev/null
  . "$RUNDIR/bin/checks/$CHECK_ID.sh"
  log hypothesis "$CHECK_HYPOTHESIS"
  log run "check $CHECK_ID pid $$, run $RUNDIR, settings: $(tr '\n' ' ' <"$RUNDIR/settings")"
  finish() { printf '%s\n' "$1" >"$RUNDIR/done"; log run "finished: $1"; exit 0; }
  refuse() { log run "REFUSED, nothing stopped: $*"; result run NOT-RUN "$*"; finish "refused: $*"; }
  # A check that ends with a window still open (an error in the script) restores at once; a check
  # that is killed leaves it to the timed restore.
  on_exit() {
    if [ -n "$(ls "$RUNDIR/open" 2>/dev/null)" ]; then
      log run "ended with windows open [$(ls "$RUNDIR/open" | tr '\n' ' ')]: restoring now"
      restore_all run
      for w in "$RUNDIR"/open/*; do window_close "$(basename "$w")"; done
      end_check run
    fi
  }
  trap on_exit EXIT
  for lock in $CHECK_LOCKS; do
    mkdir -p "$(dirname "$lock")"
    exec {fd}>"$lock" || refuse "cannot open $lock"
    flock -w 10 "$fd" || refuse "$lock is held by someone else (the runner must not hold it while the detached check runs)"
    log run "holding $lock"
  done
  baseline
  unit_active "$LEMNOSD_UNIT" || refuse "lemnosd is not active at the start"
  # A restore waits for every unit it brings back: one that was not active before would hold it.
  [ -z "$COMM_UNIT" ] || unit_active "$COMM_UNIT" || refuse "COMM_UNIT $COMM_UNIT is not active at the start (default orion-comm.service; COMM_UNIT= to name another, COMM_UNIT= empty for none)"
  watchdog_ok || refuse "the hw-watchdog device is not 'running ok' at the start"
  WD_PERIOD=$(down_budget) || refuse "DOWN_BOUND_S $DOWN_BOUND_S + $START_ALLOW_S s for a start is not below half the watchdog period ('$WD_PERIOD' s from $WATCHDOG_SYS/timeout)"
  log run "watchdog period $WD_PERIOD s: lemnosd down windows are bounded by $DOWN_BOUND_S s (+ $START_ALLOW_S s start) < $WD_PERIOD/2 s"
  check_precheck || refuse "precheck failed (lines above)"
  check_steps
  end_check run
  finish "steps done; as found: $(cat "$RUNDIR/end")"
  ;;

restore)
  RUNDIR=${3:?run directory}
  name=${4:?name}
  bound=${5:?bound}
  LOG=$RUNDIR/restore.$name.log
  load_settings
  printf '%s\n' "$$" >"$RUNDIR/restore.$name.alive"
  t0=$(mono)
  log "restore-$name" "armed by check $CHECK_ID: fires $bound s from now unless cancelled"
  while :; do
    if [ -e "$RUNDIR/restore.$name.cancel" ]; then
      log "restore-$name" "cancelled after $(since "$t0") s: the check restored by itself"
      printf 'cancelled\n' >"$RUNDIR/restore.$name.end"
      exit 0
    fi
    le "$bound" "$(since "$t0")" && break
    sleep 0.1
  done
  log "restore-$name" "FIRED at the bound ($bound s): the check did not restore in time"
  # The check first, so a hung check cannot stop anything again after the restore.
  pid=$(cat "$RUNDIR/check.pid" 2>/dev/null)
  if command -v systemd-run >/dev/null 2>&1 && [ "$(unit_state "lemnos-check-$CHECK_ID.service")" = active ]; then
    bounded 10 systemctl kill --signal=KILL "lemnos-check-$CHECK_ID.service"
    log "restore-$name" "killed the check's unit lemnos-check-$CHECK_ID: exit $?"
  elif [ -n "$pid" ] && grep -q 'lemnos-check' "/proc/$pid/cmdline" 2>/dev/null; then
    { is_group "$pid" && kill -KILL -- "-$pid" 2>/dev/null; } || kill -KILL "$pid"
    log "restore-$name" "killed the check, pid $pid"
  else
    log "restore-$name" "the check is not running"
  fi
  restore_all "restore-$name"
  for w in "$RUNDIR"/open/*; do
    [ -e "$w" ] || continue
    other=$(basename "$w")
    [ "$other" != "$name" ] && : >"$RUNDIR/restore.$other.cancel"
    rm -f "$w"
  done
  end_check "restore-$name"
  result "restore-$name" MEASURED "the timed restore fired at $bound s and restored; as found: $(cat "$RUNDIR/end")"
  printf 'fired and restored; %s\n' "$(cat "$RUNDIR/end")" >"$RUNDIR/restore.$name.end"
  [ -f "$RUNDIR/done" ] || printf 'killed by restore %s at its bound; %s\n' "$name" "$(cat "$RUNDIR/end")" >"$RUNDIR/done"
  ;;

*) die "unknown command '$cmd'" ;;
esac
