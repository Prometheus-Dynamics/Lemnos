#!/usr/bin/env bash
# build-kernel.sh <linux source tree> <build dir>: the first-flash run's arm64 guest kernel (kernel.config merged onto
# allnoconfig), built out of tree into <build dir>; the source tree is only read. The Image is
# <build dir>/arch/arm64/boot/Image, which run.sh takes as KERNEL. Needs aarch64-linux-gnu-gcc.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
src=$1; out=$2
mkdir -p "$out"
m=(make -s -C "$src" O="$out" ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-)
"${m[@]}" allnoconfig
"$src/scripts/kconfig/merge_config.sh" -m -O "$out" "$out/.config" "$here/kernel.config" >/dev/null
"${m[@]}" olddefconfig
bad=0
while read -r line; do
  [[ $line == CONFIG_* ]] || continue
  grep -qx "$line" "$out/.config" || { echo "build-kernel: did not take: $line" >&2; bad=1; }
done < "$here/kernel.config"
[[ $bad == 0 ]] || exit 1
"${m[@]}" -j"${JOBS:-4}" Image
ls -l "$out/arch/arm64/boot/Image"
