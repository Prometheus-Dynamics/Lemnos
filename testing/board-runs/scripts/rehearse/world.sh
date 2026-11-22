# The stand-in board for rehearsing the detached checks on a PC: sourced by every fake command in
# rehearse/bin. No root, no real unit, no device: CHECK_ROOT is a directory tree, and the world's
# state (units, journal, kernel log, watchdog) lives under $CHECK_ROOT/world.
#
# What it models, and where it fails like the board would:
# - systemd: units with Restart=always and RestartSec=1s (a main process that dies is started again
#   after 1 s, the rest of its cgroup killed first); `start` failing STANDIN fail/<unit> times at
#   the systemctl level (a start job that fails and is not retried by systemd); drop-ins read only
#   after daemon-reload; transient units from systemd-run in their own session.
# - the kernel: a process that closes the watchdog without the magic close gets one ping and the
#   line `watchdog0: watchdog did not stop!`; a killed lemnosd leaves its gadget tree and the kernel
#   empties UDC when ep0 closes.
# - the hardware watchdog: 15 s from the last feed or close ping, then a reset: every process of
#   the world killed (the check and its restores included), runtime drop-ins and mounts gone, a new
#   boot id, and the image's units started again.
set -u
CR=${CHECK_ROOT:?CHECK_ROOT}
W=$CR/world
STANDIN_BIN=$(cd "$(dirname "${BASH_SOURCE[0]}")/bin" && pwd)
UDC_NAME=${STANDIN_UDC:-1000480000.usb}
WD_PERIOD=15

now() { printf '%s' "${EPOCHREALTIME}"; }
wlog() { printf '%s %s\n' "$(now)" "$*" >>"$W/$1.log.tmp" 2>/dev/null; }
calls() { printf '%s %s\n' "$(now)" "$*" >>"$W/calls.log"; }
journal() { # UNIT TEXT
  printf '%s %s\n' "$(now)" "$2" >>"$W/journal/${1%.service}.log"
}
kmsg() { printf '[%s] %s\n' "$(awk '{print $1}' /proc/uptime)" "$1" >>"$W/dmesg.log"; }
unit() { local u=${1%.service}; printf '%s' "${u%.mount}"; }
state() { cat "$W/units/$(unit "$1").state" 2>/dev/null || echo inactive; }
set_state() { printf '%s\n' "$2" >"$W/units/$(unit "$1").state"; }
# is_group ID: true only for a plain number above 1 that is not this shell's own process group: the
# only ids a group kill here may take (never -1, -0 or this shell's group, whatever a pid file held).
is_group() {
  case "$1" in ''|*[!0-9]*) return 1 ;; esac
  [ "$1" -gt 1 ] || return 1
  [ "$1" != "$(sed 's/^.*) //' /proc/$$/stat | cut -d' ' -f3)" ]
}
register() { printf '%s\n' "$1" >>"$W/pgids"; }
feed() { touch "$CR/dev/watchdog"; }
GADGET_DIR=$CR/sys/kernel/config/usb_gadget/lemnos-$UDC_NAME
configfs_on() { grep -q " $CR/sys/kernel/config configfs " "$CR/proc/mounts"; }
add_mount() { grep -qF "$1" "$CR/proc/mounts" || printf '%s\n' "$1" >>"$CR/proc/mounts"; }
del_mount() { grep -vF "$1" "$CR/proc/mounts" >"$CR/proc/mounts.tmp"; mv "$CR/proc/mounts.tmp" "$CR/proc/mounts"; }
FFS_LINE="$UDC_NAME $CR/dev/usb-ffs/$UDC_NAME functionfs rw 0 0"
