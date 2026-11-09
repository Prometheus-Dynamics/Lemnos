#!/usr/bin/env bash
# Builds the kernel the guest boots: a pinned stable release, the default
# configuration plus kernel.config, with lock and memory checking on.
#
#   build-kernel.sh <directory>
#
# Leaves a built tree at <directory>/build, ready for KDIR, and its image at
# <directory>/build/arch/x86/boot/bzImage, ready for KERNEL.
set -euo pipefail

version=7.3.3
sha256=79c66dde66cd96877fcde8f7d36aec946b1f423d2b97b5a7b1751b75c764f3d1
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="${1:?usage: build-kernel.sh <directory>}"
jobs="${JOBS:-$(nproc)}"

mkdir -p "$out"
out="$(cd "$out" && pwd)"
tarball="$out/linux-$version.tar.xz"
if [[ ! -f "$tarball" ]]; then
  curl -fsSL -o "$tarball.part" "https://cdn.kernel.org/pub/linux/kernel/v${version%%.*}.x/linux-$version.tar.xz"
  mv "$tarball.part" "$tarball"
fi
echo "$sha256  $tarball" | sha256sum -c -

src="$out/linux-$version"
if [[ ! -d "$src" ]]; then
  tar -C "$out" -xf "$tarball"
fi
mkdir -p "$out/build"
make -s -C "$src" O="$out/build" ARCH=x86_64 defconfig
"$src/scripts/kconfig/merge_config.sh" -m -O "$out/build" "$out/build/.config" "$here/kernel.config"
make -s -C "$src" O="$out/build" ARCH=x86_64 olddefconfig
# merge_config.sh only warns: a line that did not take must fail here.
while read -r line; do
  [[ "$line" == CONFIG_* ]] || continue
  if ! grep -qx "$line" "$out/build/.config"; then
    echo "kernel.config: $line did not take" >&2
    exit 1
  fi
done <"$here/kernel.config"
make -s -j"$jobs" -C "$src" O="$out/build" ARCH=x86_64 bzImage modules
