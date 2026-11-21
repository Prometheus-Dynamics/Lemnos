#!/usr/bin/env bash
# Boots a kernel in QEMU with an Intel 6300ESB watchdog (QEMU's reset action) and checks what the
# watchdog does around the boot path (M36c). Modes, run in order unless MODES names some:
#
#   slot-init  the real lemnos-slot-init is the guest's /init. It arms the watchdog at 15 s, names no
#              slot, so its refusal goes to the rescue: it waits 30 s, feeding the watchdog, and restarts.
#              Passes when it is armed at 15 s, the rescue's restart is what ends the guest, and the
#              guest was up past the 15 s timeout.
#   normal     the hook (crates/watchdog-init) arms the watchdog as slot-init does and hands PID 1 to the
#              userspace stand-in (crates/watchdog-user), which opens /dev/watchdog and feeds it for 45 s.
#              Passes when the stand-in feeds for the whole 45 s and powers off, and the hand-over gap
#              (the hook's last feed, made right before its exec, to the stand-in's first feed) is at
#              most a third of the applied period (HANDOVER_GAP_DIVISOR).
#   stall      the same hook, and the stand-in never opens /dev/watchdog. The watchdog must reset the
#              guest within about 15 s of the hook's last feed. Passes when the reset comes in that window.
#   daemon     the same hook, and the stand-in starts the real lemnosd as its unit does (lemnos-unit-check
#              exec, as 999:999) with /dev/watchdog0 given to the daemon's group (0660, as the packaged
#              udev rule leaves it), on a board with one device bound ahead of the watchdog. lemnosd's
#              root phase opens the keep-alive watchdog as root and feeds it at once; its bind takes that
#              descriptor. lemnosd's start trace gives the gap's pieces, each from the hook's last feed:
#              its exec, the root phase's feed, the end of its root phase (the drop), the watchdog's bind
#              (and the binds before it), its first tick. Passes when every piece is on the console, the
#              root phase's feed ends the unfed gap within a third of the period (HANDOVER_GAP_DIVISOR),
#              the first tick comes within the period after it, and the guest stays up 45 s.
#   daemon-late  as daemon, with the node root 0600 until 3 s after lemnosd starts (a udev that has not
#              finished its coldplug). Passes as daemon does: the rule is not on the watchdog's path.
#   daemon-root  as daemon, with the node root 0600 for good (no rule). Passes as daemon does: only the
#              root phase's open can work, and the bind takes it.
#   daemon-hang  as daemon-root, and the device ahead of the watchdog hangs in its bind (a FIFO nobody
#              writes, /hang.fifo in the guest's root). The host cuts the bind at its call deadline and
#              binds the watchdog next. Passes as daemon does, with the hung bind cut ("bind identity
#              failed") and the watchdog's bind begun within CALL_DEADLINE (+ slack) of the root phase's
#              feed: a hung bind delays the first tick by the deadline, not by the period.
#   daemon-stop  as daemon-root, and lemnosd is stopped (SIGSTOP) as soon as its root phase has fed: the
#              daemon hangs before it is healthy. Passes when the guest does not stay up, nothing feeds
#              after the root phase, and the reset comes within the period of the root phase's feed.
#   pid1-hang  as daemon-root, and PID 1 (this stand-in) hangs: its main thread blocks for good, reaping nothing. lemnosd
#              is healthy and feeds on. Passes when the guest stays up 45 s with PID 1 hung, and every piece is on the console:
#              the watchdog is fed by lemnosd, not by PID 1. (Nothing in Lemnos feeds the device on PID 1's behalf.)
#   pid1-hang-stop  as pid1-hang, and lemnosd is stopped (SIGSTOP) as soon as its root phase has fed. Nothing feeds: the reset
#              must come within the period of the root phase's feed, with PID 1 hung. Passes as daemon-stop does.
#   pid1-dead  as daemon-root, and PID 1 exits 3 s after lemnosd starts. The kernel panics ("Attempted to kill init") and,
#              with panic=-1, reboots at once: the guest ends within 5 s of PID 1's exit, by the panic, not by the timeout.
#   daemon-loop  node root 0600 (only a root-phase hold can feed); lemnosd is killed at the end of its root phase on every start and
#              started again 1 s later (Restart=always, RestartSec=1s). The stand-in manager keeps the descriptor lemnosd stores,
#              and each restart takes it back: passes when only the first start fed, every restart said it adopted the stored
#              descriptor and fed nothing, and the reset comes within the period of that feed.
#   daemon-loop-bind  node owner 999 at 0660 (a bind by name can open it, and an open of a running watchdog feeds it), and
#              lemnosd is killed after its watchdog's bind has ended, every start, before it is healthy: the restarts reach the
#              bind. Passes when every restart adopted the stored descriptor (its bind took it, no open by name), only the first
#              start fed, the stored descriptor was kept under its name, and the reset comes within the period of the first feed.
#              This is the PING-ON-OPEN case: on a tree that opens the node by name at a restart, the restarts feed it for good.

