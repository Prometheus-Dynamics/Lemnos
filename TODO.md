# TODO

What is still open in Lemnos. Finished work is in [CHANGELOG.md](CHANGELOG.md) and the git history.
Boards here: two BCM2712 boards, #1 and #2, each with a USB webcam and a Coral USB accelerator.

## Next: the move to the new architecture

The plan is under review and is kept outside this repository, because it names the products. Lemnos
becomes only the engine: it talks to hardware without knowing what the hardware is, and drivers and
their contracts live elsewhere. Nothing here starts until the user says.

These are carried into that plan, because each is unbuilt and still wanted:

- **Host facts as drivers:** thermal, power, clock and RTC facts, the hardware watchdog, and USB
  gadget configfs setup.
- **The frame vocabulary:** pools, buffers, fences and accounting on the Lemnos side, graph slots and
  backpressure on the graph runtime's. Lemnos carries an opaque `u64` frame identity it never
  interprets.
- **Perf counters:** per-thread groups read into a caller-owned array, a ZST when off. On board #1
  (A76), a group `read()` of 3 counters took 413-422 ns; user-mode reads took 48.5 ns for 3 and
  7.1 ns for cycles only, with `perf_user_access`=1 and `config1` bit 1. User-mode values are 2^32
  off `read()` with the documented conversion: settle that on a board before the backend lands.
- **OV9782 beyond parity:** 2x2 analog binning (0x3820[0], 0x3821[0..1], 0x3837), fractional
  exposure (0x3500-0x3502 carry 1/16 row), a test-pattern control (0x5E00), the first-frame drop
  (0x4242), array crop (0x3800-0x3807). Keep RAW8 at 0x030D = 0x60 (96 MHz system clock). Never
  write 0x3D80 (OTP program).

## Rulings in force

- **Failure handling (all crates):** never give up; retry backoff capped at 5-10 s; never auto-off by
  default. An easy manual off and on. Auto-off only as a configured policy. Backed-off and disabled
  state visible, backoff self-clearing.
- **Out of Lemnos:** the LED look catalogue and its meanings go to the OS image's API (ws2812
  driving, the ring map and the animator stay available); PiSP tuning defaults and anything 3A-like
  go to the camera stack.
- **Raw RP1 DMA** stays dropped (the memory-copy engine only).
- **A wedged USB device** is recovered by cycling its port's `gpio-power-switch`.

## Rulings needed

- **GPU.** A 2026-11-07 ruling put raw DRM and Vulkan device setup in Lemnos (through a
  caller-supplied `vkGetInstanceProcAddr`). None of it is built. The new design leaves the GPU with
  Mesa and the graph runtime, with only the advisory lease in Lemnos. Keep or retire the ruling.
- **USB in the module.** The new design keeps USB on usbfs.
- **NPUs** (RKNPU, VIP9000, KPU). Each runs command streams made by its vendor's model compiler.
  With vendor runtimes ruled out, a submit path has nothing to run, and raw submit is a path to
  arbitrary DMA. Not started; needs a ruling on where compiled models come from.
- **K230 frame path.** The SDK's V4L2 capture nodes (`vvcam`, in `k230_linux_sdk` at `fc6cc21`)
  depend on `isp_media_server`, a binary-only daemon: the first format ioctl fails with `EIO` after
  15 s if it does not answer, and it is what programs the ISP. No capture code was written on it.
  The alternative is programming the ISP through `/dev/vvcam-isp.0`
  (`lemnos-drivers-k230isp` has its register access), which needs the ISP8000 and CSI programming and
  a board. Either way, in the two driver files read, `v4l2_buffer.sequence` and the timestamp are
  never set (`buf->sequence` is the buffer index), so a frame's sequence and time need checking on a
  board.

## Hardware coverage still to write

Written from sources, tested when each board arrives.

| Chip | Still missing |
|---|---|
| RK3588S | NPU, VPU (`rk-mpp`), IEP, JPEG |
| RK3566 | NPU, VPU (`rk-mpp`), IEP, JPEG |
| A733 | VIP9000 NPU, video engine, deinterlacer, camera ISP (`vin`) |
| RV1126B | `rkaiisp`, NPU, VPU (`rk-mpp`), JPEG |
| K230D | frame path, non-AI 2D OSD and border (their controls pass pointers and physical addresses and need the checks G2D has), KPU and AI2D, VPU, GSDMA |

Known limit: blend with a YUV layer on G2D is refused, because the BSP driver's check refuses every
usable size.

## Written, waiting for a board run

- **The graph runtime's fixed-function cells** (its backend matrix at `f09fe84`: 13 untested, 2
  deferred). `tests/multirga.rs` and `tests/v4l2.rs` need a Rockchip board.
  `a_channel_to_gray_is_exact_for_every_byte` must pass before `rgba_red` is routed.
  `tests/lines.rs` (`gpu_lines_against_bresenham_by_slope_class`) needs a K230 before lines are.
- **RGA and VGLite against the CPU route** (fills `caps`' `Exactness`), and the RVV rows on a K230
  (`LEMNOS_EXPECT_VECTOR=1`, then time against the scalar loops).
- **Gyroscope alignment:** the fixed sample-time convention (`TickClock`, `align.rs`) has only been
  checked on replayed traces. Run the fixed build on a BCM2712 board.

## Asked by the camera stack, to measure

- A cached statistics buffer (a dma-heap import with a sync before reading) against the uncached map.
- The back end config copy per job: not changed until the profile on board #1 shows it matters; a
  caller-supplied dirty list is the shape if it does.
