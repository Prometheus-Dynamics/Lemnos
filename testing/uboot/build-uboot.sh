#!/usr/bin/env bash
# Builds the U-Boot the gate runs packaging/uboot/boot.cmd under (testing/uboot/check.sh): stock U-Boot v2026.10 for
# the Raspberry Pi 64-bit defconfig, with a configuration fragment and no source change, run by QEMU's raspi3b with
# the disk image as the SD card (testing/uboot/check.py). The tree is the pinned release as published. It also builds
# the Raspberry Pi 3B device tree (rpi3-test.dtb), which the emulated firmware hands U-Boot.
#
#   build-uboot.sh <directory> <version>
#
# <version> is one of the releases pinned below. The release tag's source archive is fetched from the U-Boot
# project's GitLab (source.denx.de, the tag v<version>; it redirects to git.u-boot-project.org) into <directory>
# (once; a cached copy is checked again), checked against the sha256 pinned here, and extracted. Leaves the
# bootloader at <directory>/u-boot-<version>/u-boot.bin, the device tree at <directory>/u-boot-<version>/rpi3-test.dtb, and
# the host mkimage at <directory>/u-boot-<version>/tools/mkimage.
#
# The cross compiler is aarch64-linux-gnu-gcc (CROSS_COMPILE, from the environment: CI installs gcc-aarch64-linux-gnu).
set -euo pipefail

declare -A sha256=(
  [2026.10]=3f856da3cbbc93af51ff95fde18588a348b4a26723b01d9841e999d988481015
)
out="${1:?usage: build-uboot.sh <directory> <version>}"
version="${2:?usage: build-uboot.sh <directory> <version>}"
[[ -n "${sha256[$version]:-}" ]] || { echo "build-uboot.sh: no pinned release $version" >&2; exit 2; }
jobs="${JOBS:-$(nproc)}"
cross="${CROSS_COMPILE:-aarch64-linux-gnu-}"

mkdir -p "$out"
out="$(cd "$out" && pwd)"
tarball="$out/u-boot-v$version.tar.bz2"
if [[ ! -f "$tarball" ]]; then
  curl -fsSL --retry 3 -o "$tarball.part" "https://source.denx.de/u-boot/u-boot/-/archive/v$version/u-boot-v$version.tar.bz2"
  mv "$tarball.part" "$tarball"
fi
echo "${sha256[$version]}  $tarball" | sha256sum -c - >&2

src="$out/u-boot-$version"
if [[ ! -d "$src" ]]; then
  bzip2 -dc "$tarball" | tar -C "$out" -xf -
  mv "$out/u-boot-v$version" "$src"
fi

