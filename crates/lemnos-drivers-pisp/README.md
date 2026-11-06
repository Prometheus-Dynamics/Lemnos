# lemnos-drivers-pisp

The Raspberry Pi 5 ISP (PiSP) from Rust: the uAPI configuration and statistics layouts, the
front end and back end configuration builders, the back end tiling, statistics decoding, and,
on Linux with the `device` feature, the driver for the stock `rp1-cfe` front end and `pispbe`
back end video nodes. It has no C libraries at run time and forbids `unsafe` code.

## Features

- `std` (default): float rounding from `std`. Without it the crate is `no_std` + `alloc`, and
  everything except `device` builds for bare-metal and wasm targets (`scripts/check-nostd.sh`).
- `device`: runs the nodes through `lemnos-v4l2` (video nodes, media graph, subdevices) and
  `lemnos-linux` (dma-heap buffers). Linux only.
- `debug-hooks` (implies `device`): environment-variable hooks for diffing configurations
  against another stack's (see below). Off, they compile to nothing.
- `be-module` (implies `device`): `device::be_module`, the back end through Lemnos's own module,
  `/dev/lemnos-pisp-be` (`kernel-modules/lemnos-pisp-be`), instead of the `pispbe` nodes.
  Opt-in; the `pispbe` path stays the default and does not change. `BeModuleJob::new` prepares a
  `be::ops` op with strides of its own choosing, `BeModule::submit` queues it on the caller's
  dma-bufs and returns at once, and `BeModule::wait` returns the finished jobs, each with its
  interrupt's `CLOCK_MONOTONIC` time. Measured on a board for one op (see the module's README);
  the Bayer path through it is untested.
- `csi2-module` (implies `device`): the CSI-2 receiver through Lemnos's own module,
  `/dev/lemnos-rp1-csi2` (`kernel-modules/lemnos-rp1-csi2`), beside the `rp1-cfe` nodes.
  Opt-in. Three parts:
  - `device::csi2_module`: `Csi2Module` (start the link and stream, queue one dma-buf per frame,
    wait for each frame's start and end times) and the `Receiver` trait it shares with
    `MockReceiver`. The mock writes frames from a function into the queued buffers at a set
    rate, for a simulator without a receiver.
  - `device::cci`: a sensor's 16-bit registers over i2c-dev (`I2cCci`, which claims the address
    and never forces it) or in memory (`MockCci`).
  - `RegisterFile`: the register file read at run time. This crate ships no sensor tables.

## Modules

- `uapi`: `#[repr(C)]` mirrors of the structures; `layout.rs` checks their sizes and offsets at
  compile time.
- `fe`: the front end configuration (input, black level, statistics, outputs).
- `be`: the back end configuration, its defaults, tiling (`be::tiling`), lens shading tables
  (`be::lsc`), and the image operations on it (`be::ops`).
- `stats`: decoding of the front end statistics buffer.
- `format`: strides, pixel offsets and the common formats.
- `device` (feature `device`): the nodes. Most of it is untested on board; see below.

## Refusing bad configurations before submit

A bad back end job is not reported and passed over. A job that never finishes stops the
back end for every client of it, and only a reboot clears it. So every configuration is checked
when it is built, and a bad one (for example 8-bit uncompressed Bayer input, which `be::prepare`
refuses) returns an error before anything is submitted. Keeping every refusal is a hard
requirement; a new path that submits without the check is a bug.

## Licences and provenance

- The uAPI layouts and constants are transcribed from the Linux headers `pisp_be_config.h`,
  `pisp_fe_config.h`, `pisp_fe_statistics.h` and `pisp_common.h`, which are
  `GPL-2.0-only WITH Linux-syscall-note`: an interface definition, mirrored as layouts only.
- The front end finalisation (`fe.rs`), the back end preparation, defaults, tiling and ops
  (`be/`), and the format helpers (`format.rs`) are ported from libpisp 1.3.0,
  BSD-2-Clause, Copyright (C) 2021 - 2023 Raspberry Pi Ltd. Each of those modules names its
  libpisp source, and the notice is in `src/lib.rs`.
- `be::lsc` is derived from the Raspberry Pi PiSP IPA in libcamera (`src/ipa/rpi/pisp/pisp.cpp`,
  BSD-2-Clause, Copyright (C) 2023 Raspberry Pi Ltd); its notice is in that module.
- Nothing is taken from the stock kernel driver (`pisp_be.c`) or its driver-internal headers.

The BSD-2-Clause text the libpisp files require is in `src/lib.rs` (crate docs).

## Layout check (manual)

`layout-check/check.c` prints the sizes and offsets of the uAPI structures as the C compiler
sees the headers, and `layout-check/run.sh <linux-tree>` diffs them against the generated
assertions in `src/uapi/layout.rs`. It needs the kernel tree the device runs (the Raspberry Pi
kernel's `include/uapi/linux/media/raspberrypi` and the `rp1_cfe` driver headers), so it is not
part of the test suite or CI. Run it after a kernel change that touches the PiSP headers.
Last run 2026-11-06 against the Raspberry Pi 7.2 tree (53679a578): it matches.

## Tests

`cargo test -p lemnos-drivers-pisp` runs the host tests (default features).
`cargo test -p lemnos-drivers-pisp --features device` adds the device-side tests, which use
fakes and temporary files and need no hardware. The example `be_ops_check` needs the `device`
feature and a board, so it is `required-features` and is not built by default; so does
`be_module_check` (feature `be-module`), which compares the module with the `pispbe` nodes and
runs the IOMMU and stopped-block tests. `csi2_module_check` (feature `csi2-module`) sets a
sensor up from a register file, captures through the receiver module, and reports the frame
rate, the frame start to frame end time, drops, and a check of the test pattern's bars.

## Debug hooks (feature `debug-hooks`)

- `LEMNOS_PISP_BE_DUMP=DIR[:N,N,...]` and `LEMNOS_PISP_FE_DUMP=DIR[:N,...]` write the configuration
  of the numbered jobs (default 30 and 90, counted from 0) to `DIR/pisp_be_<N>.bin` and
  `DIR/pisp_fe_<N>.bin`.
- `LEMNOS_PISP_BE_PATCH=REF.bin:block,block,...` copies the named blocks of a reference
  configuration into every back end configuration the caller submits.

## Board status

The device layer runs on `lemnos-v4l2` and `lemnos-linux` (the same names as the layer it was
moved from). The board measurements in the source (`be::ops` exactness, the timings in
`device`) were taken on the earlier implementation. The device layer is untested on board since
the move.
