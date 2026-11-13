# H8b board run: the K230 ISP's frames through `lemnos.ko`

Status: untested on board. The module's register-job engine gained jobs that start and are
collected later (`LEMNOS_REGJOB_RUN_NOWAIT`, `LEMNOS_IOC_REGJOB_WAIT`) and blocks that run
continuously, which switch to a job's buffers at the end of a frame in the interrupt handler and
read the shadow registers back. It also gained a second role on one block: a configuration
session (`LEMNOS_REGJOB_OPEN_CONFIG`) beside the frames session, with its own write table, checked
writes and reads at once (`LEMNOS_IOC_REGJOB_WRITE`), the ISP core's frame-out events
(`LEMNOS_IOC_REGJOB_EVENT`), and `isp_ctrl`'s bits merged between the two roles under the
module's lock. The ISP's profile (`kernel/lemnos_regjob_k230isp.c`) passes the QEMU harness on a
fake register block that runs continuously: the switch on an idle block and at a frame's end, the
previous job's buffer let go when the next job's addresses are taken, the raw output's address and
its JPEG-path pair, a block whose shadows don't show the job's buffers keeping both held, 19
refusals that leave every register as it was, both roles at once (each role's table, the merged
`isp_ctrl` bits, the events queued with their time, 64 at most with the rest counted as lost), a
stalled path recovered by a reset that the configuration session is told of, a session closed
while a job runs, and a configuration session alone. `k230isp`, `k230isp-frames` and
`k230-capture` pass their tests over played-back sessions. No frame has reached a buffer through
this path.

The register map is transcribed, not confirmed on this chip. The compatible, the three
interrupts, the reset line and the memory interface's interrupt registers come from the SDK
(`k230_linux_sdk` at `fc6cc21`, `buildroot-overlay/package/vvcam/isp/vvcam_isp_driver.c` and
`vvcam_isp_hal.h`); the main path's registers, bits and update rules come from VeriSilicon's
MIT-licensed register headers as published in `nxp-imx/isp-vvcam` at
`e6453233e67f5d2f180d6ec277158dd54fa67a25` (`vvcam/isp/mrv_all_regs.h`, `mrv_all_bits.h`,
`isp_miv2.c`). Checks 2 and 3 decide whether the profile is right for this ISP. If either fails,
the profile is wrong: stop and report, do not widen it on the board.

The sensor and the CSI-2 receiver are not this path's: something else must stream raw Bayer
frames into the ISP (the SDK's sensor and receiver drivers, or `k230-capture`'s setup), as in
E11 and E30. Note what fed the ISP in the result.

Run in a local session on a K230 board running the SDK's kernel (`k230_linux_sdk` at `fc6cc21`,
kernel `ruyisdk/linux-xuantie-kernel` at `7d4e1f44`). Build on NVMe or SSD, never on the Github
HDD. Module loads, unbinds and reboots are allowed; report each.

## Checks

