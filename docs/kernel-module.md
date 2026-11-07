# Kernel module: the PiSP back end and the CSI-2 receiver

TODO item 5 plans one small kernel module of Lemnos's own. It claims a block, maps DMA buffers,
exports dma-bufs, forwards interrupts and maps registers. The drivers above it are Rust in user
space. The module is a fast path and never a requirement: the stock V4L2 driver stays the
fallback. The PiSP back end (BE) on BCM2712 is the first block. This file answers the four
questions set as the first task.

The sources read are the Raspberry Pi kernel 7.2 tree at `53679a578`. Paths are relative to that
tree:

- `drivers/media/platform/raspberrypi/pisp_be/pisp_be.c`, the stock driver (1856 lines)
- `drivers/iommu/bcm2712-iommu.c`
- `drivers/clk/bcm/clk-raspberrypi.c`
- `arch/arm64/boot/dts/broadcom/bcm2712-ds.dtsi`
- `include/uapi/linux/media/raspberrypi/pisp_be_config.h`

Sections 5 to 7 record the board runs since.

## The block as the kernel sees it

The device-tree node is `pisp_be@880000`, compatible `raspberrypi,pispbe`:

- registers at `0x10_0088_0000`, `0x4000` long
- one level-high interrupt, GIC SPI 72
- one clock, `firmware_clocks 7`
- `iommus = <&iommu2>`
- no `resets` and no `power-domains`

The register interface is small. These are all the registers the driver touches:

| Offset | Register | Use |
|---|---|---|
| `0x00` | version | `0x0225270x` on BCM2712 (minor in the low 4 bits) |
| `0x04` | control | queue a job (bit 0), copy config (bit 1), tile count (bits 16 and up) |
| `0x08`, `0x0c` | tile address low and high | DMA address of the job's tile list |
| `0x10` | status | bit 0: a job is queued |
| `0x14` | batch status | done count (bits 0-7), started count (bits 8-15) |
| `0x18` | interrupt enable | `3` (both flags) |
| `0x1c` | interrupt status | write-1-to-clear |
| `0x20` | AXI | `0x32703200` (QoS, cache, prot, sub-64-byte bursts) |
| `0x40` and up | config | the job's config words. The buffer addresses come first, as 8-byte low/high pairs per buffer. |

A job is the config struct (`pisp_be_tiles_config`, uAPI), written word by word into the config
registers, plus a tile list the block reads by DMA. The block keeps a two-deep queue: one job
running and one queued. The interrupt handler compares the done and started counters with its own
copies to tell which job finished (`pisp_be.c:651-704`).

## 1. IOMMU coverage

**Every BE access goes through `iommu2`, but the IOMMU only translates addresses inside its
aperture. Addresses below the aperture reach memory unchanged.** So the IOMMU does not confine
the block, and the module has to.

- `iommu2` serves the BE and the HEVC decoder (`bcm2712-ds.dtsi:275-282, :375, :385`). The
  driver keeps a single domain per IOMMU (`mmu->domain`, `bcm2712-iommu.c:237-255`), so the two
  blocks share one IOVA space. Through the DMA API that is safe, because the IOVA allocator hands
  out disjoint ranges.
- The aperture defaults to 40 GiB to 42 GiB. The device tree widens it to 4 GiB, so it runs from
  40 GiB to 44 GiB (`bcm2712-iommu.c:115-116, :667-685`). The driver writes a bypass range that
  covers everything below the aperture, and its comment says why: "Addresses below the
  'aperture' are passed unchanged" (`bcm2712-iommu.c:180-202`). BCM2712 DRAM sits well below
  40 GiB. So a BE address register that holds a physical DRAM address goes straight to that
  memory, untranslated and unchecked.
- The BE's DMA mask is 36 bits (`pisp_be.c:1766`).
- Write and read violations inside the aperture abort (`MMMU_CTRL_*_VIOLATION_ABORT_EN`).

**What this means for the module.** User space must never write an address the block will use.
This covers three places:

1. the buffer address pairs at the start of the config registers
2. the tile-list address registers
3. the `addr` fields inside the config struct, which the stock driver overwrites before it copies
   the struct into the registers (`pispbe_xlate_addrs`, `pisp_be.c:376`)

