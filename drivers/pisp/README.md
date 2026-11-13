# lemnos-driver-pisp

Configuration for PiSP, the BCM2712 ISP: the uAPI layouts, the front end and back end builders,
tiling, and statistics decoding, which touch no hardware; with no features (`--no-default-features`)
that is the whole crate, with no dependencies and `no_std`, without an allocator. The default
`device` feature adds the back end driver (`be_device`, `be_hw`) and the front end driver
(`fe_device`, `fe_hw`), `camera.isp/1` and `isp.pisp/1` over the kernel module; all of their
`unsafe` is in `drivers/pisp-sys` and `drivers/pisp-fe-sys`. `be_job` and `fe_job` are the parts of
that which need no operating system.

## What is kept and what is not

Tuning belongs to the camera stack, so it is not here: no gamma curve, no sharpening strength or
threshold choice beyond the defaults, no colour encoding picked for a product, no statistics
preset. The hardware's own defaults are here, in `be::defaults`, each transcribed from libpisp with
its commit (see [Provenance](#provenance-and-licences)): the colour encodings (`Encoding`: RGB to
YCbCr and back for BT.601, BT.709 and BT.2020, limited and full range), the debin filter, the
demosaic and false-colour settings, the resampler filters (`ResampleFilter`, with the smart
selection by downscale) and the sharpening filters. A new back end holds every block's defaults and
enables none of them. A block the caller enables with an empty table is refused by
`BackEnd::prepare`, except that smart resize computes a trapezoid filter when it can.

A YUV output with no colour space conversion from the caller gets full-range BT.601 (`jpeg`):
without a matrix a YUV output's colours are wrong, so the default is the only safe choice.

Fixed values that stay, because they aren't tuning:

- `be::IDENTITY_CCM`: the hardware no-op matrix.
- `FrontEnd::new`'s output AXI settings: bus burst, QoS and FIFO thresholds, not image settings.
- The back end's AXI block: the module owns it, and a job with any byte set there is refused.
- The temporal-denoise compression offset (2048, mode 1): part of the compressed buffer's format,
  shared by compressor and decompressor.
- The `be::ops` box filter (`BOX2`): that op's definition.

## Enables and dirty bits

Each back end block has its enable bit in `global` (`bayer_enable::*` or `rgb_enable::*`). The
uAPI has no other per-block dirty word: the header says the enable bits are the dirty flags. A setter
sets its block's enable bit and clears it when given `None`; `be_dirty` holds the three flags the
uAPI names beyond the enables (`GLOBAL`, `SH_FC_COMBINE`, `CROP`), which the configuration
does not carry, because every block is finalised on every `prepare`. The sharpen and false colour
combine factors have no enable: they apply while sharpening or false colour runs.

Not yet built: stitch (the HDR second input) and HOG (its output is not sized). Both are refused
by `prepare` when enabled, and their setters and layouts are in place.

## Running a job live

This is the sequence `lemnos-isp-proof` and `lemnos-fe-proof` run on the reference board. The
configuration library builds the buffer; the client writes it; the job names buffers by number.

1. Build the configuration: `BackEnd::simple_bayer(...)` (or the blocks you set), then `prepare()`.
   It returns a `BeTilesConfig`; write it into `BeTilesConfig::SIZE` bytes with `Wire::write_to`.
2. Describe the frames to the ISP: `set_input(IspInput { width, height, bit_depth: 16, bayer })`,
   then `set_output(0, IspOutput { width, height, fourcc, pad_bytes: 0 })`. `set_output` returns
   the output's `OutputLayout` (stride and size per image).
3. Open the streams by name on the ISP's device index: `config` and `input` writable, `output0`
   readable (`Client::open_stream_writable`, `open_stream`). Each has `layout().buffers` buffers.
4. `start()` the ISP.
5. Write each `config` buffer `n` with `FrameStream::write(n, 0, &bytes)`, and each `input` buffer
   with the frame. Both are written before the job that names them is posted.
6. Post a job on the `config` stream: `post_job(bytes)` with an `IspJob { identity, layout,
   config_buffer, input_buffer, outputs }` encoded with `lemnos_core::Encode`. `layout` is
   `LAYOUT_BACK_END` (`lemnos_contract_isp_pisp`). `input_buffer` is the `input` buffer number and
   `outputs[0]` the `output0` buffer number. `post_job` returns `false` when too many jobs
   wait. A job names only buffers it has, and the job's buffers are not written again until
   it completes.
7. The completion: `output0.next_frame(timeout)` returns the frame, `bytes(&frame)` its image,
   `frame.identity()` the job's identity and `frame.buffer()` its buffer. Give the buffer back with
   `release(frame)`. A job the back end refuses is not published: `counters()` counts it in
   `dropped`, and an error the block reports in `errors`.
8. `stop()` the ISP when done.

Jobs can queue: with two or more in flight, each names its own input and output buffer. A
buffer the client holds (a frame it has not released) cannot be named by a job.

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
- `be::defaults` holds hardware defaults transcribed as numbers from libpisp at commit
  `c77865739bbde5be918d5a7692c8a8050e0abc25`: `src/libpisp/backend/backend_default_config.json`
  (colour encodings, debin, demosaic, false colour, resampler and sharpening filters) and
  `utils/colourspace_calcs.py` (the matrices' 4.10 and 2^-18 scaling). Its tests recompute the
  matrices from the documented luma coefficients as the script states them.
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

## The camera chain and its streams

A PiSP camera is a chain of `link`s on the board: sensor → receiver → front end → back end. Each
hop's contract is checked when the device binds: a sensor's link names a `camera.receiver/1`
device; a receiver's link and each ISP's link name a `camera.isp/1` device. A link to the wrong
contract is refused, and the device's health says what the link needs and what its target serves.
The front end and the back end are both `camera.isp/1`, so a front end links to a back end, and a
back end has no link. A receiver with no link is a camera without an ISP, which is valid.

The streams a consumer opens, by device, with who writes each one:

| Device | Stream | Direction | Who writes | Buffers on the F2 reference board |
|---|---|---|---|---|
| receiver (`rp1-csi2`) | `frames` | out | the receiver: packed RAW10 or RAW8 frames | 8 of 1280000 bytes, dma-buf (CMA) |
| front end (`pisp-fe`) | `config` | in | the consumer: one configuration per job | 8 of 632 bytes, the job names one |
| front end | `statistics` | out | the front end: AWB, AGC and focus statistics, a job's result (declared `job` by the driver) | 8 of 23200 bytes, dma-buf |
| front end | `output0` | out | the front end: the frame, 16-bit Bayer, from the job | 4 of 2052096 bytes, dma-buf |
| front end | `output1` | out | the front end: a half-size Bayer frame, when the job turns it on | 4 of 516096 bytes, dma-buf |
| back end (`pisp-be`) | `config` | in | the consumer: one configuration per job | 2 of 16720 bytes (`job_slots` 8) |
| back end | `input` | in | the consumer: the raw frame the job reads | 2 of 2048000 bytes, dma-buf |
| back end | `output0` | out | the back end: the job's YCbCr (NV12 by default) frame | 4 of 1600000 bytes, dma-buf |
| back end | `output1` | out | the back end, when the job turns it on | as the board file gives |

The names are the board file's stream names (`[devices.streams.NAME]`) and the `CameraIsp` methods
`config_stream`, `statistics_stream`, `input_stream` and `output_stream(n)` answer them.

**From the front end to the back end.** The two are separate streams with separate buffers. Nothing
in Lemnos hands a front-end `output0` buffer to the back end as its `input`: the consumer carries the
frame, by mapping the front end's output and writing it into a back-end input buffer. That copy is the
consumer's; it is not measured on the board. The F2 runs fed the back end from a frame the client wrote
itself (`lemnos-isp-proof`, `lemnos-be-raw`).

**The receiver's raw frames beside the front end.** The module refuses a receiver stream while the
front end streams (`lemnos_rp1_csi2.c`, `EBUSY`), and the kernel README says why: in front-end mode the
CSI-2 channel feeds the front end instead of memory. The stock driver takes the same channel out of
raw mode in that mode (`fe_csi2_channel`). A consumer that wants packed raw frames and the ISP at once
does not get them from this block at this commit. The front end's `output0` is the raw frame
unpacked to 16 bits, which is what a consumer gets alongside the ISP.
