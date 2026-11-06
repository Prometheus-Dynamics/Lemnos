# Driver ownership

Where a driver lives: in Lemnos, in a board file, or in the product that uses it. Decided by the
user on 2026-11-06. It sits beside [scope-overlaps.md](scope-overlaps.md), which settles the
non-hardware parts of the tree.

**The test:** a driver goes in Lemnos if a second, unrelated product would use it unchanged.

## 1. Generic drivers belong in Lemnos

- **Chip drivers.** `no_std`, over `embedded-hal` 1.0, so the same crate runs on Linux and on a
  microcontroller. Examples: IMUs and other sensors (`lemnos-drivers-bmi088`,
  `lemnos-drivers-bmm150`, `lemnos-drivers-ina2xx`), and LED and fan controllers
  (`lemnos-drivers-ws2812`, the fan composite).
- **SoC block drivers.** PiSP (`lemnos-drivers-pisp`), RGA (`lemnos-drivers-rga`), VGLite
  (`lemnos-drivers-vglite`), rkisp (`lemnos-drivers-rkisp`), the K230 ISP
  (`lemnos-drivers-k230isp`) and V4L2 (`lemnos-v4l2`).

## 2. Board wiring is data

Which bus, address, pin, regulator or interrupt a part sits on is written in the device
package's board file ([board-definition.md](board-definition.md)), not in code. A new board
with known parts needs a board file and no new crate.

## 3. Product-only drivers live in the product

A driver for an application-specific or product-only part lives in the product's own
repository:
- **Form.** A crate on `lemnos-driver-sdk`, registered with `DriverRegistry::register`.
- **Linking, by default.** Linked into that product's `lemnosd` at build time.
- **Runtime loading.** Only for a drop-in that cannot be rebuilt, through `lemnos-capi` and its
  stable version symbol (`lemnos_version`, `lemnos_check_version`).
  - Not built yet: `lemnos-capi` is a client surface today and has no driver-loading entry
    point.
  - It is added when a product needs one, by the normal dynamic linker like the `shared`
    feature, never dlopen.
- **Moving into Lemnos.** A product driver moves into Lemnos once it is made generic, that is,
  once it passes the test above.

## 4. Product policy stays with the consumer

What states mean, LED looks, and the calibration workflow belong to the product, as
[scope-overlaps.md](scope-overlaps.md) decides for `lemnos-light` and `lemnos-fusion`. Lemnos
carries the mechanism: the look engine, the filter and the fit.

## 5. Kernel code

- **One generic Lemnos kernel module per board.** It claims a block, maps DMA buffers and
  dma-bufs, forwards interrupts and maps registers. The drivers themselves are user-space Rust.
  Today the module is one block at a time on Pi 5 class boards:
  - `kernel-modules/lemnos-pisp-be`, the PiSP back end;
  - `kernel-modules/lemnos-rp1-csi2`, the CSI-2 receiver.

  See [kernel-module.md](kernel-module.md).
- **Kernel-side code only where the kernel's own graph must see the device.** For example, a
  sensor that V4L2 or libcamera has to enumerate (`kernel-modules/lemnos-sensor-bridge`). That
  code lives in Lemnos's `kernel-modules/` (GPL-2.0) and stays minimal.
- **Products never ship their own kernel modules.** A product that needs kernel help asks for a
  generic feature in the Lemnos module.
- **No patches to other people's drivers.** A Lemnos module binds beside the stock driver
  through `driver_override`; it never replaces or patches it.