# The normal and stall modes stand in for lemnosd. The daemon modes run the real one, but not the
# systemd start ahead of it (switch_root to lemnosd's exec): that part is the image's, bounded by its
# unit ordering (docs/slots.md, "The watchdog chain"), and a board run measures it
# (testing/board-runs/WDT-CHAIN.md).
#
# The verdicts read the console of one QEMU run, after QEMU has exited. Each check names the exact
# line it looks for; when one is missing, the check prints the console's marker lines.
#
# Why the console is quiet during the hand-over: the watchdog core writes "watchdog0: watchdog did not
# stop!" (pr_crit) when the hook's descriptor closes at exec, because the hook does not write the magic
# close (the watchdog must keep running for the stall). The kernel prints that line from its console
# thread at the same moment the stand-in prints its marker, and the two interleave in the serial output:
# a marker can come out split by the kernel's line (seen: "user: stalled" intact, the rest of the line
# after the kernel's text). The normal and stall modes therefore run the console at level 2 (only
# emergency and alert messages reach it): their markers are the hook's and the stand-in's own stdout,
# and no kernel message they need is at a lower level. The slot-init mode keeps level 6, because
# lemnos-slot-init logs through /dev/kmsg at the default level, which level 2 would drop; it never closes
# the watchdog, so no crit line is printed in that mode.
#
#   KERNEL  the bootable image of a kernel with CONFIG_I6300ESB_WDT=y (required), e.g. the pinned
#           modules-off bzImage (tmp/pinned-2026-11-14-d/k-modules-off/build/arch/x86/boot/bzImage)
#   CARGO   the cargo to build the guests for musl with (default: cargo; the group's e-cargo.sh)
#   MODES   the modes to run (default: "slot-init normal stall daemon daemon-late daemon-root daemon-hang daemon-stop daemon-loop daemon-loop-bind")
#   ACCEL   QEMU's accelerator (default: kvm when /dev/kvm is usable, else tcg; tcg for a slow board's timing)
#   LOGS    where the console logs are kept (default: qemu-watchdog under TMPDIR, or /tmp)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
KERNEL="${KERNEL:?set KERNEL to the bzImage of a kernel with CONFIG_I6300ESB_WDT=y}"
CARGO="${CARGO:-cargo}"
MODES="${MODES:-slot-init normal stall daemon daemon-late daemon-root daemon-hang daemon-stop daemon-loop daemon-loop-bind pid1-hang pid1-hang-stop pid1-dead}"
LOGS="${LOGS:-${TMPDIR:-/tmp}/qemu-watchdog}"
target=x86_64-unknown-linux-musl
DOG_SECONDS=15
TIMING_SLACK=3
# The hand-over bound (docs/slots.md, "The watchdog chain", window 4): the gap from the last feed before the
# exec to the first feed after it must be at most applied / HANDOVER_GAP_DIVISOR, where applied is the period
# the hook reads back. A third: the gap is time nothing feeds, so it must be well under the period; a third
# leaves two thirds of the period for the host's delays.
HANDOVER_GAP_DIVISOR=3
# The host's call deadline (lemnos_host::Options::call_deadline's default, 2 s): a bind that does not
# return within it is cut, and the next device's bind begins.
CALL_DEADLINE=2

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
accel="${ACCEL:-}"
if [[ -z "$accel" ]]; then accel=tcg; if [[ -r /dev/kvm && -w /dev/kvm ]]; then accel=kvm; fi; fi

