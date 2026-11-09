# lemnos-driver-pisp

Configuration for PiSP, the BCM2712 ISP: the uAPI layouts, the front end and back end builders,
tiling, and statistics decoding, which touch no hardware; with no features (`--no-default-features`)
that is the whole crate, with no dependencies and `no_std`, without an allocator. The default
`device` feature adds the back end driver (`be_device`, `be_hw`) and the front end driver
(`fe_device`, `fe_hw`), `camera.isp/1` and `isp.pisp/1` over the kernel module; all of their
`unsafe` is in `drivers/pisp-sys` and `drivers/pisp-fe-sys`. `be_job` and `fe_job` are the parts of
that which need no operating system.

## What is kept and what is not

Tuning belongs to the camera stack, so it is not here: no gamma curve, no resampling filters, no
sharpening preset, no demosaic or false-colour settings, no colour encodings, no statistics preset.
A block the caller enables with an empty table is refused by `BackEnd::prepare`, except that smart
resize computes a trapezoid filter when it can.

Fixed values that stay, because they aren't tuning:

- `be::IDENTITY_CCM`: the hardware no-op matrix.
- `FrontEnd::new`'s output AXI settings: bus burst, QoS and FIFO thresholds, not image settings.
- The temporal-denoise compression offset (2048, mode 1): part of the compressed buffer's format,
  shared by compressor and decompressor.
- The `be::ops` box filter (`BOX2`): that op's definition.

## Layout check (manual)

`layout-check/run.sh <linux-tree>` compiles `check.c` against the kernel headers and diffs the
sizes and offsets it prints against the generated block in `src/uapi/layout.rs`. It needs a Raspberry
Pi kernel tree, for `drivers/media/platform/raspberrypi/rp1_cfe/` and
`include/uapi/linux/media/raspberrypi/`.

## Provenance and licences

- The uAPI layouts and constants are transcribed from the Linux headers `pisp_be_config.h`,
  `pisp_fe_config.h`, `pisp_statistics.h` and `pisp_common.h`, which are
  `GPL-2.0-only WITH Linux-syscall-note`: an interface definition, mirrored as layouts only. No
  kernel source is copied.
- The front end finalisation, back end preparation, tiling, ops and format helpers are ported from
  libpisp, BSD-2-Clause, Copyright (C) 2021 - 2023 Raspberry Pi Ltd. Each module names its source.
- `be::lsc` is derived from the Raspberry Pi PiSP IPA in libcamera, BSD-2-Clause, Copyright (C) 2023
  Raspberry Pi Ltd.

```text
Copyright (c) 2023, Raspberry Pi Ltd All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice,
this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
this list of conditions and the following disclaimer in the documentation
and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```
