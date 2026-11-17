#!/usr/bin/env bash
# The gate's check of packaging/uboot/boot.cmd by U-Boot itself (testing/uboot/check.py): the boot-count ladder (a normal
# trial, failed trials, the previous slot from the count of 4, rescue at 8), the default and fallback cases, and the
# environment-write guards. Stock U-Boot v2026.10 for the Raspberry Pi 64-bit defconfig (testing/uboot/build-uboot.sh,
# pinned release, one configuration fragment, no source change) runs under QEMU's raspi3b, with the disk image as the
# SD card and the Raspberry Pi 3B device tree from the same build.
#
#   check.sh                      (UBOOT_DIR: where the build goes; default target/uboot)
#   check.sh --mutant NAME        (fail-before: one mutation of boot.cmd must make the named case fail)
#
# The bootloader, the device tree and mkimage are built once and cached in UBOOT_DIR; CI caches that directory on the script.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
target="${CARGO_TARGET_DIR:-$repo/target}"
uboot_dir="${UBOOT_DIR:-$target/uboot}"
version=2026.10
mkdir -p "$uboot_dir"
tree="$uboot_dir/u-boot-$version"
if [[ ! -f "$tree/u-boot.bin" || ! -f "$tree/rpi3-test.dtb" || ! -x "$tree/tools/mkimage" ]]; then
  "$here/build-uboot.sh" "$uboot_dir" "$version" >/dev/null
fi
exec python3 "$here/check.py" --uboot "$tree/u-boot.bin" --mkimage "$tree/tools/mkimage" \
  --dtb "$tree/rpi3-test.dtb" --qemu qemu-system-aarch64 --work "$uboot_dir/work" "$@"
