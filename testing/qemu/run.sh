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

# A Rust kernel is built with clang, so every module built against it is too (LLVM=1).
llvm=()
if grep -q '^CONFIG_RUST=y' "$KDIR/.config" 2>/dev/null; then llvm=(LLVM=1); fi

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

# Copies, so the build leaves nothing in the source tree.
cp -r "$repo/kernel" "$work/kernel"
cp -r "$here/test_block" "$work/test_block"
cp -r "$here/mock_rp1" "$work/mock_rp1"
make -s -j"$JOBS" -C "$KDIR" ${llvm[@]+"${llvm[@]}"} M="$work/test_block" modules
# The real PIO driver only exists on one family of boards, so this guest gets a stand-in for it.
make -s -j"$JOBS" -C "$KDIR" ${llvm[@]+"${llvm[@]}"} M="$work/mock_rp1" LEMNOS_MOCK_RP1=1 modules
make -s -j"$JOBS" -C "$KDIR" ${llvm[@]+"${llvm[@]}"} M="$work/kernel" CONFIG_RP1_PIO=m \
  LEMNOS_EXTRA_CFLAGS="-I$work/mock_rp1" KBUILD_EXTRA_SYMBOLS="$work/mock_rp1/Module.symvers" modules
# The Rust half's skeleton (kernel/rust): built beside the C module, loaded by lemnos-init.
make -s -j"$JOBS" -C "$KDIR" ${llvm[@]+"${llvm[@]}"} M="$work/kernel/rust" modules

root="$work/root"
mkdir -p "$root"/{dev,proc,sys}
flags=(-static -O2 -Wall -Wextra -Wno-unused-parameter -pthread -I"$work/test_block")
read -r -a compiler <<<"$CC"
for check in lemnos_rp1_csi2_check; do
  "${compiler[@]}" "${flags[@]}" -o "$root/$check" "$work/kernel/test/$check.c"
done
# lemnos_board_check is Rust now (CZ-GUEST-2): built by cargo above so it keeps compiling; it runs only on a board.
# Its uAPI header is the kernel's own.
source="$KDIR/source/include/uapi/linux/media/raspberrypi"
[[ -d "$source" ]] || source="$KDIR/include/uapi/linux/media/raspberrypi"
# The real backend, static so the image needs no libraries.
target=x86_64-unknown-linux-musl
# The bounce trace of the link check (its comm core's log and each read and write on the link) is off
# unless asked for: LINK_CHECK_TRACE=1 builds it in. Left out, the check runs as it does in CI.
link_check_features=()
if [[ "${LINK_CHECK_TRACE:-0}" == 1 ]]; then
  link_check_features=(--features lemnos-orion-link-check/trace)
