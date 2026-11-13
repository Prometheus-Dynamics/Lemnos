#!/usr/bin/env bash
# Boots a kernel with lemnos built in (CONFIG_LEMNOS=y, no lemnos.ko in the image) and checks
# that the module takes its parameters from the kernel command line and passes the claim checks.
#
#   KERNEL  the bootable image of such a kernel (required)
#   CC      a compiler that can link static binaries (default: gcc)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
KERNEL="${KERNEL:?set KERNEL to the bzImage of the built-in kernel}"
CC="${CC:-gcc}"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

root="$work/root"
mkdir -p "$root"/{dev,proc,sys}
flags=(-static -O2 -Wall -Wextra -Wno-unused-parameter)
read -r -a compiler <<<"$CC"
"${compiler[@]}" "${flags[@]}" -o "$root/init" "$here/init-builtin.c"
"${compiler[@]}" "${flags[@]}" -o "$root/lemnos_claim_check" "$repo/kernel/test/lemnos_claim_check.c"
(cd "$root" && find . -print0 | cpio --quiet --null -o -H newc | gzip -1) >"$work/initramfs"

accel=tcg
if [[ -r /dev/kvm && -w /dev/kvm ]]; then accel=kvm; fi
log="$work/console.log"
status=0
# The module's parameters on the command line, as a built-in module takes them.
timeout 600 qemu-system-x86_64 -accel "$accel" -cpu max -smp 2 -m 768 -nographic -no-reboot \
  -kernel "$KERNEL" -initrd "$work/initramfs" \
  -append "console=ttyS0 panic=-1 oops=panic loglevel=4 lemnos.owned=lemnos,test-block,lemnos,test-spi lemnos.bus_buffer=8192" \
  | tee "$log" || status=$?
after="$(sed -n '/lemnos-builtin: start/,$p' "$log")"
if [[ $status -ne 0 ]]; then
  echo "qemu: exited with status $status" >&2
  exit 1
fi
if ! grep -q 'lemnos-builtin: passed' <<<"$after"; then
  echo "qemu: the built-in checks did not pass" >&2
  exit 1
fi
if grep -E 'BUG:|WARNING:|Oops|possible (circular|recursive) locking|UBSAN|KASAN|Call Trace' <<<"$after" >&2; then
  echo "qemu: the kernel reported a problem during the checks" >&2
  exit 1
fi
echo "qemu: built-in passed on $accel"
