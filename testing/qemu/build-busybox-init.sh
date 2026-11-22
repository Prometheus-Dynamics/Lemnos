#!/usr/bin/env bash
# Builds the BusyBox the restart guest boots as PID 1 (testing/qemu/run-busybox-restart.sh): a static BusyBox with the
# init applet and the applets its guest uses (sh, mount, mkdir, sleep, setuidgid). It is built from the release tarball
# that testing/mdev/build-busybox.sh pins and fetches, with the same zig musl build, in its own tree beside the tarball.
#
#   build-busybox-init.sh <directory> <version>
#
# Leaves the binary at <directory>/init-<version>/busybox-<version>/busybox and prints its path. The tarball's sha256 is
# checked against the pin on every run. A finished build is kept; its stamp names the tarball, this script and the zig.
set -euo pipefail

declare -A sha256=(
  [1.37.0]=3311dff32e746499f4df0d5df04d7eb396382d7e108bb9250e7b519b837043a4
  [1.38.0]=34f9ea6ff8636f2c9241153b9114eefa9e65674a45318ae1ef95bb5f31c53bb2
)
out="${1:?usage: build-busybox-init.sh <directory> <version>}"
version="${2:?usage: build-busybox-init.sh <directory> <version>}"
[[ -n "${sha256[$version]:-}" ]] || { echo "build-busybox-init.sh: no pinned release $version" >&2; exit 2; }
jobs="${JOBS:-$(nproc)}"

mkdir -p "$out"
out="$(cd "$out" && pwd)"
tarball="$out/busybox-$version.tar.bz2"
if [[ ! -f "$tarball" ]]; then
  curl -fsSL -o "$tarball.part" "https://busybox.net/downloads/busybox-$version.tar.bz2"
  mv "$tarball.part" "$tarball"
fi
echo "${sha256[$version]}  $tarball" | sha256sum -c - >&2

tree="$out/init-$version"
mkdir -p "$tree"
# One build at a time in a directory.
exec 9>"$tree/.build.lock"
flock 9
src="$tree/busybox-$version"
stamp="$tree/built-$version"
zig_cc="${ZIG_CC:-zig cc}"
want="${sha256[$version]} $(sha256sum "${BASH_SOURCE[0]}" | cut -d' ' -f1) $zig_cc"
if [[ -x "$src/busybox" && -f "$stamp" && "$(cat "$stamp")" == "$want" ]]; then
  echo "$src/busybox"
  exit 0
fi
rm -f "$stamp"
rm -rf "$src"
bzip2 -dc "$tarball" | tar -C "$tree" -xf -

# The configuration: no applets, then init (with its inittab), the shell, and the applets the guest runs.
cd "$src"
make -s allnoconfig >/dev/null
# ASH and its builtins (echo, test, printf, getopts) are the guest shell's; the math is its $((...)).
for option in STATIC INIT FEATURE_USE_INITTAB SH_IS_ASH ASH ASH_ECHO ASH_TEST ASH_PRINTF ASH_GETOPTS FEATURE_SH_MATH FEATURE_SH_MATH_BASE MOUNT MKDIR SLEEP SETUIDGID CAT; do
  if grep -q "^# CONFIG_$option is not set" .config; then
    sed -i "s/^# CONFIG_$option is not set/CONFIG_$option=y/" .config
  else
    echo "CONFIG_$option=y" >> .config
  fi
done
(yes "" || true) | make -s oldconfig >/dev/null
for option in STATIC INIT FEATURE_USE_INITTAB ASH ASH_ECHO ASH_TEST FEATURE_SH_MATH SETUIDGID; do
  grep -qx "CONFIG_$option=y" .config || { echo "build-busybox-init.sh: CONFIG_$option did not take" >&2; exit 1; }
done
# zig's linker refuses the GNU link-map and verbose options that BusyBox's trylink adds (see build-busybox.sh).
if grep -q 'echo "-Wl,--warn-common -Wl,-Map,$EXE.map -Wl,--verbose"' scripts/trylink; then
  sed -i 's/echo "-Wl,--warn-common -Wl,-Map,$EXE.map -Wl,--verbose"/echo ""/' scripts/trylink
fi
make -s -j"$jobs" HOSTCC="${HOSTCC:-gcc}" CC="$zig_cc -target x86_64-linux-musl" \
  EXTRA_CFLAGS="" LDFLAGS="-static" busybox >"$tree/build-$version.log" 2>&1 || {
  tail -n 40 "$tree/build-$version.log" >&2
  exit 1
}
# The applets are checked without running init: BusyBox init is never started on this host (it is PID 1 only in the guest).
# setuidgid answers its own usage when built in; an unbuilt applet answers "applet not found".
probe="$("$src/busybox" setuidgid 2>&1 </dev/null || true)"
[[ "$probe" != *"applet not found"* ]] || { echo "build-busybox-init.sh: setuidgid is not built in" >&2; exit 1; }
grep -aq '/etc/inittab' "$src/busybox" || { echo "build-busybox-init.sh: the init applet is not built in" >&2; exit 1; }
printf '%s\n' "$want" > "$stamp.part"
mv "$stamp.part" "$stamp"
echo "$src/busybox"
