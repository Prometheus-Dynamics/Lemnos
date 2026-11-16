# lemnos-driver-rockchip-maskrom-sys

Bench tooling for the Rockchip maskrom flasher. Holds only:

- `read32` and `write32`: volatile MMIO primitives, one operation per block, each with a `SAFETY` note;
- `src/stage471.S`: the 471 watchdog stub and its arm-and-spin entry, position-independent, assembled
  into this crate for `aarch64-unknown-none`.

No logic lives here. The register sequences and the host tests are in
`drivers/rockchip-maskrom-stage` (which forbids unsafe code and calls these functions).

This crate is bench tooling. It is never linked into `lemnosd` or any shipped program.