cd "$src"
# The defconfig is the Raspberry Pi 64-bit one, as published. The fragment below is configuration only.
make -s rpi_arm64_defconfig >/dev/null
set_option() {
  sed -i "/^CONFIG_$1=/d; /^# CONFIG_$1 is not set/d" .config
  if [[ "$2" == n ]]; then echo "# CONFIG_$1 is not set" >> .config; else echo "CONFIG_$1=$2" >> .config; fi
}
# The environment: on the MMC device's uboot-env partition (found by its GPT type), as on the boards. The Pi defconfig
# keeps it on the FAT boot partition (uboot.env), which U-Boot would read before the GPT copy: off here.
set_option ENV_IS_IN_FAT n
set_option ENV_IS_IN_MMC y
set_option ENV_IS_NOWHERE n
set_option ENV_IS_IN_EXT4 n
set_option ENV_IS_IN_SPI_FLASH n
set_option ENV_SIZE 0x2000
# Kconfig needs a value for the fallback offsets when the environment is on MMC. The GPT lookup (env/mmc.c) finds the
# uboot-env partition by its type, and the check's disk puts that partition at sector 18432 (byte 0x900000), so the
# fallback offsets are set to that place and to the second copy after it. They are used only if no such partition exists.
set_option ENV_OFFSET 0x900000
set_option ENV_OFFSET_REDUND 0x902000
set_option ENV_MMC_DEVICE_INDEX 0
set_option PARTITION_TYPE_GUID y
# The commands of packaging/uboot/boot.cmd: sysboot (extlinux), part (named partitions), fdt, source, setexpr, sleep.
set_option CMD_SYSBOOT y
set_option CMD_PART y
set_option CMD_GPT y
set_option CMD_FDT y
set_option CMD_SETEXPR y
set_option CMD_SOURCE y
set_option CMD_SAVEENV y
set_option CMD_SLEEP y
set_option CMD_MMC y
# The host tools (mkimage) are built without OpenSSL: the build hosts have no openssl headers. Legacy script images
# need no crypto; FIT signing and EFI capsule authentication (efitools) are off for the same reason.
set_option TOOLS_LIBCRYPTO n
set_option TOOLS_KWBIMAGE n
set_option TOOLS_MKEFICAPSULE n
set_option FIT_SIGNATURE n
set_option FIT n
set_option EFI_CAPSULE_AUTHENTICATE n
# Non-interactive, and bounded: a prompt here would wait for input forever (no input is given).
timeout 300 make -s olddefconfig </dev/null >/dev/null
for option in ENV_IS_IN_MMC ENV_SIZE PARTITION_TYPE_GUID CMD_SYSBOOT CMD_PART CMD_SAVEENV; do
  grep -q "^CONFIG_$option=" .config || { echo "build-uboot.sh: CONFIG_$option did not take" >&2; exit 1; }
done
grep -qx "# CONFIG_ENV_IS_IN_FAT is not set" .config || { echo "build-uboot.sh: CONFIG_ENV_IS_IN_FAT did not take" >&2; exit 1; }
grep -qx "# CONFIG_TOOLS_LIBCRYPTO is not set" .config || { echo "build-uboot.sh: CONFIG_TOOLS_LIBCRYPTO did not take" >&2; exit 1; }

# No Python tools (binman, dtoc, pylibfdt): they need swig, which the build hosts do not have, and the boot path does
# not use them. The bootloader target is built on its own, so the default target's binman step is not run.
export NO_PYTHON=1
# Exported, so every sub-make compiles with the cross compiler (a top-level variable alone is not seen by the
# recursive builds the prepare step starts).
export CROSS_COMPILE="$cross"
make -s -j"$jobs" CROSS_COMPILE="$cross" u-boot.bin >"$out/build-$version.log" 2>&1 || {
  tail -n 40 "$out/build-$version.log" >&2
  exit 1
}
make -s tools >>"$out/build-$version.log" 2>&1 || { tail -n 40 "$out/build-$version.log" >&2; exit 1; }
[[ -f "$src/u-boot.bin" && -x "$src/tools/mkimage" ]] || { echo "build-uboot.sh: u-boot.bin or mkimage was not built" >&2; exit 1; }

# The device tree the firmware hands U-Boot (OF_BOARD takes it from x0; QEMU's raspi3b gets it with -dtb). The
# Raspberry Pi 3B tree of this release, built by U-Boot's own dtbs target. It has no /aliases mmc0, which the boot
# script needs to name the boot source (it reads the alias from the working tree). A board's U-Boot tree carries
# that alias for the controller U-Boot numbers mmc0; this test tree is given the same alias, for the SDHOST controller
# (mmc@7e202000), which is the controller U-Boot numbers mmc 0 on the emulated machine. This is test data, not a
# change to the source.
make -s -j"$jobs" CROSS_COMPILE="$cross" DEVICE_TREE=bcm2837-rpi-3-b dtbs >>"$out/build-$version.log" 2>&1 || {
  tail -n 40 "$out/build-$version.log" >&2
  exit 1
}
cp "$src/arch/arm/dts/bcm2837-rpi-3-b.dtb" "$src/rpi3-test.dtb"
fdtput -t s "$src/rpi3-test.dtb" /aliases mmc0 /soc/mmc@7e202000
echo "$src/u-boot.bin"
