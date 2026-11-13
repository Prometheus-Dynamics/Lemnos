#!/usr/bin/env bash
# Boots a kernel with no loadable modules and lemnos built in (build-kernel.sh --modules-off),
# once per parameter set, each set on the kernel command line. Each boot must pass the guest's
# checks (testing/qemu/init-builtin.c): the parameters read back as given, the claims work, and
# the kernel reports nothing wrong.
#
#   KERNEL  the bootable image of such a kernel (required: set KERNEL to its bzImage)
#   CC      a compiler that can link static binaries (default: gcc)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
KERNEL="${KERNEL:?set KERNEL to the bzImage of the modules-off kernel}"
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

# One boot per set: each gives the module different parameters, and the guest checks each one
# against the line it was booted with. The last set gives none, so every default is checked.
sets=(
  "lemnos.owned=lemnos,test-block,lemnos,test-spi lemnos.bus_buffer=8192"
  "lemnos.owned=lemnos,test-csi2 lemnos.confined=lemnos,test-csi2 lemnos.unconfined=lemnos,test-vip lemnos.bus_buffer=4096"
  ""
)
for set in "${sets[@]}"; do
  log="$work/console.log"
  status=0
  # shellcheck disable=SC2086 # the words of the set are separate kernel parameters
  timeout 600 qemu-system-x86_64 -accel "$accel" -cpu max -smp 2 -m 768 -nographic -no-reboot \
    -kernel "$KERNEL" -initrd "$work/initramfs" \
    -append "console=ttyS0 panic=-1 oops=panic loglevel=4 $set" \
    | tee "$log" || status=$?
  echo "qemu: boot with [${set:-no parameters}] exited with status $status" >&2
  after="$(sed -n '/lemnos-builtin: start/,$p' "$log")"
  if [[ $status -ne 0 ]]; then
    echo "qemu: exited with status $status" >&2
    exit 1
  fi
  if ! grep -q 'lemnos-builtin: passed' <<<"$after"; then
    echo "qemu: the built-in checks did not pass with [$set]" >&2
    exit 1
  fi
  if grep -E 'BUG:|WARNING:|Oops|possible (circular|recursive) locking|UBSAN|KASAN|Call Trace' <<<"$after" >&2; then
    echo "qemu: the kernel reported a problem with [$set]" >&2
    exit 1
  fi
done
echo "qemu: built-in passed on $accel, ${#sets[@]} parameter sets"