1. **Bind the ISP to the module.** Find the ISP's platform device (`ls /sys/bus/platform/devices
   | grep isp`; its window is `0x90000000`) and its compatible (`cat .../of_node/compatible`;
   pass: `verisilicon,isp`). Unbind it from `vvcam-isp`, then `insmod lemnos.ko
   owned=verisilicon,isp k230_isp=verisilicon,isp`, set `driver_override` to `lemnos` and bind.
   Pass: bound with no error in `dmesg`, three interrupts on the node (`interrupts` in `of_node`)
   with the second one in `/proc/interrupts` under `lemnos`, and a `resets` property. Record
   whether the node has `iommus`.
2. **The main path's registers.** With the sensor streaming a known size (for example 1920 by
   1080, 10-bit RGGB), run one job of the `k230isp-frames` setup writes for that size with an NV12
   buffer of exactly `w*h*3/2` bytes from `/dev/dma_heap/system`, collected with
   `LEMNOS_IOC_REGJOB_WAIT`. Pass: no bus fault; the job completes (status bit 0) within 100 ms;
   `0x1370` (Y base shadow) reads the import's `addr`, `0x1374` the Y size the module wrote, and
   `0x137c` the Cb base, which is `addr + w*h`.
3. **Frames into buffers.** Run 300 jobs over four buffers, each collected before the next is
   started. Pass: every collect gives status bit 0 and no wrap bit (`0x700`), each buffer that
   comes free holds a picture (luma of a lit scene neither all 0 nor all 255; dump one to a file and
   look at it), no line is torn across two pictures, and `dmesg` shows no IOMMU fault or bus error.
4. **The extents hold.** Repeat check 3 with an NV12 buffer one 4 KiB page short of `w*h*3/2`.
   Pass: the module refuses the job with `EFAULT`. Then set the picture size to twice the height
   with the right-sized buffer (the job's ranges stay those of the buffer). Pass: the wrap bit is
   set in the status, and no byte outside the buffer changed (place a guard buffer next to it from
   the same heap and compare it before and after).
5. **Refusals on the board.** A write to each base, size and offset register of the main path
   (`0x1324`..`0x1360`), to `mp_ctrl` (`0x1310`) with `AUTO_UPDATE` or `PINGPONG`, to the self
   path's enable in `0x1300`, and to `acq_prop` (`0x404`) selecting the DMA input; a buffer not
   imported; a range past its buffer. Pass: each refused, the frames of check 3 still land in the
   right buffers afterwards.
6. **A stall is recovered.** Stop the sensor's stream with a job started. Pass: the collect gives
   `ETIMEDOUT` after the job's timeout, `LEMNOS_IOC_REGJOB_INFO` shows one timeout, one reset and
   nothing live, `dmesg` shows no fault after the reset. Start the sensor again: the next job
   completes and check 3 passes.
7. **Through the driver.** Run `lemnosd` with a board file giving the ISP `driver =
   "k230isp-frames"` and `block` set to its device name, `output0` as a dma-buf stream of four
   buffers of `w*h*3/2` bytes, and post a job per frame through `camera.isp/1`. Pass: images come
   out on `output0` with the identity of the job before, statistics with the same identity, the
   counters show no errors, and 0 allocations per job after the first four.
8. **Unbind with a session open.** While the driver runs, unbind the ISP from `lemnos`. Pass: the
   driver reports degraded, the session closes, `rmmod lemnos` then succeeds. Bind it again:
   frames come out again within 8 s.
9. **Both roles on one ISP.** With the ISP bound as in check 1, run `lemnosd` with the board file
   of `docs/boards.md` ("register jobs"): `k230-capture` with `block` and `k230isp` with `block`,
   the sensor set up as in E30. Pass: both bind; `k230-capture` delivers frames on `frames` (raw
   10-bit, then NV12), and at the same time `k230isp` applies a configuration job (read its
   registers back through `LEMNOS_IOC_REGJOB_WRITE` with reads only) and its statistics arrive
   once per frame. `/proc/interrupts` grows by the frame rate on the ISP core's line (no storm:
   each frame-out is cleared through `ISP_ICR`) and the frame ends arrive on the memory
   interface's. Record whether the two are one line. `isp_ctrl` (`0x400`) keeps `k230isp`'s bits
   (gamma out, bit 11) across `k230-capture`'s starts and stops.
10. **A stall seen by both.** In check 9, stop the sensor's stream. Pass: `k230-capture` reports
    degraded and retries; `k230isp` takes the reset event as a device error and recovers (its
    `statistics` stop meanwhile); after the sensor runs again both
    are healthy within 8 s, and the configuration reads back as it was.
11. **The raw output's memory.** In check 9 with the raw format, dump one frame: the picture is
    the sensor's, and `0x13c0` (the JPEG path's base) reads the same as `0x1394` (the raw base), as
    the module writes it.

