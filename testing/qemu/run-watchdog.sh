#!/usr/bin/env bash
# Boots a kernel in QEMU with an Intel 6300ESB watchdog (QEMU's reset action) and checks what the
# watchdog does around the boot path (M36c). Modes, run in order unless MODES names some:
#
#   slot-init  the real lemnos-slot-init is the guest's /init. It arms the watchdog at 15 s, names no
#              slot, so its refusal goes to the rescue: it waits 30 s, feeding the watchdog, and restarts.
#              Passes when it is armed at 15 s, the rescue's restart is what ends the guest, and the
#              guest was up past the 15 s timeout.
#   normal     the hook (watchdog-init.c) arms the watchdog as slot-init does and hands PID 1 to the
#              userspace stand-in (watchdog-user.c), which opens /dev/watchdog and feeds it for 45 s.
#              Passes when the stand-in feeds for the whole 45 s and powers off, and the hand-over gap
#              (the hook's last feed, made right before its exec, to the stand-in's first feed) is at
#              most a third of the applied period (HANDOVER_GAP_DIVISOR).
#   stall      the same hook, and the stand-in never opens /dev/watchdog. The watchdog must reset the
#              guest within about 15 s of the hook's last feed. Passes when the reset comes in that window.
#
# The normal and stall modes stand in for lemnosd: no slot is in the guest, so lemnosd never runs here.
# The gap the normal mode checks is the hook's last feed to the stand-in's first feed, not lemnosd's: the
# real gap also holds systemd's start of lemnosd and its bind, which only a board run measures
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
#   CC      a compiler that can link static binaries (default: gcc, as run.sh and run-builtin.sh use)
#   CARGO   the cargo to build lemnos-slot-init for musl with (default: cargo; the group's e-cargo.sh)
#   MODES   the modes to run (default: "slot-init normal stall")
#   LOGS    where the console logs are kept (default: qemu-watchdog under TMPDIR, or /tmp)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
KERNEL="${KERNEL:?set KERNEL to the bzImage of a kernel with CONFIG_I6300ESB_WDT=y}"
CC="${CC:-gcc}"
CARGO="${CARGO:-cargo}"
MODES="${MODES:-slot-init normal stall}"
LOGS="${LOGS:-${TMPDIR:-/tmp}/qemu-watchdog}"
target=x86_64-unknown-linux-musl
DOG_SECONDS=15
TIMING_SLACK=3
# The hand-over bound (docs/slots.md, "The watchdog chain", window 4): the gap from the last feed before the
# exec to the first feed after it must be at most applied / HANDOVER_GAP_DIVISOR, where applied is the period
# the hook reads back. A third: the gap is time nothing feeds, so it must be well under the period; a third
# leaves two thirds of the period for the host's delays.
HANDOVER_GAP_DIVISOR=3

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
read -r -a compiler <<<"$CC"
accel=tcg
if [[ -r /dev/kvm && -w /dev/kvm ]]; then accel=kvm; fi

# The real slot-init, static musl for the initramfs, built the way run.sh builds its musl binaries (the
# rustup target's own linking, with the system cc), and the two stand-ins, static with CC.
(cd "$repo" && "$CARGO" build -q -p lemnos-slot-init --target "$target")
slot_init="${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-slot-init"
flags=(-static -O2 -Wall -Wextra)
"${compiler[@]}" "${flags[@]}" -o "$work/watchdog-init" "$here/watchdog-init.c"
"${compiler[@]}" "${flags[@]}" -o "$work/watchdog-user" "$here/watchdog-user.c"

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
  esac
  passed+=("$mode")
done
echo "qemu-watchdog: passed (${passed[*]}) on $accel"
