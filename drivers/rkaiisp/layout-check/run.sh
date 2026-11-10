#!/bin/sh
# Regenerates the layout assertions of drivers/rkaiisp/src/layout.rs from the vendor kernel's
# AI-ISP uAPI headers and diffs them.
#
#   ./run.sh <develop-6.1 include/uapi/linux> [out-dir]
#
# The directory needs rk-aiisp-config.h and rk-isp2-config.h. Compiled for aarch64 when a cross
# compiler is given in CC, else for the host: every structure is packed, so the layout is the same
# on both.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
uapi="$1"
out="${2:-$(mktemp -d)}"
cc="${CC:-cc}"
mkdir -p "$out/linux"
cp "$uapi/rk-aiisp-config.h" "$out/linux/"
sed '/#include <linux\/rk-camera-module.h>/d' "$uapi/rk-isp2-config.h" >"$out/linux/rk-isp2-config.h"
"$cc" -I"$out" -o "$out/check" "$here/check.c"
"$out/check" >"$out/layout.rs.inc"
sed -n '/^\/\/ BEGIN GENERATED/,/^\/\/ END GENERATED/p' "$here/../src/layout.rs" |
    sed '1d;$d' >"$out/current.inc"
if diff -u "$out/current.inc" "$out/layout.rs.inc"; then
    echo "layout.rs matches the headers"
else
    echo "layout.rs differs from the headers (generated: $out/layout.rs.inc)"
    exit 1
fi
