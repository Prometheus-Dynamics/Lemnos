#!/bin/sh
# Compiles check.c, whose assertions are all static, for riscv64 Linux (the board) and for the
# host. Needs clang (for the riscv64 target; no riscv sysroot is needed, only the compiler's own
# stdbool.h, stddef.h and stdint.h) or CC for the host alone.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
cc="${CC:-clang}"
"$cc" -fsyntax-only -ffreestanding "$here/check.c"
echo "host: layouts match"
if "$cc" --version 2>/dev/null | grep -q clang; then
    "$cc" --target=riscv64-linux-gnu -fsyntax-only -ffreestanding "$here/check.c"
    echo "riscv64: layouts match"
fi
