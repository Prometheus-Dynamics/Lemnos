#!/bin/sh
# Regenerates the layout assertions of drivers/rkisp-sys/src/layout.rs from the vendor kernel's ISP
# uAPI headers (the same two directories bindgen.sh takes) and diffs them.
#
#   ./run.sh <develop-5.10 include/uapi/linux> <develop-6.1 include/uapi/linux> [out-dir]
#
# Compiled for aarch64 when a cross compiler is given in CC, else for the host: every structure
# is packed, so the layout is the same on both.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
h510="$1"
h61="$2"
out="${3:-$(mktemp -d)}"
cc="${CC:-cc}"
mkdir -p "$out/510/linux" "$out/61/linux"
for h in rk-isp2-config.h rk-isp21-config.h rk-isp3-config.h rk-isp32-config.h; do
    sed '/#include <linux\/rk-camera-module.h>/d' "$h510/$h" >"$out/510/linux/$h"
done
cp "$out"/510/linux/*.h "$out/61/linux/"
for h in rk-isp2-config.h rk-isp33-config.h rk-isp35-config.h rk-isp39-config.h; do
    sed '/#include <linux\/rk-camera-module.h>/d' "$h61/$h" >"$out/61/linux/$h"
done
"$cc" -DV510 -I"$out/510" -o "$out/v510" "$here/check.c"
"$cc" -DV61 -I"$out/61" -o "$out/v61" "$here/check.c"
{ "$out/v510"; "$out/v61"; } >"$out/layout.rs.inc"
sed -n '/^\/\/ BEGIN GENERATED/,/^\/\/ END GENERATED/p' "$here/../src/layout.rs" |
    sed '1d;$d' >"$out/current.inc"
if diff -u "$out/current.inc" "$out/layout.rs.inc"; then
    echo "layout.rs matches the headers"
else
    echo "layout.rs differs from the headers (generated: $out/layout.rs.inc)"
    exit 1
fi
