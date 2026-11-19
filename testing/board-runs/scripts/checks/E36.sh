# E36, detached (sourced by lemnos-check.sh run). Settings:
#   ALT_BOARD  the board file for the run: the image's /etc/lemnos/board.toml with its gadget device
#              replaced by E36's (`E36-0001`); it must keep the hw-watchdog device (required)
#   ECHO_BIN   lemnos-usb-echo built for the board (required)
#   WINDOW_S   how long the host side has for sections 1 to 4.4 (default 900)
#
# E36 serves its own gadget, so the image's link (the comm core's USB link, NCM) is gone for the
# whole run, not only for 4.5: the PC is the USB host of the E36 device for that time and has no
# login. The board side runs lemnosd on ALT_BOARD (a runtime drop-in, gone at the next boot),
# runs the echo tool, waits for the host side's end mark or WINDOW_S, then does 4.5 (stop lemnosd
# while the echo runs) and returns lemnosd to the image's own settings.
#
# The host side ends its window with a vendor OUT request carrying the 4 bytes "DONE"; the echo
# tool prints it as `request data [44, 4f, 4e, 45]` (testing/crates/usb-echo/src/lib.rs).
#
# Windows: "session" (WINDOW_S + 120 s: the image's settings back even if this script dies), and
# a DOWN_BOUND_S window around each lemnosd restart or stop.

WINDOW_S=${WINDOW_S:-900}
ALT_BOARD=${ALT_BOARD:-}
ECHO_BIN=${ECHO_BIN:-}
DONE_MARK='request data [44, 4f, 4e, 45]'

CHECK_HYPOTHESIS="E36 on the board, with lemnosd on E36's gadget for the run. Sections 1 to 4.4 are driven by the host side inside a $WINDOW_S s window and judged from its own record and the echo tool's output kept here. 4.5: lemnosd stopped while the echo runs; the echo tool ends, lemnosd's process is gone within 1 s of the stop, and no process of lemnosd, its helpers or the tool is left waiting in io_destroy. The image's settings and link come back after; each lemnosd down window stays under $DOWN_BOUND_S s."

check_precheck() {
  local ok=0
  [ -n "$ALT_BOARD" ] && [ -r "$ALT_BOARD" ] || { log precheck "ALT_BOARD '$ALT_BOARD' is not readable"; ok=1; }
  [ -n "$ECHO_BIN" ] && [ -x "$ECHO_BIN" ] || { log precheck "ECHO_BIN '$ECHO_BIN' is not executable"; ok=1; }
  [ "$ok" = 0 ] || return 1
  # Without hw-watchdog lemnosd on that file would not feed: the board would reset one period on.
  grep -Eq '^[[:space:]]*driver[[:space:]]*=[[:space:]]*"hw-watchdog"' "$ALT_BOARD" || { log precheck "ALT_BOARD has no hw-watchdog device: refused (no feeder)"; return 1; }
  grep -q 'E36-0001' "$ALT_BOARD" || { log precheck "ALT_BOARD has no E36 gadget (serial E36-0001)"; return 1; }
  cp "$ALT_BOARD" "$RUNDIR/board.toml" && chmod 0644 "$RUNDIR/board.toml"
  logout precheck lemnos-ctl validate "$RUNDIR/board.toml" || { log precheck "the board file does not validate"; return 1; }
}

# The image unit's start line with the run's board file (packaging/systemd/lemnosd.service).
alt_dropin() {
  printf '[Service]\nExecStart=\nExecStart=%s --board %s --user lemnos --group lemnos\n' "$LEMNOSD_BIN" "$RUNDIR/board.toml"
}

e36_into_alt() {
  local dns out
  window_open d1 "$DOWN_BOUND_S" || return 1
  dns=$(did_not_stop_count)
  dropin_install E36 "$(alt_dropin)"
  STOP_T=$(mono)
  out=$(bounded 40 systemctl restart "$LEMNOSD_UNIT" 2>&1)
  log d1 "restart onto the run's board file: exit $?${out:+: $out}"
  start_lemnosd d1
  window_close d1
  watchdog_account d1 "$STOP_T" "$ACTIVE_T" "$dns"
  gadget_snapshot d1
  local d
  d=$(gadget_dir) && log d1 "serialnumber '$(cat "$d/strings/0x409/serialnumber" 2>/dev/null)' idVendor '$(cat "$d/idVendor" 2>/dev/null)'"
  watchdog_ok || log d1 "WARNING: the watchdog row is not running ok on the run's board file: $(watchdog_row)"
}

