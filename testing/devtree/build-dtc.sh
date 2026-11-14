#!/usr/bin/env bash
# Builds the device-tree compiler the overlay gate runs (service/devtree/tests/apply.rs), from the
# release tarball, the way testing/mdev/build-busybox.sh builds BusyBox.
#
#   build-dtc.sh <directory> <version>
#
# <version> is one of the releases pinned below. The tarball is fetched from kernel.org into
# <directory> (once; a cached copy is checked again), checked against the sha256 pinned here (the
# value in the release's sha256sums.asc on kernel.org), and extracted. Leaves the binaries at
# <directory>/dtc-<version>/{dtc,fdtoverlay,fdtget}.
#
# `dtc` is built with the upstream -Werror. `fdtoverlay` and `fdtget` are built with -Wno-error:
# 1.7.2's fdtput does not compile warning-free with a current gcc (discarded qualifiers); the gate
# does not use fdtput, and the warning does not touch the two tools the gate runs.
set -euo pipefail

declare -A sha256=(
  [1.7.2]=92d8ca769805ae1f176204230438fe52808f4e1c7944053c9eec0e649b237539
)
out="${1:?usage: build-dtc.sh <directory> <version>}"
version="${2:?usage: build-dtc.sh <directory> <version>}"
[[ -n "${sha256[$version]:-}" ]] || { echo "build-dtc.sh: no pinned release $version" >&2; exit 2; }
jobs="${JOBS:-$(nproc)}"

mkdir -p "$out"
out="$(cd "$out" && pwd)"
tarball="$out/dtc-$version.tar.xz"
if [[ ! -f "$tarball" ]]; then
  curl -fsSL -o "$tarball.part" "https://www.kernel.org/pub/software/utils/dtc/dtc-$version.tar.xz"
  mv "$tarball.part" "$tarball"
fi
echo "${sha256[$version]}  $tarball" | sha256sum -c - >&2

src="$out/dtc-$version"
# A cached build directory that lost its sources (a build cache that keeps only some files) is not
# a source tree: extract again whenever the Makefile is not there.
if [[ ! -f "$src/Makefile" ]]; then
  rm -rf "$src"
  xz -dc "$tarball" | tar -C "$out" -xf -
fi
cd "$src"
# No YAML and no Python bindings: neither is needed for the compiler or the two tools.
make -s -j"$jobs" NO_YAML=1 NO_PYTHON=1 dtc >"$out/build-$version.log" 2>&1 || { tail -n 40 "$out/build-$version.log" >&2; exit 1; }
make -s -j"$jobs" NO_YAML=1 NO_PYTHON=1 EXTRA_CFLAGS=-Wno-error fdtoverlay fdtget >>"$out/build-$version.log" 2>&1 || { tail -n 40 "$out/build-$version.log" >&2; exit 1; }
probe="$("$src/dtc" --version)"
[[ "$probe" == "Version: DTC $version"* ]]  # dtc appends git describe when built in a git tree || { echo "build-dtc.sh: built $probe, not $version" >&2; exit 1; }
echo "$src"
