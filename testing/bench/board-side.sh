#!/usr/bin/env bash
# The steps bench.sh runs against one lemnosd, in one of two modes:
#   sim    on the build host, against a lemnosd --sim that this script starts and stops (dry run):
#          the board file check, the start, the device list, the thread count and the memory. No
#          timing, no hardware, no systemctl.
#   board  on the board, against the lemnosd that is already running. bench.sh --real never stops,
#          restarts or masks it, and this script never starts a daemon or signals one. It reads that
#          daemon's socket and PID, and sends it only the usb-a-power switch's off and on (always
#          undone). The timings, the IMU stamps, the power readings and the USB port power cycle.
# Prints TSV rows (key, status, value, note) on stdout.
#
#   board-side.sh sim WORKDIR
#   board-side.sh board WORKDIR SOCKET PID
#
# WORKDIR holds lemnos-ctl, lemnos-bench-stamp and, for sim, lemnosd and board.toml.
# On the board, SOCKET is the running daemon's socket and PID is its main process.
# The rows lemnosd.check and lemnosd.start are not measured on the board: they need a daemon of
# their own (the file check and a start), so they are measured in sim, which is their only home.
set -u

mode=${1:?sim or board}
work=${2:?the work directory}
ctl=$work/lemnos-ctl
srv=$work/lemnosd
sim=()
where="board"
usb_off=0
own=
dpid=

if [ "$mode" = sim ]; then
  sim=(--sim)
  where="sim: build host, not the board"
  sock=$work/lemnosd.sock
elif [ "$mode" = board ]; then
  sock=${3:?the running daemon socket}
  dpid=${4:?the running daemon PID}
  case $dpid in
    '' | *[!0-9]*) echo "board-side: PID must be a number" >&2; exit 2 ;;
  esac
else
  echo "board-side: sim or board, not $mode" >&2
  exit 2
fi

row() { printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4"; }

# now_ms: milliseconds on the monotonic clock, from /proc/uptime (10 ms resolution). Not `date +%s%N`: a
# board whose date has no %N prints a literal N, and the arithmetic on it fails (`N: unbound variable`).
now_ms() { awk '{ printf "%d\n", $1 * 1000 }' /proc/uptime; }

# guard PID SECONDS: kills PID (a process this script started) if it is still running after SECONDS. The board
# has no `timeout` command, so the bound is this background kill, which names our own PID only.
guard() { ( sleep "$2"; kill "$1" 2>/dev/null ) >/dev/null 2>&1 & echo $!; }

# put KEY VALUE NOTE: a measured row when there is a value, otherwise not measured.
put() {
  if [ -n "$2" ]; then
    row "$1" measured "$2" "$3"
  else
    row "$1" not_measured "" "no value: $3"
  fi
}

cleanup() {
  if [ "$usb_off" = 1 ]; then
    "$ctl" --socket "$sock" on usb-a-power >/dev/null 2>&1 ||
      echo "usb-a-power may still be off: switch it on" >&2
  fi
  # Only the sim daemon is ours to stop. The board's daemon is never signalled here.
  if [ -n "$own" ]; then
    kill "$own" 2>/dev/null
    wait "$own" 2>/dev/null
  fi
}
trap cleanup EXIT

if [ "$mode" = sim ]; then
  if ! "$srv" --sim --check --board "$work/board.toml" >"$work/check.txt" 2>&1; then
    row lemnosd.check not_measured "" "board.toml does not parse: $(tr '\n' ' ' <"$work/check.txt")"
    exit 1
  fi
  row lemnosd.check measured ok "board.toml parses; lemnosd --check, $where"

  "$srv" "${sim[@]}" --board "$work/board.toml" --socket "$sock" >"$work/lemnosd.log" 2>&1 &
  own=$!
  up=0
  for _ in $(seq 1 100); do
    if [ -S "$sock" ]; then
      up=1
      break
    fi
    kill -0 "$own" 2>/dev/null || break
    sleep 0.1
  done
  if [ "$up" != 1 ]; then
    row lemnosd.start not_measured "" "lemnosd did not open its socket: $(tail -n 3 "$work/lemnosd.log" | tr '\n' ' ')"
    exit 1
  fi
  row lemnosd.start measured up "lemnosd on the bench board file, $where"
  dpid=$own
else
  row lemnosd.check not_measured "" "moved to sim: the file check needs no daemon on the board, and the board's daemon is not started again to run it"
  row lemnosd.start not_measured "" "moved to sim: the start time needs a daemon start, which the board run does not do (the daemon stays up)"
  if [ ! -d "/proc/$dpid" ]; then
    echo "board-side: no process $dpid: the daemon is gone" >&2
    exit 1
  fi
fi

"$ctl" --socket "$sock" list >"$work/list.txt" 2>&1
put lemnosd.devices "$(paste -sd ';' "$work/list.txt")" "lemnos-ctl list, $where"

ticks() { awk '{ print $14 + $15 }' "/proc/$dpid/stat" 2>/dev/null; }

# window SECONDS: lemnosd's CPU share over that many seconds, in percent, from its utime and stime.
window() {
  local t0 t1 s0 s1 ms
  t0=$(ticks)
  s0=$(now_ms)
  sleep "$1"
  t1=$(ticks)
  s1=$(now_ms)
  ms=$((s1 - s0))
  [ "$ms" -gt 0 ] || ms=1
  # USER_HZ is 100 on Linux (the unit of /proc/PID/stat); the board has no getconf.
  awk -v d="$((t1 - t0))" -v ms="$ms" -v hz=100 \
    'BEGIN { printf "%.2f", d / hz / (ms / 1000) * 100 }'
}

usb_count() { ls /sys/bus/usb/devices | grep -c -E '^[0-9]+-[0-9.]+$' || true; }

# settle lt|ge COUNT: waits for the sysfs USB device count to drop below, or return to at least,
# COUNT. Prints the wait in milliseconds. Gives up after 15 s.
settle() {
  local start now n
  start=$(now_ms)
  while :; do
    n=$(usb_count)
    if { [ "$1" = lt ] && [ "$n" -lt "$2" ]; } || { [ "$1" = ge ] && [ "$n" -ge "$2" ]; }; then
      break
    fi
    now=$(now_ms)
    if [ $((now - start)) -ge 15000 ]; then
      return 1
    fi
    sleep 0.05
  done
  now=$(now_ms)
  echo $((now - start))
}

usb_cycle() {
  local before drop back
  before=$(usb_count)
  usb_off=1
  if ! "$ctl" --socket "$sock" off usb-a-power >"$work/usb.txt" 2>&1; then
    usb_off=0
    row usb.cycle not_measured "" "lemnos-ctl off usb-a-power failed: $(tr '\n' ' ' <"$work/usb.txt")"
    return
  fi
  drop=$(settle lt "$before") || drop=
  if "$ctl" --socket "$sock" on usb-a-power >"$work/usb.txt" 2>&1; then
    usb_off=0
  fi
  back=$(settle ge "$before") || back=
  if [ -n "$drop" ] && [ -n "$back" ] && [ "$usb_off" = 0 ]; then
    row usb.cycle measured "devices $before, down after $drop ms, back after $back ms" \
      "$where, usb-a-power (the camera's port); devices counted in sysfs"
  else
    row usb.cycle not_measured "" \
      "cycle incomplete: devices $before, down ${drop:-no}, back ${back:-no}, switch off ${usb_off}"
  fi
}

if [ "$mode" = board ]; then
  put cpu.idle_pct "$(window 30)" "board, 30 s, no subscriber"

  "$ctl" --socket "$sock" watch imu acceleration.x --period-ms 20 --count 1500 \
    >/dev/null 2>&1 &
  sub=$!
  g=$(guard "$sub" 90)
  put cpu.50hz_pct "$(window 30)" "board, 30 s, a subscriber at 20 ms"
  wait "$sub" 2>/dev/null || true
  kill "$g" 2>/dev/null

  "$ctl" --socket "$sock" watch imu acceleration.x --period-ms 5 --count 9000 \
    2>"$work/imu.err" | "$work/lemnos-bench-stamp" --period-ms 5 --expect 9000 >"$work/imu.tsv" &
  imu=$!
  g=$(guard "$imu" 150)
  put cpu.200hz_pct "$(window 45)" "board, 45 s, the 200 Hz IMU run below"
  wait "$imu" || true
  kill "$g" 2>/dev/null
  cat "$work/imu.tsv"

  if "$ctl" --socket "$sock" read power0 >"$work/power.txt" 2>&1; then
    put power.readings "$(paste -sd ';' "$work/power.txt")" "board, lemnos-ctl read power0"
  else
    row power.readings not_measured "" "lemnos-ctl read power0 failed: $(tr '\n' ' ' <"$work/power.txt")"
  fi

  usb_cycle
fi

put threads.lemnosd "$(awk '/^Threads:/ { print $2 }' "/proc/$dpid/status" 2>/dev/null)" \
  "$where, lemnosd after the run"
put mem.pss_kb "$(awk '/^Pss:/ { print $2 }' "/proc/$dpid/smaps_rollup" 2>/dev/null)" \
  "$where, lemnosd PSS in kB after the run"

exit 0