stamp_lines() { while IFS= read -r l; do printf '%s %s\n' "$(stamp)" "$l"; done; }

# held: processes of lemnosd, its helpers and the echo tool with their wait channel.
held() { ps -eo pid=,stat=,wchan:32=,comm= 2>/dev/null | awk '$4 ~ /^(lemnosd|lemnosd-gsuper|lemnosd-gadget|lemnos-usb-echo)$/'; }

e36_45() {
  local dns pid t0 gone_at="" echo_at="" waits
  window_open d2 "$DOWN_BOUND_S" || { result 4.5 NOT-RUN "the timed restore could not be armed"; return; }
  dns=$(did_not_stop_count)
  pid=$(main_pid "$LEMNOSD_UNIT")
  log 4.5 "echo tool pid $ECHO_PID $([ -d "/proc/$ECHO_PID" ] && echo alive || echo 'ALREADY ENDED'); lemnosd pid $pid"
  t0=$(mono)
  STOP_T=$t0
  bounded 5 systemctl stop --no-block "$LEMNOSD_UNIT"
  log 4.5 "stop queued: exit $?"
  # 20 ms samples until 1 s after the stop (the record's bound): lemnosd's process gone, the tool
  # gone; then the wait channels. Kept short: the window's restore fires at DOWN_BOUND_S.
  while le "$(since "$t0")" 1; do
    [ -z "$gone_at" ] && [ ! -d "/proc/$pid" ] && gone_at=$(since "$t0")
    [ -z "$echo_at" ] && [ ! -d "/proc/$ECHO_PID" ] && echo_at=$(since "$t0")
    sleep 0.02
  done
  waits=$(held)
  log 4.5 "lemnosd gone at ${gone_at:-not within 1 s} s; echo tool ended at ${echo_at:-not within 1 s} s"
  log 4.5 "processes at 1 s: ${waits:-none}"
  dropin_remove
  start_lemnosd 4.5
  window_close d2
  watchdog_account 4.5 "$STOP_T" "$ACTIVE_T" "$dns"
  wait "$ECHO_PID" 2>/dev/null
  log 4.5 "echo tool exit status $?"
  if [ -n "$gone_at" ] && le "$gone_at" 1 && [ -n "$echo_at" ] && ! grep -q io_destroy <<<"$waits"; then
    result 4.5 PASS "lemnosd gone $gone_at s after the stop, the echo tool ended at $echo_at s, nothing in io_destroy"
  else
    result 4.5 FAIL "lemnosd gone at ${gone_at:-never} s (want <= 1), tool ended at ${echo_at:-never} s, io_destroy: $(grep -c io_destroy <<<"$waits")"
  fi
}

check_steps() {
  window_open session $((WINDOW_S + 120)) || { result run NOT-RUN "the session restore could not be armed"; return; }
  if ! e36_into_alt; then
    result d1 NOT-RUN "the restart onto the run's board file could not be armed"
    restore_all run
    window_close session
    return
  fi
  "$ECHO_BIN" "$LEMNOSD_SOCKET" > >(stamp_lines >"$RUNDIR/echo.out") 2>&1 &
  ECHO_PID=$!
  log echo "lemnos-usb-echo pid $ECHO_PID on $LEMNOSD_SOCKET; host window $WINDOW_S s; output in $RUNDIR/echo.out"
  local t0 mark=""
  t0=$(mono)
  while le "$(since "$t0")" "$WINDOW_S"; do
    grep -qF "$DONE_MARK" "$RUNDIR/echo.out" 2>/dev/null && { mark=yes; break; }
    [ -d "/proc/$ECHO_PID" ] || break
    sleep 0.5
  done
  log echo "window over after $(since "$t0") s: $([ -n "$mark" ] && echo "the host's DONE mark" || ([ -d "/proc/$ECHO_PID" ] && echo "WINDOW_S with no DONE mark" || echo "THE ECHO TOOL ENDED"))"
  result 1-4.4 MEASURED "host window $(since "$t0") s, DONE mark: ${mark:-no}; judged from the host's record and echo.out"
  e36_45
  restore_all run
  window_close session
}
