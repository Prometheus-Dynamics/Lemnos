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
cp -r "$here/mock_rp1" "$work/mock_rp1"
make -s -j"$JOBS" -C "$KDIR" M="$work/test_block" modules
# The real PIO driver only exists on one family of boards, so this guest gets a stand-in for it.
make -s -j"$JOBS" -C "$KDIR" M="$work/mock_rp1" LEMNOS_MOCK_RP1=1 modules
make -s -j"$JOBS" -C "$KDIR" M="$work/kernel" CONFIG_RP1_PIO=m \
  LEMNOS_EXTRA_CFLAGS="-I$work/mock_rp1" KBUILD_EXTRA_SYMBOLS="$work/mock_rp1/Module.symvers" modules

root="$work/root"
mkdir -p "$root"/{dev,proc,sys}
flags=(-static -O2 -Wall -Wextra -Wno-unused-parameter -pthread -I"$work/test_block")
read -r -a compiler <<<"$CC"
"${compiler[@]}" "${flags[@]}" -o "$root/init" "$here/init.c"
for check in lemnos_check lemnos_irq_check lemnos_unbind_check lemnos_claim_check lemnos_bus_check lemnos_gpio_check lemnos_power_check lemnos_pio_check lemnos_stream_check lemnos_rp1_csi2_check lemnos_gsdma_check lemnos_vip_check lemnos_board_check lemnos_regjob_check; do
  "${compiler[@]}" "${flags[@]}" -o "$root/$check" "$work/kernel/test/$check.c"
done
# lemnos_board_check is built so it keeps compiling; it runs only on a board (C6).
# Its uAPI header is the kernel's own.
source="$KDIR/source/include/uapi/linux/media/raspberrypi"
[[ -d "$source" ]] || source="$KDIR/include/uapi/linux/media/raspberrypi"
"${compiler[@]}" "${flags[@]}" -I"$source" -pthread -o "$root/lemnos_pisp_be_check" \
  "$work/kernel/test/lemnos_pisp_be_check.c"
"${compiler[@]}" "${flags[@]}" -I"$source" -o "$root/lemnos_pisp_fe_check" \
  "$work/kernel/test/lemnos_pisp_fe_check.c"
# The real backend, static so the image needs no libraries.
target=x86_64-unknown-linux-musl
(cd "$repo" && cargo build -q -j"$JOBS" -p lemnos-module-check --target "$target")
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-module-check" "$root/"
cp "$work/kernel/lemnos.ko" "$work"/test_block/lemnos_test_*.ko "$work/mock_rp1/rp1_pio_mock.ko" "$root/"
# The kernel's own test modules: from the built tree, or, for this machine's kernel, installed.
# gpio-sim and spidev may be built in instead, and then there's no file to bring.
# Installed modules only fit this machine's kernel, so they're only looked at when that's what boots.
places=("$KDIR/drivers")
if [[ "$KERNEL" == "/boot/vmlinuz-$release" ]]; then places+=("/lib/modules/$release/kernel/drivers"); fi
for module in i2c-stub spi-loopback-test gpio-sim spidev; do
  found="$(find -L "${places[@]}" -name "$module.ko*" 2>/dev/null | head -1 || true)"
  case "$found" in
    "")
      [[ "$module" == gpio-sim || "$module" == spidev ]] && continue
      echo "qemu: $module not found under $KDIR" >&2
      exit 1
      ;;
    *.zst) zstd -q -d -o "$root/$module.ko" "$found" ;;
    *.xz) xz -d -c "$found" >"$root/$module.ko" ;;
    *) cp "$found" "$root/$module.ko" ;;
  esac
done
(cd "$root" && find . -print0 | cpio --quiet --null -o -H newc | gzip -1) >"$work/initramfs"

accel=tcg
if [[ -r /dev/kvm && -w /dev/kvm ]]; then accel=kvm; fi
log="$work/console.log"
status=0
timeout 600 qemu-system-x86_64 -accel "$accel" -cpu max -smp 2 -m 768 -nographic -no-reboot \
  -kernel "$KERNEL" -initrd "$work/initramfs" \
  -append "console=ttyS0 panic=-1 oops=panic cma=64M loglevel=4 memmap=64K\$0x20000000" | tee "$log" || status=$?

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
