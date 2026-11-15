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
#              Passes when the stand-in feeds for the whole 45 s and powers off.
#   stall      the same hook, and the stand-in never opens /dev/watchdog. The watchdog must reset the
#              guest within about 15 s of the hook's last feed. Passes when the reset comes in that window.
#
# The normal and stall modes stand in for lemnosd: no slot is in the guest, so lemnosd never runs here.
# The gap the normal mode prints is the hook's last feed to the stand-in's first open, not lemnosd's.
#
#   KERNEL  the bootable image of a kernel with CONFIG_I6300ESB_WDT=y (required), e.g. the pinned
#           modules-off bzImage (tmp/pinned-2026-11-14-d/k-modules-off/build/arch/x86/boot/bzImage)
#   CC      a compiler that can link static binaries (default: gcc, as run.sh and run-builtin.sh use)
#   CARGO   the cargo to build lemnos-slot-init for musl with (default: cargo; the group's e-cargo.sh)
#   MODES   the modes to run (default: "slot-init normal stall")
#   LOGS    where the console logs are kept (default: a qemu-watchdog directory under the gates tmp directory)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
KERNEL="${KERNEL:?set KERNEL to the bzImage of a kernel with CONFIG_I6300ESB_WDT=y}"
CC="${CC:-gcc}"
CARGO="${CARGO:-cargo}"
MODES="${MODES:-slot-init normal stall}"
LOGS="${LOGS:-/mnt/nvme-wd/lemnos-gates/tmp/qemu-watchdog}"
target=x86_64-unknown-linux-musl

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

# One guest per mode: its /init and its command line, and the stand-in when there is one.
run_mode() {
  local mode="$1" root="$work/root-$1" log="$work/console-$1.log" extra="" status=0 start end
  rm -rf "$root"
  mkdir -p "$root"/{dev,proc,sys,sbin}
  case "$mode" in
    slot-init) cp "$slot_init" "$root/init" ;;
    normal) cp "$work/watchdog-init" "$root/init"; cp "$work/watchdog-user" "$root/sbin/watchdog-user" ;;
    stall) cp "$work/watchdog-init" "$root/init"; cp "$work/watchdog-user" "$root/sbin/watchdog-user"; extra="lemnos.watchdog=stall" ;;
    *) echo "qemu-watchdog: no mode $mode" >&2; exit 2 ;;
  esac
  (cd "$root" && find . -print0 | cpio --quiet --null -o -H newc | gzip -1) >"$work/initramfs-$mode"
  start="$(date +%s)"
  # -no-reboot: a restart or a watchdog reset ends QEMU, so the run's end is the guest's end.
  # The console is stamped with the time each line arrives, so the stall can be timed.
  timeout 180 qemu-system-x86_64 -accel "$accel" -cpu max -smp 2 -m 512 -nographic -no-reboot \
    -device i6300esb -watchdog-action reset \
    -kernel "$KERNEL" -initrd "$work/initramfs-$mode" \
    -append "console=ttyS0 panic=-1 oops=panic loglevel=6 printk.time=1 $extra" \
    2>&1 | while IFS= read -r line; do
      printf '%s %s\n' "$(date +%s.%N)" "$line" | tee -a "$log" >/dev/null
      printf '%s\n' "$line"
    done >"$work/raw-$mode.log" || status=$?
  end="$(date +%s.%N)"
  echo "qemu-watchdog: $mode ended after $(awk -v a="$start" -v b="$end" 'BEGIN{printf "%.1f", b-a}') s, status $status" >&2
  # The stamped console, without the stamps, for the checks; and the stamp of the stall's line.
  sed 's/^[0-9.]* //' "$log" >"$work/text-$mode.log"
  # The console logs are kept (LOGS, default a scratch directory) and not only in this run's temporary directory.
  mkdir -p "$LOGS" && cp "$log" "$LOGS/console-$mode.stamped.log" && cp "$work/text-$mode.log" "$LOGS/console-$mode.log"
  if [[ "$mode" == stall ]]; then
    stalled="$(awk '/user: stalled/ {print $1; exit}' "$log")"
    [[ -n "$stalled" ]] || fail "the stall line is not in the console"
    echo "qemu-watchdog: stall: the guest ended $(awk -v a="$stalled" -v b="$end" 'BEGIN{printf "%.1f", b-a}') s after the stall line (watchdog timeout 15 s)" >&2
  fi
  echo "$work/text-$mode.log"
}

passed=()
for mode in $MODES; do
  text="$(run_mode "$mode")"
  case "$mode" in
    slot-init)
      grep -q 'slot-init: watchdog armed: 15 s' "$text" || fail "slot-init did not arm the watchdog at 15 s"
      grep -q 'not a Lemnos disk: no flashing mode is asked' "$text" || fail "the refusal did not take the non-Lemnos path"
      grep -q 'restarting in 30 s' "$text" || fail "the rescue wait and restart did not happen"
      grep -qi 'watchdog.*reset\|Watchdog timer' "$text" && fail "the guest was reset by the watchdog during the rescue wait"
      ;;
    normal)
      grep -q 'user: first open of /dev/watchdog' "$text" || fail "the stand-in never opened the watchdog"
      grep -q 'fed the watchdog for 45 s and the guest stayed up' "$text" || fail "the guest did not stay up for 45 s"
      ;;
    stall)
      grep -q 'user: stalled after the handover' "$text" || fail "the stall did not start"
      ;;
  esac
  passed+=("$mode")
done
echo "qemu-watchdog: passed (${passed[*]}) on $accel"
