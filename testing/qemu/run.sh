#!/usr/bin/env bash
# Builds the module and its checks against a kernel, boots that kernel in QEMU
# with them as the whole image, and fails unless every check passed and the
# kernel reported nothing wrong while they ran.
#
#   KDIR    a built kernel tree (default: this machine's headers)
#   KERNEL  its bootable image (default: this machine's kernel)
#   CC      a compiler that can link static binaries (default: gcc)
#   JOBS    parallel jobs for the module build (default: 2)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
release="$(uname -r)"
KDIR="${KDIR:-/lib/modules/$release/build}"
KERNEL="${KERNEL:-/boot/vmlinuz-$release}"
CC="${CC:-gcc}"
JOBS="${JOBS:-2}"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

# Copies, so the build leaves nothing in the source tree.
cp -r "$repo/kernel" "$work/kernel"
cp -r "$here/test_block" "$work/test_block"
make -s -j"$JOBS" -C "$KDIR" M="$work/kernel" modules
make -s -j"$JOBS" -C "$KDIR" M="$work/test_block" modules

root="$work/root"
mkdir -p "$root"/{dev,proc,sys}
flags=(-static -O2 -Wall -Wextra -Wno-unused-parameter)
read -r -a compiler <<<"$CC"
"${compiler[@]}" "${flags[@]}" -o "$root/init" "$here/init.c"
for check in lemnos_check lemnos_irq_check lemnos_unbind_check lemnos_claim_check lemnos_stream_check; do
  "${compiler[@]}" "${flags[@]}" -o "$root/$check" "$work/kernel/test/$check.c"
done
cp "$work/kernel/lemnos.ko" "$work/test_block/lemnos_test_block.ko" "$root/"
(cd "$root" && find . -print0 | cpio --quiet --null -o -H newc | gzip -1) >"$work/initramfs"

accel=tcg
if [[ -r /dev/kvm && -w /dev/kvm ]]; then accel=kvm; fi
log="$work/console.log"
status=0
timeout 600 qemu-system-x86_64 -accel "$accel" -cpu max -smp 2 -m 768 -nographic -no-reboot \
  -kernel "$KERNEL" -initrd "$work/initramfs" \
  -append "console=ttyS0 panic=-1 oops=panic cma=64M loglevel=4" | tee "$log" || status=$?

# Only what happened after the guest started the checks counts.
after="$(sed -n '/lemnos-qemu: start/,$p' "$log")"
if [[ $status -ne 0 ]]; then
  echo "qemu: exited with status $status" >&2
  exit 1
fi
if ! grep -q 'lemnos-qemu: passed' <<<"$after"; then
  echo "qemu: the checks did not pass" >&2
  exit 1
fi
# dev_warn from the module's test parameters is expected, and says "test_".
if grep -E 'BUG:|WARNING:|Oops|possible (circular|recursive) locking|UBSAN|KASAN|Call Trace' <<<"$after" >&2; then
  echo "qemu: the kernel reported a problem during the checks" >&2
  exit 1
fi
echo "qemu: passed on $accel"
