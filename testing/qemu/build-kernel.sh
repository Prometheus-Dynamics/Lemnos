#!/usr/bin/env bash
# Builds the kernel the guest boots: a pinned stable release, the default
# configuration plus kernel.config, with lock and memory checking on.
#
#   build-kernel.sh <directory> [--modules-off]
#
# --modules-off builds the kernel for an image with no loadable modules instead: the
# configuration is kernel-modules-off.config, and lemnos is built into the kernel from kernel/
# (drivers/misc/lemnos, CONFIG_LEMNOS=y). Its image is the only thing built, with no modules.
#
# Leaves a built tree at <directory>/build, ready for KDIR, and its image at
# <directory>/build/arch/x86/boot/bzImage, ready for KERNEL.
set -euo pipefail

version=7.3.3
sha256=79c66dde66cd96877fcde8f7d36aec946b1f423d2b97b5a7b1751b75c764f3d1
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="${1:?usage: build-kernel.sh <directory> [--modules-off]}"
mode="${2:-}"
case "$mode" in
  ""|--modules-off) ;;
  *) echo "usage: build-kernel.sh <directory> [--modules-off]" >&2; exit 2 ;;
esac
config="$here/kernel.config"
if [[ "$mode" == --modules-off ]]; then config="$here/kernel-modules-off.config"; fi
jobs="${JOBS:-$(nproc)}"
# CONFIG_RUST needs clang (LLVM=1, and KASAN is only allowed with clang): a Rust kernel is built with
# LLVM=1 and needs ld.lld. A system without ld.lld gets rustc's own gcc-ld/ld.lld, which is the same linker.
llvm=()
if grep -q '^CONFIG_RUST=y' "$config"; then
  llvm=(LLVM=1)
  if ! command -v ld.lld >/dev/null; then
    lld_dir="$(mkdir -p "$out/lld" && cd "$out/lld" && pwd)"
    host="$(rustc -vV | sed -n 's/^host: //p')"
    ln -sf "$(rustc --print sysroot)/lib/rustlib/$host/bin/gcc-ld/ld.lld" "$lld_dir/ld.lld"
    export PATH="$lld_dir:$PATH"
  fi
fi

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
if [[ "$mode" == --modules-off ]]; then
  # The module in tree: a copy of kernel/ under drivers/misc, and the two lines an image builder
  # adds (docs/kernel-config.md). Copied again each time, so a changed kernel/ is what gets built.
  rm -rf "$src/drivers/misc/lemnos"
  cp -r "$here/../../kernel" "$src/drivers/misc/lemnos"
  grep -q 'drivers/misc/lemnos/Kconfig' "$src/drivers/misc/Kconfig" ||
    sed -i '/source "drivers\/misc\/issei\/Kconfig"/a source "drivers/misc/lemnos/Kconfig"' "$src/drivers/misc/Kconfig"
  grep -q 'obj-$(CONFIG_LEMNOS)' "$src/drivers/misc/Makefile" ||
    printf 'obj-$(CONFIG_LEMNOS)\t\t+= lemnos/\n' >>"$src/drivers/misc/Makefile"
fi
make -s -C "$src" O="$out/build" ARCH=x86_64 ${llvm[@]+"${llvm[@]}"} defconfig
"$src/scripts/kconfig/merge_config.sh" -m -O "$out/build" "$out/build/.config" "$config"
# A Rust kernel needs rustc with rust-src, bindgen and clang. Ask the kernel which is missing and print its
# answer, so a missing piece is named here, not left as "did not take" below.
if [[ ${#llvm[@]} -gt 0 ]]; then
  make -C "$src" O="$out/build" ARCH=x86_64 LLVM=1 rustavailable ||
    { echo "$config: CONFIG_RUST needs the Rust kernel toolchain (rustc with rust-src, bindgen, clang): see above" >&2; exit 1; }
fi
make -s -C "$src" O="$out/build" ARCH=x86_64 ${llvm[@]+"${llvm[@]}"} olddefconfig
# merge_config.sh only warns: a line that did not take must fail here.
while read -r line; do
  [[ "$line" == CONFIG_* ]] || continue
  if ! grep -qx "$line" "$out/build/.config"; then
    echo "$config: $line did not take" >&2
    exit 1
  fi
done <"$config"
if [[ "$mode" == --modules-off ]]; then
  grep -qx '# CONFIG_MODULES is not set' "$out/build/.config" || {
    echo "modules-off: CONFIG_MODULES did not take" >&2
    exit 1
  }
  make -s -j"$jobs" -C "$src" O="$out/build" ARCH=x86_64 ${llvm[@]+"${llvm[@]}"} bzImage
else
  make -s -j"$jobs" -C "$src" O="$out/build" ARCH=x86_64 ${llvm[@]+"${llvm[@]}"} bzImage modules
fi