The module takes dma-buf file descriptors, maps each one through the DMA API on the BE's own
platform device, which yields IOVAs inside the aperture, and writes those itself. It copies the
tile list into a buffer it owns. Register mapping straight into user space (one of the module's
planned services) must not be offered for this block. The config words reach the block only
through the module, after it has put its own addresses in. Every other config field is a
parameter, not an address: the tiles carry offsets, not pointers.

## 2. Interrupt-to-user-space latency

**Not measured yet.** It needs the module on a board. The design:

- The hard interrupt handler only reads and clears the status register, reads the batch
  counters, and records `ktime_get_ns()`. It wakes a wait queue and does nothing else.
- User space waits with `poll` on the module's descriptor, then `read`s a completion record:
  the job id, the done and started counters, and the interrupt timestamp.
- The latency is the gap from that timestamp to `CLOCK_MONOTONIC` in user space just after
  `read` returns, which needs no extra instrumentation. Report p50, p99 and max, with the
  service thread at normal priority and at `SCHED_FIFO`, alone and beside a 120 fps pipeline.

For scale: a frame at 120 fps is 8.3 ms. A wakeup on an idle Cortex-A76 typically takes tens of
microseconds. The thing to watch is the tail under load, as the `lemnosd` schedule tests showed
on the desktop.

The two-deep hardware queue hides most of the latency. The next job is queued while the current
one runs, so the latency adds to the time-to-result for each frame, not to the block's
throughput.

## 3. Clocks, power and reset

- **Clock.** Firmware clock 7 is `RPI_FIRMWARE_ISP_CLK_ID`, and the firmware owns its rate. The
  driver never sets a rate. It only enables the clock in runtime resume and disables it in
  runtime suspend, with a 200 ms autosuspend (`pisp_be.c:1665-1688, :1776`). The Linux side
  marks the ISP clock `CLK_IS_CRITICAL` (`clk-raspberrypi.c:151-155`), so the disable never
  gates it. The block stays clocked whenever the system runs. The module needs one
  `clk_prepare_enable` at bind and nothing more.
- **Power.** The node has no power domain. Runtime PM gates nothing real, because the only
  resource it handles is that critical clock. The module can skip runtime PM.
- **Reset.** None is reachable from Linux:
  - the node has no `resets`
  - the driver has no reset sequence
  - gating the clock is a no-op, because the clock is critical

  At probe, the stock driver refuses a block that is still busy with `-EBUSY` "HW is stuck or
  busy": status nonzero, or done ≠ started (`pisp_be.c:1700-1713`). This agrees with what board
  #2 showed, that a stuck job stops the block for every client and only a reboot clears it. The
  module keeps the same probe check. It refuses bad configs before submit, because it cannot
  recover from them, and on a job timeout it reports the block dead instead of retrying. Two
  things are worth trying on a board, since a reboot is the recovery path anyway:
  - a firmware clock rate change while the block is stuck
  - re-writing the AXI register and clearing the interrupt status

  If neither works, "only a reboot" stands.

## 4. GPL position

- **The module is GPL-2.0.** It lives under `kernel-modules/` at the repository root, kept apart
  from the MIT OR Apache-2.0 crates (block-move step 6). Porting from `pisp_be.c` (GPL-2.0,
  copyright Raspberry Pi Ltd) into the module is allowed, provided the copyright notice stays on
  whatever is ported.
- **The user-space drivers stay MIT OR Apache-2.0.** What they may use:
  - The uAPI config header, `pisp_be_config.h`, and `pisp_common.h`. Both are `GPL-2.0-only WITH
    Linux-syscall-note`. The note covers programs that use the kernel through its interfaces.
    `lemnos-linux-sys` already transcribes uAPI headers such as `videodev2.h` into `#[repr(C)]`
    structs on this basis, and the same works for these.
  - libpisp, which is BSD-2-Clause, for format and tiling knowledge, with its notice kept.
- **What they may not take.**
  - Code from `pisp_be.c`.
  - Anything from `pisp_be_formats.h`. That is a plain GPL-2.0 driver-internal header, not uAPI,
    so it must not be copied into user space. The format table is rebuilt from libpisp or from
    the uAPI instead.
  - The register offsets and the job sequence in the table above. These are interface facts, but
    they stay in the module: user space never programs this block's registers directly (see
    question 1).

