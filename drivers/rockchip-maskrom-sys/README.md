# lemnos-driver-rockchip-maskrom-sys

The unsafe part of the Rockchip maskrom stage. Holds only:

- `read32` and `write32`: volatile MMIO primitives, one operation per block, each with a `SAFETY` note;
- the board's DMA memory for the bulk pair and the device side (`Dma`, implementing the stage's `DmaMem` and `EventMem`):
  the TRBs, the receive and transmit buffers, the event buffer the controller writes, the setup packet the controller
  writes, and the control data buffer. Each is cache-line aligned, and each cache operation is its own block with a
  `SAFETY` note;
- `src/stage471.S`: the 471 watchdog stub and its arm-and-spin entry, position-independent, assembled into this crate for
  `aarch64-unknown-none`;
- the 472 loader's entry, which calls into `drivers/rockchip-maskrom-stage`.

No logic lives here. The register sequences, the device side and the host tests are in `drivers/rockchip-maskrom-stage`
(which forbids unsafe code and reaches the hardware through these functions).

This crate is bench tooling. It is never linked into `lemnosd` or any shipped program.
