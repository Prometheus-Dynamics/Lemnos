# H4a, detached (sourced by lemnos-check.sh run). Settings:
#   ALT_BOARD    the image's /etc/lemnos/board.toml plus the record's [[each]] camera entry; it must
#                keep the hw-watchdog device and the image's usb-gadget device (both required)
#   H4A_MODE     step5 (default): check 5, hints off, a camera plugged by a person, timed;
#                hold: lemnosd on ALT_BOARD with hints on, held for checks 1 to 4 over the session
#                until `lemnos-check.sh release H4a` or HOLD_S
#   PLUG_WAIT_S  step5: how long to wait for the plug (default 600)
#   HOLD_S       hold: the longest hold (default 1800)
#
# The image's root is read-only, so the [[each]] entry cannot go into /etc/lemnos/board.toml; and
# the unit passes --board, so LEMNOSD_BOARD (the record's wording) has no effect under it
# (packaging/systemd/lemnosd.env.example). Both modes therefore run lemnosd on ALT_BOARD through a
# runtime drop-in (gone at the next boot). Step 5's "block uevents" is done in the same drop-in:
# RestrictAddressFamilies reset to AF_UNIX, so the daemon's uevent socket (AF_NETLINK) is refused
# and it logs `not watching for hardware coming and going` (service/lemnosd/src/main.rs).
# ALT_BOARD keeps the image's gadget, so the link comes back after each restart: lemnosd is down
# only for the two restarts, each bounded by DOWN_BOUND_S.

H4A_MODE=${H4A_MODE:-step5}
PLUG_WAIT_S=${PLUG_WAIT_S:-600}
HOLD_S=${HOLD_S:-1800}
ALT_BOARD=${ALT_BOARD:-}

CHECK_HYPOTHESIS="H4a ($H4A_MODE) on the board. step5: with lemnosd's uevent socket refused (its log says it is not watching), a UVC camera plugged in by a person is bound as camera@usb:B-P within 8 s of its USB device appearing, by the look the retry cap brings. hold: lemnosd runs ALT_BOARD with hints on for checks 1 to 4, then returns to the image's settings. Each lemnosd down window stays under $DOWN_BOUND_S s."

check_precheck() {
  [ -n "$ALT_BOARD" ] && [ -r "$ALT_BOARD" ] || { log precheck "ALT_BOARD '$ALT_BOARD' is not readable"; return 1; }
  grep -Eq '^[[:space:]]*driver[[:space:]]*=[[:space:]]*"hw-watchdog"' "$ALT_BOARD" || { log precheck "ALT_BOARD has no hw-watchdog device: refused (no feeder)"; return 1; }
  grep -Eq '^[[:space:]]*driver[[:space:]]*=[[:space:]]*"usb-gadget"' "$ALT_BOARD" || { log precheck "ALT_BOARD has no usb-gadget device: refused (the link would stay down)"; return 1; }
  grep -Eq 'usb:class:0e' "$ALT_BOARD" || { log precheck "ALT_BOARD has no [[each]] usb:class:0e entry"; return 1; }
  case $H4A_MODE in step5 | hold) ;; *) log precheck "H4A_MODE '$H4A_MODE' is not step5 or hold"; return 1 ;; esac
  cp "$ALT_BOARD" "$RUNDIR/board.toml" && chmod 0644 "$RUNDIR/board.toml"
  logout precheck lemnos-ctl validate "$RUNDIR/board.toml" || { log precheck "the board file does not validate"; return 1; }
}

h4a_dropin() {
  printf '[Service]\nExecStart=\nExecStart=%s --board %s --user lemnos --group lemnos\n' "$LEMNOSD_BIN" "$RUNDIR/board.toml"
  [ "$H4A_MODE" = step5 ] && printf 'RestrictAddressFamilies=\nRestrictAddressFamilies=AF_UNIX\n'
}

# restart_window NAME: one bounded lemnosd restart (after the drop-in is in place or removed).
restart_window() {
  local name=$1 dns out
  window_open "$name" "$DOWN_BOUND_S" || return 1
  dns=$(did_not_stop_count)
  STOP_T=$(mono)
  out=$(bounded 40 systemctl restart "$LEMNOSD_UNIT" 2>&1)
  log "$name" "restart: exit $?${out:+: $out}"
  start_lemnosd "$name"
  window_close "$name"
  watchdog_account "$name" "$STOP_T" "$ACTIVE_T" "$dns"
}