The split that answers the security question also answers the licence one. The register-level
code is GPL in the module. The permissively licensed user space deals only in uAPI structs and
dma-buf handles.

## 5. On the board

Measured 2026-11-06 on board #1 (a Pi 5 class board, 7.2.9-v8-16k) with the module,
`kernel-modules/lemnos`, and `lemnos-drivers-pisp`'s `be_module_check`. The full table
is in the module's README.

- **Latency.** Interrupt to user-space wake-up: p50 2 µs, p99 20 µs, max 76 µs under
  `SCHED_FIFO` over 10000 jobs; p50 2 µs, p99 30 µs, max 411 µs at normal priority over 2000.
  The job's round trip (an NV12 640x480 downscale) is the block's own time, p50 511 µs, the same
  as through `pispbe` (505 µs), once the module keeps buffers mapped between jobs as vb2 does.
  Mapping per job cost about 50 µs a job.
- **Soft recovery.** A Bayer TDN and stitch input on an RGB input stopped the block (status 0x2,
  started one ahead of done). Neither idea of question 3 revived it: the AXI rewrite with the
  interrupts cleared, and the ISP clock halved and restored, both left status 0x2, and `pispbe`
  refused the block at probe until a reboot. "Only a reboot" stands.
- **IOMMU violation.** Nothing reaches the block's status, and nothing is logged: the bcm2712
  IOMMU driver has no fault reporting, and an unmapped page translates to its "default" page.
  A job whose input was unmapped while it ran finished with status 0 and wrong output; one
  whose output was unmapped wrote almost nothing to it. So the module must never unmap a buffer
  while a job holds it, which it does not, and it keeps a stopped block's buffers mapped.

## 6. The CSI-2 receiver

`kernel-modules/lemnos` is the second block: RP1's CSI-2 receiver, without the PiSP
front end. The block has the same split:
- **The module.** GPL-2.0, ported from `rp1_cfe`. It writes every address, and it only receives.
- **The sensor.** Driven from user space over i2c-dev from a register file read at run time
  (`lemnos-drivers-pisp`'s `device::cci`). The sensor's kernel driver is unbound first, because
  i2c-dev claims the address and never forces it.

The questions of sections 1 to 3, for this block:
- **Addresses.** RP1 has no IOMMU in front of it, so a buffer must be one contiguous segment (a
  CMA heap buffer). The module checks this at `QUEUE`.
- **Latency.** Each frame's start and end interrupts are timestamped in the module and returned
  with the buffer.
- **Recovery.** `STOP` forces the DMA channel off as `rp1-cfe` does. Rebinding `rp1-cfe`
  afterwards worked on each run.

Measured 2026-11-06 on board #1 with an OV9782 at 1280x800 RAW10, 2 lanes at 800 Mbit/s:
- 3600 frames at 60.28 fps, none failed or dropped;
- frame start to frame end median 7465 µs;
- the colour-bar test pattern exact.

- With `UNPACK16`, each frame's buffer went straight into a back end demosaic job (no copy):
  600 frames, frame end to the back end's interrupt p50 757 µs, p99 1324 µs.

The full record is in the module's README.

## 7. One module (2026-11-06)

The per-block modules became one, `kernel-modules/lemnos` (`lemnos.ko`):
- **A generic core.** Driver `lemnos`, node `/dev/lemnos-<device>`. It claims any block Lemnos
  owns (the `owned` parameter's compatibles) and gives user space:
  - its register regions;
  - coherent DMA buffers exported as dma-bufs;
  - dma-bufs imported to contiguous device addresses;
  - interrupts as reads, masked from firing until the driver re-enables them.

  Registers are mapped to user space only where the block's DMA is confined to what the module
  mapped.
- **The two blocks above as personalities.** Their DMA is not confined, so the module keeps
  writing their jobs. Their driver names, nodes and ioctls are unchanged, so `be-module` and
  `csi2-module` run as before.

On board #1:
- `test/lemnos_check` passed 20 of 20 with the generic core bound to the back end's node.
- In four interleaved 120 fps chain runs, receiver to back end, 9000 frames each, the
  personalities matched the old modules: 0 failed, frame end to back end interrupt p99
  923-962 µs for both.

The record is in `kernel-modules/lemnos/README.md`.
