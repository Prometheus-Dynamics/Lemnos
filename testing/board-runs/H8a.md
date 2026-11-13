# H8a board run: the A733 video engine's encoder through `lemnos.ko`

Status: untested on board. The module's register-job engine (`kernel/lemnos_regjob.c`) and the
encoder's profile (`kernel/lemnos_regjob_a733.c`) pass the QEMU harness on a fake register block:
a job's writes, the address registers the module writes, the derived bitstream end and size, the
read-back, 41 refusals that leave every register as it was, a timeout recovered by a reset, and
an unbind with a session open. The `a733-video` driver's module route passes its tests over a
played-back session. No job has reached the engine.

The register map is transcribed, not confirmed on this engine. The window, top register, reset
bits, interrupt registers and compatible come from the vendor kernel (`radxa/allwinner-bsp` at
`a842bb8dc74608ba13975c97c31b9518f1976c01`: `ve_plat_sun60iw2.c`, `cedar_ve.c`); the encoder's
address and geometry registers come from the mainline Cedrus register list and a public
reverse-engineered H.264 encoder for earlier sunxi engines. Checks 2 and 3 below are what decide
whether the profile is right for this engine. If any of them fails, the profile is wrong: stop
and report, do not widen it on the board.

Run in a local session on an A733 board running the vendor kernel. Build on NVMe or SSD, never
on the Github HDD. Module loads, unbinds and reboots are allowed; report each.

## Checks

1. **Bind the engine to the module.** Find the engine's platform device (`ls
   /sys/bus/platform/devices | grep ve2`) and its compatible (`cat .../of_node/compatible`; pass:
   `allwinner,sunxi-cedar-ve2`). Unbind it from the vendor driver, then `insmod lemnos.ko
   owned=allwinner,sunxi-cedar-ve2 a733_ve=allwinner,sunxi-cedar-ve2`, set `driver_override` to
   `lemnos` and bind. Pass: bound, `dmesg` shows no error, and the device has its interrupt in
   `/proc/interrupts` and a reset line (a `resets` property in `of_node`). Record whether the
   node has `iommus`: if it does, every bus address below must be an IOMMU address.
2. **The registers the profile names.** Open a session and run a job of the frame-size, stride,
   control, encoder-select and bitstream-offset writes of check 3 with every address in place,
   a start value of `0x8` and reads of `0x800..0x810`, `0xa00..0xa10` and `0xb00..0xbc4`. Pass:
   no bus fault, `0x800` reads back bit 7 set, and each address register reads the bus address
   the module wrote (the import's `addr` plus the job's offset).
3. **An encode.** Encode one 64 by 64 NV12 frame of a known pattern as an intra picture: the
   input planes, a bitstream buffer of 64 KiB, and the reference, reconstruction, sub-sampled
   luma, macroblock-info and extra buffers sized as `ve_check` in `lemnos_regjob_a733.c` computes
   them, all from `/dev/dma_heap/system`. Pass: status bit 0 (encode done) within 100 ms, a
   nonzero bitstream length in `0xb90`, the bitstream decodes (prefix SPS/PPS headers) to a
   picture within a few levels of the input, and `dmesg` shows no IOMMU fault or bus error.
4. **The extents are tight.** Repeat check 3 with each buffer allocated exactly the size the
   module requires for it, placed at the end of its own dma-buf. Pass: no IOMMU fault for any of
   them. Then shrink each by one 4 KiB page below the requirement. Pass: the module refuses the
   job with `EFAULT` before writing a register.
5. **Refusals on the board.** A write to each address register, a write to `0xb14`, `0xb84`,
   `0xb8c` and `0x804`, a buffer not imported, a range past its buffer. Pass: each refused, with
   the engine's registers unchanged (read them back in the next good job).
6. **A timeout is recovered.** Encode a large frame (for example 1920 by 1088, buffers sized as
   the module requires) with a timeout of 1 ms, too short for the engine to finish. Pass:
   `ETIMEDOUT`, `LEMNOS_IOC_REGJOB_INFO` shows one timeout, one reset and nothing live, `dmesg`
   shows no IOMMU fault after the reset, and the next encode of check 3 completes with the same
   bitstream as before.
7. **Through the driver.** Run `lemnosd` with a board file giving the device `driver =
   "a733-video"`, `route = "module"` and `block` set to the engine's device name, and send the
   job of check 3 on its `jobs` stream. Pass: `Done`, the status word first in the read-back,
   and 0 allocations per job after the first.
8. **Unbind with a session open.** While the driver runs jobs in a loop, unbind the engine from
   `lemnos`. Pass: jobs fail, the driver reports `reopening`, the session closes, `rmmod lemnos`
   then succeeds. Bind it again: jobs succeed again within 8 s.