fi
(cd "$repo" && cargo build -q -j"$JOBS" -p lemnos-module-check -p lemnos-gadget-check -p lemnosd -p lemnos-stream-compose-check -p lemnos-shm-ring-check -p lemnos-shm-holds-check -p lemnos-shm-bell-check -p lemnos-nand-check -p lemnos-orion-link-check -p lemnos-orion -p lemnos-claim-check -p lemnos-unbind-check -p lemnos-gpio-check -p lemnos-power-check -p lemnos-irq-check -p lemnos-bus-check -p lemnos-board-check -p lemnos-check-main -p lemnos-regjob-check -p lemnos-regjob-isp-check -p lemnos-pio-check -p lemnos-lend-check -p lemnos-pisp-be-check -p lemnos-pisp-be-version-check -p lemnos-pisp-fe-check -p lemnos-stream-check -p lemnos-gsdma-check -p lemnos-vip-check -p lemnos-init --target "$target" ${link_check_features[@]+"${link_check_features[@]}"})
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-module-check" "$root/"
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-gadget-check" "$root/"
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnosd" "$root/"
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-stream-compose-check" "$root/"
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-shm-ring-check" "$root/"
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-shm-holds-check" "$root/"
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-shm-bell-check" "$root/"
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-nand-check" "$root/"
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-orion-link-check" "$root/"
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-orion" "$root/"
# The claim check is Rust (CZ-GUEST); init runs it as /lemnos_claim_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-claim-check" "$root/lemnos_claim_check"
# The unbind check is Rust (CZ-GUEST slice 2); init runs it as /lemnos_unbind_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-unbind-check" "$root/lemnos_unbind_check"
# The GPIO check is Rust (CZ-GUEST-2); init runs it as /lemnos_gpio_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-gpio-check" "$root/lemnos_gpio_check"
# The power check is Rust (CZ-GUEST-2); init runs it as /lemnos_power_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-power-check" "$root/lemnos_power_check"
# The irq check is Rust (CZ-GUEST-2); init runs it as /lemnos_irq_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-irq-check" "$root/lemnos_irq_check"
# The bus check is Rust (CZ-GUEST-2); init runs it as /lemnos_bus_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-bus-check" "$root/lemnos_bus_check"
# The board check is Rust (CZ-GUEST-2). It is built so it keeps compiling; it runs only on a board.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-board-check" "$root/lemnos_board_check"
# The block check is Rust (CZ-GUEST-2); init runs it as /lemnos_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-check-main" "$root/lemnos_check"
# The register-job check is Rust (CZ-GUEST-2); init runs it as /lemnos_regjob_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-regjob-check" "$root/lemnos_regjob_check"
# The register-job check on the ISP is Rust (CZ-GUEST-2); init runs it as /lemnos_regjob_isp_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-regjob-isp-check" "$root/lemnos_regjob_isp_check"
# The PIO check is Rust (CZ-GUEST-2); init runs it as /lemnos_pio_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-pio-check" "$root/lemnos_pio_check"
# The dma-buf lend check is Rust (CZ-GUEST-2); init runs it as /lemnos_lend_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-lend-check" "$root/lemnos_lend_check"
# The PiSP back-end check is Rust (CZ-GUEST-3); init runs it as /lemnos_pisp_be_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-pisp-be-check" "$root/lemnos_pisp_be_check"
# The PiSP back-end version check is Rust (CZ-GUEST-3); init runs it as /lemnos_pisp_be_version_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-pisp-be-version-check" "$root/lemnos_pisp_be_version_check"
# The PiSP front-end check is Rust (CZ-GUEST-3); init runs it as /lemnos_pisp_fe_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-pisp-fe-check" "$root/lemnos_pisp_fe_check"
# The frame stream check is Rust (CZ-GUEST-4); init runs it as /lemnos_stream_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-stream-check" "$root/lemnos_stream_check"
# The graphics DMA check is Rust (CZ-GUEST-4); init runs it as /lemnos_gsdma_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-gsdma-check" "$root/lemnos_gsdma_check"
# The VIP9000 check is Rust (CZ-GUEST-4); init runs it as /lemnos_vip_check.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-vip-check" "$root/lemnos_vip_check"
# The first process is Rust (CZ-GUEST-3): it runs as the image's init.
cp "${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-init" "$root/init"
cp "$work/kernel/lemnos.ko" "$work"/test_block/lemnos_test_*.ko "$work/mock_rp1/rp1_pio_mock.ko" "$work/kernel/rust/lemnos_rs.ko" "$root/"
# The kernel's own test modules: from the built tree, or, for this machine's kernel, installed.
# gpio-sim and spidev may be built in instead, and then there's no file to bring.
# Installed modules only fit this machine's kernel, so they're only looked at when that's what boots.
places=("$KDIR/drivers")
if [[ "$KERNEL" == "/boot/vmlinuz-$release" ]]; then places+=("/lib/modules/$release/kernel/drivers"); fi
for module in i2c-stub gpio-sim spidev; do
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
# The NAND simulator is a module (testing/qemu/kernel.config): init loads it with its geometry.
nandsim="$(find -L "$KDIR/drivers" -name 'nandsim.ko*' 2>/dev/null | head -1 || true)"
if [[ -z "$nandsim" ]]; then echo "qemu: nandsim not found under $KDIR" >&2; exit 1; fi
case "$nandsim" in
  *.zst) zstd -q -d -o "$root/nandsim.ko" "$nandsim" ;;
  *.xz) xz -d -c "$nandsim" >"$root/nandsim.ko" ;;
  *) cp "$nandsim" "$root/nandsim.ko" ;;
esac
# The Rust checks (CZ-GUEST-2) are static binaries with debug info, about 5 MB each unstripped. No check reads
# their symbols or line tables (they compare output lines), so they are stripped here to keep the initramfs
# within the guest's memory. The workspace's debug profile is unchanged.
for guest in init lemnos_pisp_be_version_check lemnos_claim_check lemnos_unbind_check lemnos_gpio_check lemnos_power_check lemnos_irq_check lemnos_bus_check lemnos_board_check lemnos_check lemnos_regjob_check lemnos_regjob_isp_check lemnos_pio_check lemnos_lend_check lemnos_pisp_be_check lemnos_pisp_fe_check lemnos_stream_check lemnos_gsdma_check lemnos_vip_check; do
  strip "$root/$guest"
done
(cd "$root" && find . -print0 | cpio --quiet --null -o -H newc | gzip -1) >"$work/initramfs"

accel=tcg
if [[ -r /dev/kvm && -w /dev/kvm ]]; then accel=kvm; fi
log="$work/console.log"
status=0
timeout 600 qemu-system-x86_64 -accel "$accel" -cpu max -smp 2 -m 768 -nographic -no-reboot \
  -kernel "$KERNEL" -initrd "$work/initramfs" \
  -append "console=ttyS0 panic=-1 oops=panic cma=64M loglevel=4 memmap=64K\$0x20000000 lemnos.serial=0123456789abcdef" | tee "$log" || status=$?

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
# The kernel's UBI says so on any error it finds (the first-flash check reads what its UBI attaches).
if grep -E 'BUG:|WARNING:|Oops|possible (circular|recursive) locking|UBSAN|KASAN|Call Trace|UBI error|bad VID header|corrupted' <<<"$after" >&2; then
  echo "qemu: the kernel reported a problem during the checks" >&2
  exit 1
fi
echo "qemu: passed on $accel"
