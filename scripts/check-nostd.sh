#!/usr/bin/env bash
# Builds and lints the no_std crates for bare-metal and wasm targets.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

targets=(${NOSTD_TARGETS:-thumbv7em-none-eabihf riscv32imac-unknown-none-elf wasm32-unknown-unknown})
# crate:features (empty features: none beyond --no-default-features)
crates=(${NOSTD_CRATES:-lemnos-hal: lemnos-hal:mock lemnos-device: lemnos-device:float lemnos-device:alloc,serde lemnos-lite: lemnos-light: lemnos-drivers-ws2812: lemnos-core: lemnos-core:serde lemnos-driver-manifest: lemnos-driver-manifest:serde lemnos-drivers-vcm: lemnos-drivers-ina2xx: lemnos-drivers-ina2xx:float lemnos-drivers-bmm150: lemnos-drivers-bmm150:float lemnos-drivers-bmi088: lemnos-drivers-bmi088:float})

for target in "${targets[@]}"; do
  rustup target add "$target" >/dev/null 2>&1 || true
  for entry in "${crates[@]}"; do
    crate="${entry%%:*}"
    features="${entry#*:}"
    echo "==> [nostd] $crate (${features:-no features}) for $target"
    cargo clippy -p "$crate" --no-default-features ${features:+--features "$features"} \
      --target "$target" -- -D warnings
  done
done
