#!/usr/bin/env bash
# Builds lemnos-capi in release and installs both forms under a prefix:
#
#   <prefix>/include/lemnos.h, <prefix>/include/lemnos/abi.h
#   <prefix>/lib/liblemnos_capi.so.<major>, liblemnos_capi.so -> .so.<major>
#   <prefix>/lib/liblemnos_capi.a
#   <prefix>/lib/pkgconfig/lemnos-capi.pc         (shared)
#   <prefix>/lib/pkgconfig/lemnos-capi-static.pc  (static)
#
# Usage: scripts/install-capi.sh <prefix> [--target <triple>]
# Build a program with `pkg-config --cflags --libs lemnos-capi` or
# `lemnos-capi-static`, never both.
set -euo pipefail
prefix=${1:?usage: install-capi.sh <prefix> [--target <triple>]}
shift
target_args=()
target_dir_part=release
if [[ ${1:-} == --target ]]; then
  target_args=(--target "${2:?--target needs a triple}")
  target_dir_part="$2/release"
fi
root=$(cd "$(dirname "$0")/.." && pwd)
crate="$root/crates/lemnos-capi"
header="$crate/include/lemnos.h"
major=$(sed -n 's/^#define LEMNOS_ABI_MAJOR \([0-9]*\)$/\1/p' "$header")
minor=$(sed -n 's/^#define LEMNOS_ABI_MINOR \([0-9]*\)$/\1/p' "$header")
[[ -n $major && -n $minor ]] || { echo "no LEMNOS_ABI_MAJOR/MINOR in $header" >&2; exit 1; }

native=$(cd "$root" && cargo rustc -q -p lemnos-capi --release "${target_args[@]}" \
  --crate-type staticlib --crate-type cdylib -- --print native-static-libs 2>&1 |
  sed -n 's/.*native-static-libs: //p' | head -n 1)
[[ -n $native ]] || { echo "rustc printed no native-static-libs" >&2; exit 1; }
out="${CARGO_TARGET_DIR:-$root/target}/$target_dir_part"

install -d "$prefix/include/lemnos" "$prefix/lib/pkgconfig"
install -m 0644 "$header" "$prefix/include/lemnos.h"
install -m 0644 "$crate/include/lemnos/abi.h" "$prefix/include/lemnos/abi.h"
install -m 0755 "$out/liblemnos_capi.so" "$prefix/lib/liblemnos_capi.so.$major"
ln -sfn "liblemnos_capi.so.$major" "$prefix/lib/liblemnos_capi.so"
install -m 0644 "$out/liblemnos_capi.a" "$prefix/lib/liblemnos_capi.a"
for pc in lemnos-capi lemnos-capi-static; do
  sed -e "s|@PREFIX@|$prefix|" -e "s|@VERSION@|$major.$minor|" -e "s|@MAJOR@|$major|" \
    -e "s|@NATIVE@|$native|" "$crate/pkgconfig/$pc.pc.in" > "$prefix/lib/pkgconfig/$pc.pc"
done
echo "installed lemnos-capi $major.$minor under $prefix"