# The real slot-init, and the hook and the stand-in: all Rust, static musl for the initramfs (CZ-GUEST).
(cd "$repo" && "$CARGO" build -q -p lemnos-slot-init -p lemnos-watchdog-init -p lemnos-watchdog-user \
  -p lemnosd -p lemnos-unit-check --target "$target")
guests="${CARGO_TARGET_DIR:-$repo/target}/$target/debug"
slot_init="$guests/lemnos-slot-init"
cp "$guests/lemnos-watchdog-init" "$work/watchdog-init"
cp "$guests/lemnos-watchdog-user" "$work/watchdog-user"
# lemnosd and the unit launcher, stripped: the initramfs stays small.
strip -o "$work/lemnosd" "$guests/lemnosd"
strip -o "$work/lemnos-unit-check" "$guests/lemnos-unit-check"

fail() { echo "qemu-watchdog: $*" >&2; exit 1; }

# The console marker lines of a text log, for a failed check to show.
markers() { grep -nE '^(hook|user|slot-init):' "$1" | head -40 >&2 || true; }

# expect MODE TEXT LINE: the console (TEXT) has a line containing LINE; otherwise the check fails and
# prints the marker lines it did find.
expect() {
  local mode="$1" text="$2" want="$3"
  if ! grep -qF -- "$want" "$text"; then
    echo "qemu-watchdog: $mode: no console line contains: $want" >&2
    echo "qemu-watchdog: $mode: the console's marker lines were:" >&2
    markers "$text"
    fail "$mode check failed"
  fi
}

# One guest per mode: its /init and its command line, and the stand-in when there is one.
run_mode() {
  local mode="$1" root="$work/root-$1" log="$work/console-$1.stamped" text="$work/text-$1.log" extra="" status=0 start level=6
  rm -rf "$root"
  mkdir -p "$root"/{dev,proc,sys,sbin}
  case "$mode" in
    slot-init) cp "$slot_init" "$root/init" ;;
    normal) cp "$work/watchdog-init" "$root/init"; cp "$work/watchdog-user" "$root/sbin/watchdog-user"; level=2 ;;
    stall) cp "$work/watchdog-init" "$root/init"; cp "$work/watchdog-user" "$root/sbin/watchdog-user"; extra="lemnos.watchdog=stall"; level=2 ;;
    daemon|daemon-late|daemon-root|daemon-hang|daemon-stop|daemon-loop|daemon-loop-bind|pid1-hang|pid1-hang-stop|pid1-dead)
      cp "$work/watchdog-init" "$root/init"; cp "$work/watchdog-user" "$root/sbin/watchdog-user"
      cp "$work/lemnosd" "$root/sbin/lemnosd"; cp "$work/lemnos-unit-check" "$root/sbin/lemnos-unit-check"
      mkdir -p "$root"/{run,tmp}
      if [[ "$mode" == daemon-hang ]]; then mkfifo -m 0666 "$root/hang.fifo"; fi
      extra="lemnos.watchdog=$mode lemnos.serial=QEMUWDT0001"; level=2 ;;
    *) echo "qemu-watchdog: no mode $mode" >&2; exit 2 ;;
  esac
  (cd "$root" && find . -print0 | cpio --quiet --null -o -H newc | gzip -1) >"$work/initramfs-$mode"
  start="$(date +%s.%N)"
  # -no-reboot: a restart or a watchdog reset ends QEMU, so the run's end is the guest's end.
  # loglevel: see the header (2 for the hand-over modes, 6 for slot-init).
  # Each console line is stamped with the time it arrives, for the stall's timing.
  timeout 180 qemu-system-x86_64 -accel "$accel" -cpu max -smp 2 -m 512 -nographic -no-reboot \
    -device i6300esb -watchdog-action reset \
    -kernel "$KERNEL" -initrd "$work/initramfs-$mode" \
    -append "console=ttyS0 panic=-1 oops=panic loglevel=$level printk.time=1 $extra" \
    2>&1 | {
      : >"$log"
      while IFS= read -r line; do
        printf '%s %s\n' "$(date +%s.%N)" "$line" >>"$log"
        printf '%s\n' "$line"
      done
    } >"$work/raw-$mode.log" || status=$?
  end="$(date +%s.%N)"  # global: the stall check reads it
  echo "qemu-watchdog: $mode ended after $(awk -v a="$start" -v b="$end" 'BEGIN{printf "%.1f", b-a}') s, status $status" >&2
  # QEMU has exited and its output is closed: the checks read the whole console, without the stamps.
  sed 's/^[0-9.]* //' "$log" >"$text"
  # The console logs are kept (LOGS) for a failed run to be read afterwards.
  mkdir -p "$LOGS" || fail "cannot keep the console logs: $LOGS cannot be made"
  cp "$log" "$LOGS/console-$mode.stamped.log"
  cp "$text" "$LOGS/console-$mode.log"
  [[ "$status" == 0 ]] || fail "$mode: QEMU ended with status $status (124 is the 180 s timeout)"
}

