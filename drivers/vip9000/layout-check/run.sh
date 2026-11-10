#!/bin/sh
# Regenerates the layout assertions of drivers/vip9000/src/layout.rs from the vendor kernel's
# vipcore headers and diffs them.
#
#   ./run.sh <drivers/npu/aw_nna_vip/vip2 dir> [out-dir]
#
# The directory needs inc/ (vip_drv_share.h, vip_lite_config.h, vip_lite_common.h) and
# os/linux/vip_drv_os.h. The defines are the ones the module's Kbuild passes for the platform
# build, with the target chip's configuration selected (`IS_ENABLED` stands in for the kernel's).
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
vip="$1"
out="${2:-$(mktemp -d)}"
cc="${CC:-cc}"
mkdir -p "$out"
"$cc" -DLINUX -DDRIVER -DUSE_LINUX_PLATFORM_DEVICE -DvpmdAUTO_CORRECT_CONFLICTS=1 \
    -DNPU_USER_IOMMU=0 -DDBG=0 '-DIS_ENABLED(x)=x' -DCONFIG_ARCH_SUN60IW2=1 \
    -I"$vip/inc" -I"$vip/os/linux" -o "$out/check" "$here/check.c"
"$out/check" >"$out/layout.rs.inc"
sed -n '/^\/\/ BEGIN GENERATED/,/^\/\/ END GENERATED/p' "$here/../src/layout.rs" |
    sed '1d;$d' >"$out/current.inc"
if diff -u "$out/current.inc" "$out/layout.rs.inc"; then
    echo "layout.rs matches the headers"
else
    echo "layout.rs differs from the headers (generated: $out/layout.rs.inc)"
    exit 1
fi
