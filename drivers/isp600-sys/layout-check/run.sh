#!/bin/sh
# Regenerates the layout assertions of drivers/isp600-sys/src/layout.rs from the vendor kernel
# source and diffs them.
#
#   ./run.sh <allwinner-bsp checkout> [out-dir]
#
# Reads include/uapi/media/sunxi_camera_v2.h and drivers/vin/vin-isp/isp600/isp600_reg_cfg.h.
# Compiled for the host, which must be 64-bit.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
bsp="$1"
out="${2:-$(mktemp -d)}"
cc="${CC:-cc}"
mkdir -p "$out/linux"
cp "$bsp/include/uapi/media/sunxi_camera_v2.h" "$out/linux/"
"$cc" -I"$out" -I"$bsp/drivers/vin/vin-isp/isp600" -o "$out/check" "$here/check.c"
"$out/check" >"$out/layout.rs.inc"
sed -n '/^\/\/ BEGIN GENERATED/,/^\/\/ END GENERATED/p' "$here/../src/layout.rs" |
    sed '1d;$d' >"$out/current.inc"
if diff -u "$out/current.inc" "$out/layout.rs.inc"; then
    echo "layout.rs matches $bsp"
else
    echo "layout.rs differs from $bsp (generated: $out/layout.rs.inc)"
    exit 1
fi
