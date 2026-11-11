#!/usr/bin/env bash
# The steps bench.sh runs against one lemnosd, in one of two modes:
#   sim    on the build host, against lemnosd --sim (dry run): the board file check, the device
#          list, the thread count and the memory. No timing, no hardware, no systemctl.
#   board  on the board, under the board lock taken by bench.sh --real: the reference set's timings,
#          the power readings and the USB port power cycle, against the real buses.
# Prints TSV rows (key, status, value, note) on stdout. lemnosd is stopped by its PID on exit.
#
#   board-side.sh sim|board WORKDIR
#
# WORKDIR holds lemnosd, lemnos-ctl, board.toml and stamp.py.
set -u

mode=${1:?sim or board}
work=${2:?the work directory}
srv=$work/lemnosd
ctl=$work/lemnos-ctl
sock=$work/lemnosd.sock
sim=()
where="board"
if [ "$mode" = sim ]; then
  sim=(--sim)
  where="sim: build host, not the board"
fi
usb_off=0
pid=

row() { printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4"; }

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
  if [ -n "$pid" ]; then
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
  fi
}
trap cleanup EXIT

if ! "$srv" "${sim[@]}" --check --board "$work/board.toml" >"$work/check.txt" 2>&1; then
  row lemnosd.check not_measured "" "board.toml does not parse: $(tr '\n' ' ' <"$work/check.txt")"
  exit 1
fi
row lemnosd.check measured ok "board.toml parses; lemnosd --check, $where"

"$srv" "${sim[@]}" --board "$work/board.toml" --socket "$sock" >"$work/lemnosd.log" 2>&1 &
pid=$!
up=0
for _ in $(seq 1 100); do
  if [ -S "$sock" ]; then
    up=1
    break
  fi
  kill -0 "$pid" 2>/dev/null || break
  sleep 0.1
done
if [ "$up" != 1 ]; then
  row lemnosd.start not_measured "" "lemnosd did not open its socket: $(tail -n 3 "$work/lemnosd.log" | tr '\n' ' ')"
  exit 1
fi
row lemnosd.start measured up "lemnosd on the bench board file, $where"

"$ctl" --socket "$sock" list >"$work/list.txt" 2>&1
put lemnosd.devices "$(paste -sd ';' "$work/list.txt")" "lemnos-ctl list, $where"

ticks() { awk '{ print $14 + $15 }' "/proc/$pid/stat" 2>/dev/null; }

# window SECONDS: lemnosd's CPU share over that many seconds, in percent, from its utime and stime.
window() {
  local t0 t1 s0 s1
  t0=$(ticks)
  s0=$(date +%s%N)
  sleep "$1"
  t1=$(ticks)
  s1=$(date +%s%N)
  awk -v d="$((t1 - t0))" -v ns="$((s1 - s0))" -v hz="$(getconf CLK_TCK)" \
    'BEGIN { printf "%.2f", d / hz / (ns / 1e9) * 100 }'
}

usb_count() { ls /sys/bus/usb/devices | grep -c -E '^[0-9]+-[0-9.]+$' || true; }

# settle lt|ge COUNT: waits for the sysfs USB device count to drop below, or return to at least,
# COUNT. Prints the wait in milliseconds. Gives up after 15 s.
settle() {
  local start now n
  start=$(date +%s%N)
  while :; do
    n=$(usb_count)
    if { [ "$1" = lt ] && [ "$n" -lt "$2" ]; } || { [ "$1" = ge ] && [ "$n" -ge "$2" ]; }; then
      break
    fi
    now=$(date +%s%N)
    if [ $(((now - start) / 1000000)) -ge 15000 ]; then
      return 1
    fi
    sleep 0.05
  done
  now=$(date +%s%N)
  echo $(((now - start) / 1000000))
}

usb_cycle() {
  local before drop back
  before=$(usb_count)
  if ! "$ctl" --socket "$sock" off usb-a-power >"$work/usb.txt" 2>&1; then
    row usb.cycle not_measured "" "lemnos-ctl off usb-a-power failed: $(tr '\n' ' ' <"$work/usb.txt")"
    return
  fi
  usb_off=1
  drop=$(settle lt "$before") || drop=
  if "$ctl" --socket "$sock" on usb-a-power >"$work/usb.txt" 2>&1; then
    usb_off=0
  fi
  back=$(settle ge "$before") || back=
  if [ -n "$drop" ] && [ -n "$back" ] && [ "$usb_off" = 0 ]; then
    row usb.cycle measured "devices $before, down after $drop ms, back after $back ms" \
      "board, usb-a-power (the camera's port); devices counted in sysfs"
  else
    row usb.cycle not_measured "" \
      "cycle incomplete: devices $before, down ${drop:-no}, back ${back:-no}, switch off ${usb_off}"
  fi
}

if [ "$mode" = board ]; then
  put cpu.idle_pct "$(window 30)" "board, 30 s, no subscriber"

  timeout 60 "$ctl" --socket "$sock" watch imu acceleration --period-ms 20 --count 1500 \
    >/dev/null 2>&1 &
  sub=$!
  put cpu.50hz_pct "$(window 30)" "board, 30 s, a subscriber at 20 ms"
  wait "$sub" 2>/dev/null || true

  timeout 120 "$ctl" --socket "$sock" watch imu acceleration --period-ms 5 --count 9000 \
    2>"$work/imu.err" | python3 -I "$work/stamp.py" --period-ms 5 --expect 9000 >"$work/imu.tsv" &
  imu=$!
  put cpu.200hz_pct "$(window 45)" "board, 45 s, the 200 Hz IMU run below"
  wait "$imu" || true
  cat "$work/imu.tsv"

  if "$ctl" --socket "$sock" read power0 >"$work/power.txt" 2>&1; then
    put power.readings "$(paste -sd ';' "$work/power.txt")" "board, lemnos-ctl read power0"
  else
    row power.readings not_measured "" "lemnos-ctl read power0 failed: $(tr '\n' ' ' <"$work/power.txt")"
  fi

  usb_cycle
fi

put threads.lemnosd "$(awk '/^Threads:/ { print $2 }' "/proc/$pid/status" 2>/dev/null)" \
  "$where, lemnosd after the run"
put mem.pss_kb "$(awk '/^Pss:/ { print $2 }' "/proc/$pid/smaps_rollup" 2>/dev/null)" \
  "$where, lemnosd PSS in kB after the run"

exit 0