# piece TEXT NAME: the seconds after the hook's last feed of the daemon's piece NAME, or empty.
piece() { tr -d '\r' <"$1" | sed -n "s/.*user: piece: $2 at \([0-9.-]*\) s after the hook's last feed.*/\1/p" | head -1; }

# The pieces of the gap, printed for the record and checked present.
pieces() {
  local mode="$1" text="$2" p v
  for p in 'exec' 'root-phase feed' 'drop' 'watchdog bind begins ([0-9]* binds before it)' 'watchdog bind ok' 'watchdog first feed'; do
    v="$(piece "$text" "$p")"
    [[ -n "$v" ]] || { echo "qemu-watchdog: $mode: no piece: $p" >&2; markers "$text"; fail "$mode check failed"; }
    echo "qemu-watchdog: $mode: piece: $(tr -d '\r' <"$text" | grep -o "user: piece: $p" | head -1 | sed 's/user: piece: //') at ${v} s" >&2
  done
}

passed=()
for mode in $MODES; do
  run_mode "$mode"
  text="$work/text-$mode.log"
  case "$mode" in
    slot-init)
      expect "$mode" "$text" 'slot-init: watchdog armed: 15 s'
      expect "$mode" "$text" 'not a Lemnos disk: no flashing mode is asked'
      expect "$mode" "$text" 'restarting in 30 s'
      if grep -qi 'watchdog.*reset\|Watchdog timer' "$text"; then fail "slot-init: the guest was reset by the watchdog during the rescue wait"; fi
      ;;
    normal)
      expect "$mode" "$text" 'user: first open of /dev/watchdog'
      expect "$mode" "$text" 'fed the watchdog for 45 s and the guest stayed up'
      # The hand-over gap: the hook's last feed (right before its exec) to the stand-in's first feed.
      expect "$mode" "$text" 'hook: watchdog armed: '
      expect "$mode" "$text" 'user: first feed of /dev/watchdog'
      # The serial console ends lines with CR and can put terminal codes before them: the markers are matched anywhere.
      applied="$(tr -d '\r' <"$text" | sed -n 's/.*hook: watchdog armed: \([0-9]*\) s$/\1/p' | head -1)"
      gap="$(sed -n 's/.*user: first feed of \/dev\/watchdog \([0-9.]*\) s after.*/\1/p' "$text" | head -1)"
      [[ -n "$applied" && -n "$gap" ]] || fail "normal: the hand-over lines (the applied period, the first feed) are not in the console"
      bound="$(awk -v a="$applied" -v d="$HANDOVER_GAP_DIVISOR" 'BEGIN{printf "%.3f", a/d}')"
      echo "qemu-watchdog: normal: the hand-over gap is ${gap} s; the bound is ${bound} s (a third of the ${applied} s applied period)" >&2
      awk -v g="$gap" -v b="$bound" 'BEGIN{exit !(g <= b)}' || fail "normal: the hand-over gap ${gap} s is above the bound ${bound} s"
      ;;
    stall)
      expect "$mode" "$text" 'user: stalled after the handover'
      # The reset must come within the timeout of the last feed. The stall line's arrival time is
      # the host's stamp of it; it says how long ago the last feed was, so the reset's deadline is
      # the stall line plus (timeout - that age), with a small slack for the host's reading.
      stalled="$(awk '/user: stalled after the handover/ {print $1; exit}' "$work/console-stall.stamped")"
      age="$(sed -n 's/.*the last feed was \([0-9.]*\) s ago.*/\1/p' "$text" | head -1)"
      [[ -n "$age" ]] || fail "stall: the stall line has no age of the last feed"
      elapsed="$(awk -v a="$stalled" -v b="$end" 'BEGIN{printf "%.1f", b-a}')"
      window="$(awk -v age="$age" -v t="$DOG_SECONDS" -v s="$TIMING_SLACK" 'BEGIN{printf "%.1f", t-age+s}')"
      echo "qemu-watchdog: stall: the guest ended ${elapsed} s after the stall line (the reset is due by ${window} s)" >&2
      awk -v e="$elapsed" -v w="$window" 'BEGIN{exit !(e <= w)}' || fail "stall: the guest ended ${elapsed} s after the stall line, past the ${window} s window"
      ;;
    daemon|daemon-late|daemon-root)
      expect "$mode" "$text" "user: daemon ($mode): starting lemnosd under the unit"
      pieces "$mode" "$text"
      expect "$mode" "$text" "user: daemon ($mode): the guest stayed up 45 s on lemnosd's feeding"
      fed="$(piece "$text" 'root-phase feed')"
      first="$(piece "$text" 'watchdog first feed')"
      bound="$(awk -v a="$DOG_SECONDS" -v d="$HANDOVER_GAP_DIVISOR" 'BEGIN{printf "%.3f", a/d}')"
      echo "qemu-watchdog: $mode: the unfed gap ends at the root phase's feed, ${fed} s after the hook's last feed (bound ${bound} s); the first tick is ${first} s" >&2
      awk -v g="$fed" -v b="$bound" 'BEGIN{exit !(g <= b)}' || fail "$mode: the root phase's feed ${fed} s is past the bound ${bound} s"
      awk -v f="$fed" -v g="$first" -v t="$DOG_SECONDS" 'BEGIN{exit !(g - f < t)}' || fail "$mode: the first tick ${first} s is not within the ${DOG_SECONDS} s period of the root phase's feed ${fed} s"
      ;;
    daemon-hang)
      expect "$mode" "$text" "user: daemon ($mode): starting lemnosd under the unit"
      pieces "$mode" "$text"
      expect "$mode" "$text" 'start trace: '
      expect "$mode" "$text" ' bind identity failed'
      expect "$mode" "$text" "user: daemon ($mode): the guest stayed up 45 s on lemnosd's feeding"
      fed="$(piece "$text" 'root-phase feed')"
      begun="$(piece "$text" 'watchdog bind begins ([0-9]* binds before it)')"
      first="$(piece "$text" 'watchdog first feed')"
      echo "qemu-watchdog: daemon-hang: the hung bind ahead was cut; the watchdog's bind began ${begun} s and its first tick ${first} s after the hook's last feed (root phase's feed ${fed} s)" >&2
      awk -v f="$fed" -v b="$begun" -v d="$CALL_DEADLINE" -v s="$TIMING_SLACK" 'BEGIN{exit !(b - f <= d + s)}' || fail "daemon-hang: the watchdog's bind began ${begun} s, more than the ${CALL_DEADLINE} s call deadline after the root phase's feed ${fed} s"
      awk -v f="$fed" -v g="$first" -v t="$DOG_SECONDS" 'BEGIN{exit !(g - f < t)}' || fail "daemon-hang: the first tick ${first} s is not within the period of the root phase's feed"
      ;;
    daemon-stop)
      expect "$mode" "$text" "user: daemon ($mode): starting lemnosd under the unit"
      expect "$mode" "$text" "user: daemon (daemon-stop): lemnosd stopped after its root phase's feed"
      if grep -qF "the guest stayed up" "$text"; then fail "daemon-stop: the guest stayed up with lemnosd stopped"; fi
      if [[ -n "$(piece "$text" 'watchdog first feed')" ]]; then fail "daemon-stop: the watchdog was fed after the root phase with lemnosd stopped"; fi
      [[ -n "$(piece "$text" 'root-phase feed')" ]] || { markers "$text"; fail "daemon-stop: no root-phase feed"; }
      # The reset is due within the period of the root phase's feed: the stamp of its piece line.
      fedat="$(awk '/user: piece: root-phase feed/ {print $1; exit}' "$work/console-$mode.stamped")"
      elapsed="$(awk -v a="$fedat" -v b="$end" 'BEGIN{printf "%.1f", b-a}')"
      window=$((DOG_SECONDS + TIMING_SLACK))
      echo "qemu-watchdog: daemon-stop: the guest ended ${elapsed} s after the root phase's feed (due by ${window} s)" >&2
      awk -v e="$elapsed" -v w="$window" 'BEGIN{exit !(e <= w)}' || fail "daemon-stop: the guest ended ${elapsed} s after the root phase's feed, past ${window} s"
      ;;
    daemon-loop)
      expect "$mode" "$text" "user: daemon (daemon-loop): lemnosd start 2 killed after its root phase"
      # A restart takes the descriptor the store kept, and does not open the node by name.
      expect "$mode" "$text" 'root phase: watchdog adopted, not fed'
      if grep -qF "the guest stayed up" "$text"; then fail "daemon-loop: the guest stayed up through the restarts"; fi
      feeds="$(grep -c 'user: piece: root-phase feed' "$text" || true)"
      [[ "$feeds" == 1 ]] || fail "daemon-loop: ${feeds} starts fed the watchdog; only the boot's first may"
      fedat="$(awk '/user: piece: root-phase feed/ {print $1; exit}' "$work/console-$mode.stamped")"
      elapsed="$(awk -v a="$fedat" -v b="$end" 'BEGIN{printf "%.1f", b-a}')"
      window=$((DOG_SECONDS + TIMING_SLACK))
      starts="$(grep -c 'killed after its root phase' "$text" || true)"
      echo "qemu-watchdog: daemon-loop: ${starts} starts, one fed; the guest ended ${elapsed} s after that feed (due by ${window} s)" >&2
      awk -v e="$elapsed" -v w="$window" 'BEGIN{exit !(e <= w)}' || fail "daemon-loop: the guest ended ${elapsed} s after the first start's feed, past ${window} s"
      ;;
    daemon-loop-bind)
      # The symptom first: a restart's open of the node by name feeds a lemnosd that never got healthy.
      if grep -qF "the guest stayed up" "$text"; then fail "daemon-loop-bind: the guest stayed up through the restarts: a restart's open of the node fed it"; fi
      expect "$mode" "$text" "user: daemon (daemon-loop-bind): lemnosd start 2 killed after its watchdog bind"
      expect "$mode" "$text" 'user: manager: stored 1 descriptor(s) as lemnos-watchdog'
      expect "$mode" "$text" 'root phase: watchdog adopted, not fed'
      feeds="$(grep -c 'user: piece: root-phase feed' "$text" || true)"
      [[ "$feeds" == 1 ]] || fail "daemon-loop-bind: ${feeds} starts fed the watchdog; only the boot's first may"
      fedat="$(awk '/user: piece: root-phase feed/ {print $1; exit}' "$work/console-$mode.stamped")"
      elapsed="$(awk -v a="$fedat" -v b="$end" 'BEGIN{printf "%.1f", b-a}')"
      window=$((DOG_SECONDS + TIMING_SLACK))
      starts="$(grep -c 'killed after its watchdog bind' "$text" || true)"
      echo "qemu-watchdog: daemon-loop-bind: ${starts} starts, one fed; the guest ended ${elapsed} s after that feed (due by ${window} s)" >&2
      awk -v e="$elapsed" -v w="$window" 'BEGIN{exit !(e <= w)}' || fail "daemon-loop-bind: the guest ended ${elapsed} s after the first start's feed, past ${window} s"
      ;;
    pid1-hang)
      expect "$mode" "$text" "user: daemon (pid1-hang): PID 1 is hung"
      expect "$mode" "$text" "user: daemon (pid1-hang): the guest stayed up 45 s with PID 1 hung; powering off"
      pieces "$mode" "$text"
      fed="$(piece "$text" 'root-phase feed')"
      first="$(piece "$text" 'watchdog first feed')"
      awk -v f="$fed" -v g="$first" -v t="$DOG_SECONDS" 'BEGIN{exit !(g - f < t)}' || fail "pid1-hang: the first tick ${first} s is not within the period of the root phase's feed ${fed} s"
      echo "qemu-watchdog: pid1-hang: PID 1 hung; lemnosd kept the board up: root phase's feed ${fed} s, first tick ${first} s after the hook's last feed" >&2
      ;;
    pid1-hang-stop)
      expect "$mode" "$text" "user: daemon (pid1-hang-stop): PID 1 is hung"
      expect "$mode" "$text" "user: daemon (pid1-hang-stop): lemnosd stopped after its root phase's feed"
      if grep -qF "stayed up 45 s with PID 1 hung" "$text"; then fail "pid1-hang-stop: the guest stayed up with lemnosd stopped"; fi
      if [[ -n "$(piece "$text" 'watchdog first feed')" ]]; then fail "pid1-hang-stop: the watchdog was fed after the root phase with lemnosd stopped"; fi
      fedat="$(awk '/user: piece: root-phase feed/ {print $1; exit}' "$work/console-$mode.stamped")"
      elapsed="$(awk -v a="$fedat" -v b="$end" 'BEGIN{printf "%.1f", b-a}')"
      window=$((DOG_SECONDS + TIMING_SLACK))
      echo "qemu-watchdog: pid1-hang-stop: PID 1 hung and lemnosd stopped: the guest ended ${elapsed} s after the root phase's feed (due by ${window} s)" >&2
      awk -v e="$elapsed" -v w="$window" 'BEGIN{exit !(e <= w)}' || fail "pid1-hang-stop: the guest ended ${elapsed} s after the root phase's feed, past ${window} s"
      ;;
    pid1-dead)
      expect "$mode" "$text" "user: daemon (pid1-dead): PID 1 exits now"
      expect "$mode" "$text" "Kernel panic - not syncing: Attempted to kill init"
      if grep -qF "the guest stayed up" "$text"; then fail "pid1-dead: the guest stayed up after PID 1 exited"; fi
      exitedat="$(awk '/user: daemon \(pid1-dead\): PID 1 exits now/ {print $1; exit}' "$work/console-$mode.stamped")"
      elapsed="$(awk -v a="$exitedat" -v b="$end" 'BEGIN{printf "%.1f", b-a}')"
      echo "qemu-watchdog: pid1-dead: the guest ended ${elapsed} s after PID 1 exited; the kernel's panic reset it" >&2
      awk -v e="$elapsed" 'BEGIN{exit !(e <= 5)}' || fail "pid1-dead: the guest ended ${elapsed} s after PID 1 exited, not at once"
      ;;
  esac
  passed+=("$mode")
done
echo "qemu-watchdog: passed (${passed[*]}) on $accel"
