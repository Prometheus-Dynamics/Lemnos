#!/usr/bin/env bash
# Runs the CPU route's tests (`lemnos-drivers-rga`'s `cpu` module) as riscv64 binaries under
# qemu's user mode: with the V extension at VLEN 128 (the K230's C908), 256 and 512, where the
# RVV rows of `lemnos_linux_sys::rvv` must run and match the scalar loops, and with V off, where
# the scalar loops must run. Needs `qemu-riscv64-static` (or QEMU=...) and the
# riscv64gc-unknown-linux-musl target; links with rust-lld, no C cross toolchain.
#
#   scripts/rvv-qemu.sh
set -euo pipefail

QEMU=${QEMU:-qemu-riscv64-static}
command -v "$QEMU" >/dev/null || { echo "rvv-qemu: $QEMU not found" >&2; exit 1; }
root_dir=$(cd "$(dirname "$0")/.." && pwd)
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root_dir/target}/rvv-qemu
export CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
export CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes"
cd "$root_dir"
for cpu in "v=true,vlen=128 1" "v=true,vlen=256 1" "v=true,vlen=512 1" "v=false 0"; do
  set -- $cpu
  echo "==> rv64,$1"
  LEMNOS_EXPECT_VECTOR=$2 CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_MUSL_RUNNER="$QEMU -cpu rv64,$1" \
    cargo test -q -p lemnos-drivers-rga --lib --target riscv64gc-unknown-linux-musl cpu::
done
