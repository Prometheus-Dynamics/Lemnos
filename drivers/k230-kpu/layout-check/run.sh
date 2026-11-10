#!/bin/sh
# Compiles layout-check/check.c for riscv64 and for the host; any assertion that fails stops it.
# The numbers it asserts are the ones layout.rs asserts.
#
#   ./run.sh
#
# Needs clang (for the riscv64 target) and the host's <asm-generic/ioctl.h>.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
inc="${KERNEL_HEADERS:-/usr/include}"
clang --target=riscv64-unknown-linux-gnu -ffreestanding -fsyntax-only -isystem "$inc" "$here/check.c"
"${CC:-cc}" -fsyntax-only "$here/check.c"
echo "layout.rs matches the C compiler on riscv64 and the host"
