#!/usr/bin/env bash
# Builds the BusyBox mdev the gate runs the generated mdev.conf with (testing/mdev/check.sh): a
# static BusyBox with the mdev applet and nothing else.
#
#   build-busybox.sh <directory> <version>
#
# <version> is one of the releases pinned below. The release tarball is fetched from busybox.net
# into <directory> (once; a cached copy is checked again), checked against the sha256 pinned here
# (the value of busybox-<version>.tar.bz2.sha256 beside it on busybox.net), and extracted. Leaves the
# binary at <directory>/busybox-<version>/busybox, ready to run.
set -euo pipefail

declare -A sha256=(
  [1.37.0]=3311dff32e746499f4df0d5df04d7eb396382d7e108bb9250e7b519b837043a4
  [1.38.0]=34f9ea6ff8636f2c9241153b9114eefa9e65674a45318ae1ef95bb5f31c53bb2
)
out="${1:?usage: build-busybox.sh <directory> <version>}"
version="${2:?usage: build-busybox.sh <directory> <version>}"
[[ -n "${sha256[$version]:-}" ]] || { echo "build-busybox.sh: no pinned release $version" >&2; exit 2; }
jobs="${JOBS:-$(nproc)}"

mkdir -p "$out"
out="$(cd "$out" && pwd)"
tarball="$out/busybox-$version.tar.bz2"
if [[ ! -f "$tarball" ]]; then
  curl -fsSL -o "$tarball.part" "https://busybox.net/downloads/busybox-$version.tar.bz2"
  mv "$tarball.part" "$tarball"
fi
echo "${sha256[$version]}  $tarball" | sha256sum -c - >&2

src="$out/busybox-$version"
if [[ ! -d "$src" ]]; then
  bzip2 -dc "$tarball" | tar -C "$out" -xf -
fi
# The configuration: no applets, then the mdev applet with its config file, its rename and its
# command options (the forms the generated file uses), built static. `oldconfig` resolves the rest.
cd "$src"
make -s allnoconfig >/dev/null
for option in STATIC MDEV FEATURE_MDEV_CONF FEATURE_MDEV_RENAME FEATURE_MDEV_RENAME_REGEXP FEATURE_MDEV_EXEC; do
  if grep -q "^# CONFIG_$option is not set" .config; then
    sed -i "s/^# CONFIG_$option is not set/CONFIG_$option=y/" .config
  else
    echo "CONFIG_$option=y" >> .config
  fi
done
(yes "" || true) | make -s oldconfig >/dev/null
for option in STATIC MDEV FEATURE_MDEV_CONF; do
  grep -qx "CONFIG_$option=y" .config || { echo "build-busybox.sh: CONFIG_$option did not take" >&2; exit 1; }
done
# zig's linker refuses the GNU link-map and verbose options that BusyBox's trylink adds in INFO_OPTS
# (diagnostic output only). They are blanked in the extracted tree; nothing else in trylink changes.
if grep -q 'echo "-Wl,--warn-common -Wl,-Map,$EXE.map -Wl,--verbose"' scripts/trylink; then
  sed -i 's/echo "-Wl,--warn-common -Wl,-Map,$EXE.map -Wl,--verbose"/echo ""/' scripts/trylink
fi
# A static musl build, as the harness is built (the glibc static library is not installed everywhere).
# ZIG_CC is the zig command (CI runs it as `python3 -m ziglang cc`, from the pinned ziglang wheel).
zig_cc="${ZIG_CC:-zig cc}"
make -s -j"$jobs" HOSTCC="${HOSTCC:-gcc}" CC="$zig_cc -target x86_64-linux-musl" \
  EXTRA_CFLAGS="" LDFLAGS="-static" busybox >"$out/build-$version.log" 2>&1 || {
  tail -n 40 "$out/build-$version.log" >&2
  exit 1
}
# The applet answers to its name: an unknown applet is "applet not found", and mdev's own usage
# message names the option it was given.
probe="$("$src/busybox" mdev -h 2>&1 || true)"
[[ "$probe" == *"unrecognized option"* ]] || { echo "build-busybox.sh: the mdev applet did not run" >&2; exit 1; }
echo "$src/busybox"