# usb_cameras: the ports (B-P) with a video-class interface, one per line.
usb_cameras() {
  local f
  for f in "$SYSFS"/bus/usb/devices/*:*/bInterfaceClass; do
    [ -e "$f" ] || continue
    [ "$(cat "$f" 2>/dev/null)" = 0e ] || continue
    f=${f%/bInterfaceClass}
    f=${f##*/}
    printf '%s\n' "${f%%:*}"
  done | sort -u
}

h4a_step5() {
  local e0 before port="" t_plug t_run="" n=0 row
  local base
  base=$(usb_cameras | tr '\n' ' ')
  log step5 "cameras before: [${base}]"
  e0=$(epoch)
  restart_window r1 || { result step5 NOT-RUN "the restart could not be armed"; return; }
  if ! journal_since "$LEMNOSD_UNIT" "$e0" | grep -q 'not watching for hardware coming and going'; then
    logout step5 journal_since "$LEMNOSD_UNIT" "$e0"
    result step5 NOT-RUN "lemnosd did not log 'not watching for hardware coming and going': hints may still be on"
    return
  fi
  log step5 "hints off: lemnosd logged 'not watching for hardware coming and going'"
  log step5 "PLUG NOW: a person plugs one UVC camera into a hub port on the board (waiting up to $PLUG_WAIT_S s)"
  before=$(mono)
  while le "$(since "$before")" "$PLUG_WAIT_S"; do
    port=$(comm -13 <(printf '%s' "$base" | tr ' ' '\n' | sed '/^$/d' | sort) <(usb_cameras) | head -n1)
    [ -n "$port" ] && break
    sleep 0.1
  done
  if [ -z "$port" ]; then
    result step5 NOT-RUN "no camera was plugged within $PLUG_WAIT_S s"
    return
  fi
  t_plug=$(mono)
  log step5 "camera on port $port seen in sysfs"
  while [ "$n" -lt 150 ]; do
    row=$(device_row "camera@usb:$port")
    if awk '$3 == "running" { f = 1 } END { exit !f }' <<<"$row"; then
      t_run=$(since "$t_plug")
      break
    fi
    sleep 0.2
    n=$((n + 1))
  done
  logout step5 lemnos-ctl list
  if [ -n "$t_run" ] && le "$t_run" 8.5; then
    result step5 PASS "camera@usb:$port running $t_run s after its USB device appeared (want <= 8 s + one look)"
  else
    result step5 FAIL "camera@usb:$port running after: ${t_run:-not within 30 s} s (want <= 8 s); row: ${row:-none}"
  fi
}

h4a_hold() {
  local t0
  restart_window r1 || { result hold NOT-RUN "the restart could not be armed"; return; }
  logout hold lemnos-ctl list
  log hold "HOLD: lemnosd runs the run's board file; run checks 1 to 4 over the session; end with 'lemnos-check.sh release H4a' (at most $HOLD_S s)"
  t0=$(mono)
  while le "$(since "$t0")" "$HOLD_S" && [ ! -e "$RUNDIR/release" ]; do sleep 1; done
  log hold "hold over after $(since "$t0") s: $([ -e "$RUNDIR/release" ] && echo released || echo HOLD_S)"
  logout hold lemnos-ctl list
  result hold MEASURED "held $(since "$t0") s"
}

check_steps() {
  local bound
  if [ "$H4A_MODE" = hold ]; then bound=$((HOLD_S + 120)); else bound=$((PLUG_WAIT_S + 120)); fi
  window_open session "$bound" || { result run NOT-RUN "the session restore could not be armed"; return; }
  dropin_install H4a "$(h4a_dropin)"
  if [ "$H4A_MODE" = hold ]; then h4a_hold; else h4a_step5; fi
  # Back to the image's settings: the drop-in out, one more bounded restart.
  dropin_remove
  restart_window r2 || restore_all run
  restore_all run
  window_close session
}
