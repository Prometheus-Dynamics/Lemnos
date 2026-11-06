# lemnos-drivers-k230isp

The Canaan K230D's VeriSilicon ISP8000 (the MRV-derived ISP behind the vendor kernel's
`vvcam-isp` misc device, `/dev/vvcam-isp.0`) from Rust: its register map, the register access a
parameter builder writes through, the statistics register reads, and, with the `device` feature,
the device's register ioctls on Linux. It has no C libraries at run time and no `unsafe` code.

Status: **written, untested on board.** Nothing here has run on a K230D. The device layer
(`device`) is untested on board, and so is the register map (the sources agree, but the
unconfirmed constants are named where they are defined).

## Features

- `std` (default): the device layer's dependencies. Without it the crate is `no_std`.
- `device` (Linux only): the `vvcam-isp` register ioctls through `lemnos-linux-sys::vvcam_isp`.
  Untested on board.

## Modules

- `regs`: the register map (block bases, offsets, field masks), from the vendor's MIT headers
  (`mrv_all_regs.h`, `mrv_all_bits.h`, `vvcam_isp.h`).
- `RegisterAccess`: a register read and write. Public, and implemented by the device.
- `stats`: `Raw` (the AE grid, the histogram and the AWB means as the registers hold them) and
  `read_raw`, which reads them through any `RegisterAccess`. `HIST_BINS` is the bin count.
- `device` (feature `device`): `IspDevice`, opened on `/dev/vvcam-isp.0`, implementing
  `RegisterAccess`. The request numbers and the register-pair structure are in
  `lemnos-linux-sys::vvcam_isp`, with their layouts pinned by a test.

## What moved, and what stayed (with the camera stack)

From the camera stack's `crates/k230isp` (commit d17de2ed), this crate takes the register
interface. The builder and the statistics conversion stay in the camera stack.

| Moved to `lemnos-drivers-k230isp` | Stays in the camera stack's `k230isp` crate |
|---|---|
| `regs.rs` (all of it; `CC_COEFF_MASK` and `CT_OFFSET_MASK` public, as the builder uses them) | `params.rs`: the builder (`Params`, `RegWrite`, the range constants) and `Params::apply`, over a `RegisterAccess` |
| `device.rs` (the `vvcam-isp` ioctls, now on `lemnos-linux-sys`) | `stats.rs`: `statistics` (the conversion into the camera stack's `Statistics`) and `MEAN_FULL` |
| `RegisterAccess` (the trait, from `params.rs`) | the builder and conversion tests (`tests.rs`) |
| `stats::Raw`, `read_raw`, `HIST_BINS` (from `stats.rs`) | |

The camera stack's `Params::apply` and `statistics` keep their names; they take the trait and
the snapshot from this crate.

## Renames

Crate name: the camera stack's `k230isp` crate is `lemnos-drivers-k230isp`; the camera stack
switches by renaming its dependency or its paths. Items in the moved code:

- `device.rs`: the `libc::ioctl` calls are the safe `lemnos_linux_sys::Ioctl::call`. The
  camera stack's public `VVCAM_ISP_RESET`, `VVCAM_ISP_READ_REG` and `VVCAM_ISP_WRITE_REG` are the
  typed requests `lemnos_linux_sys::vvcam_isp::{RESET, READ_REG, WRITE_REG}`; the register pair is
  `lemnos_linux_sys::vvcam_isp::RegPair` (the camera stack's was private). `VVCAM_ISP_CLOSE_IRQ`
  and `VVCAM_ISP_FE_START` are not carried over: the builder does not use them.
- `device.rs`: `IspDevice::open`, `reset`, `statistics` and `RegisterAccess` keep their names.
- No public type, env var or capability string was product-named.

## Licences

The register map follows the vendor's VeriSilicon headers, MIT. The crate is `MIT OR Apache-2.0`,
the workspace licence.

## Tests

`cargo test -p lemnos-drivers-k230isp --all-features` runs the statistics read against a fake
register file, which checks the AWB mean's channel order. The ioctl numbers and the register-pair
layout are tested in `lemnos-linux-sys`.
