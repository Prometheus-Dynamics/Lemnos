#!/bin/sh
# Regenerates the layout assertions of drivers/rknpu/src/uapi.rs from the vendor kernel's NPU
# uAPI header and diffs them.
#
#   ./run.sh <dir holding rknpu_ioctl.h> [out-dir]
#
# Compiled for the host, or with the compiler in CC (aarch64-linux-gnu-gcc for the boards). The
# header needs <drm/drm.h> from the compiler's kernel headers.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
include="$1"
out="${2:-$(mktemp -d)}"
cc="${CC:-cc}"
mkdir -p "$out"
"$cc" -I"$include" -o "$out/check" "$here/check.c"
if [ -n "${RUN:-}" ]; then
    $RUN "$out/check" >"$out/uapi.rs.inc"
else
    "$out/check" >"$out/uapi.rs.inc"
fi
sed -n '/^\/\/ BEGIN GENERATED/,/^\/\/ END GENERATED/p' "$here/../src/uapi.rs" |
    sed '1d;$d' >"$out/current.inc"
if diff -u "$out/current.inc" "$out/uapi.rs.inc"; then
    echo "uapi.rs matches the header"
else
    echo "uapi.rs differs from the header (generated: $out/uapi.rs.inc)"
    exit 1
fi
