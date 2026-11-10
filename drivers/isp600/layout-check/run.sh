#!/bin/sh
# Regenerates the layout assertions of drivers/isp600/src/layout.rs (between its BEGIN and END GENERATED lines) from
# the vendor kernel source and diffs them.
#
#   ./run.sh <allwinner-bsp checkout> [out-dir]
#
# Reads drivers/vin/vin-isp/isp600/isp600_reg.h and isp600_reg_cfg.h.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
bsp="$1"
out="${2:-$(mktemp -d)}"
cc="${CC:-cc}"
mkdir -p "$out"
"$cc" -I"$bsp/drivers/vin/vin-isp/isp600" -o "$out/check" "$here/check.c"
"$out/check" >"$out/layout.rs.inc"
sed -n '/^\/\/ BEGIN GENERATED/,/^\/\/ END GENERATED/p' "$here/../src/layout.rs" |
    sed '1d;$d' >"$out/current.inc"
if diff -u "$out/current.inc" "$out/layout.rs.inc"; then
    echo "layout.rs matches $bsp"
else
    echo "layout.rs differs from $bsp (generated: $out/layout.rs.inc)"
    exit 1
fi
