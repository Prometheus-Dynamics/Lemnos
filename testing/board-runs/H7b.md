# H7b board run: the graphics DMA engine through `lemnos.ko`

Status: untested on board. In QEMU, `lemnos.ko` drives a fake engine (`lemnos-test-gsdma`, a page
of memory the module is told is registers, which walks the descriptors the module built and copies
through the bus addresses they name). There, `lemnos_gsdma_check` shows 21 refusals with their
exact errno and the engine untouched, turned and mirrored copies right pixel for pixel, a job the
engine hangs on stopped and the stream going on, an engine that never stops wedged with its memory
held, and an image past the engine's reach refused; `lemnos-module-check gsdma` drives the Rust
driver's `LinuxDevice` through the same node. Nothing has run on the real engine.

Run in a local session on a K230 board running the K230 Linux SDK kernel
(`ruyisdk/linux-xuantie-kernel` at `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`, with the patches
`kendryte/k230_linux_sdk` at `fc6cc21` applies). The CM5 has no such block. Take
`/run/lock/board-gpu.lock` for the whole run. Build on NVMe or SSD, never on the Github HDD.
Module loads, unbinds and reboots are allowed; report each. Never write the engine's registers
from user space.

## Setup

1. Build `lemnos.ko` against the board kernel's build tree (`make -C kernel KDIR=... ARCH=riscv
   CROSS_COMPILE=...`), and `kernel/test/lemnos_gsdma_check.c` for the board
   (`-I kernel`, statically).
2. Find the engine: `ls /sys/bus/platform/devices/ | grep gdma` (expect `80800000.gdma`). Record
   which driver holds it (`readlink /sys/bus/platform/devices/80800000.gdma/driver`). Stop anything
   that rotates a display plane, then unbind the vendor driver from it.
3. `insmod lemnos.ko owned=canaan,gdma gsdma=canaan,gdma`, then
   `echo lemnos > /sys/bus/platform/devices/80800000.gdma/driver_override` and bind it to
   `lemnos`. Pass: `dmesg` shows no error from `lemnos`, and `/proc/interrupts` lists the engine's
   interrupt under `lemnos`.

## Checks

1. **Refusals and copies.** `lemnos_gsdma_check /dev/lemnos 80800000.gdma
   /dev/dma_heap/<cma heap> board`. Pass: `lemnos_gsdma_check board: 0 failed`. This shows every
   refusal with its errno and the engine's finished count unchanged, then an ARGB8888 copy, a
   quarter turn and an NV12 copy mirrored across, each right pixel for pixel. Record the output.
   A pixel check that fails on the real engine but passed on the fake means the fake's reading of
   the rotation or mirror codes is wrong: record which pixels, and fix the fake and the docs.
2. **Reach.** Record the `reach` and image address the board mode prints (expect `0x80000000`,
   and an image address below it). A CMA heap above 2 GiB would make every job a refusal.
3. **Timing.** A 1920x1080 ARGB8888 quarter turn through `lemnos-module-check gsdma` or a small
   loop: record the median and 99th percentile time from kick to frame. It must finish well inside
   the module's 100 ms plus 1 ms per 256 KiB; if not, the timeout is wrong for real frames.
4. **Unbind while idle and while busy.** Unbind with no stream; then with a 4K turn in flight (from
   another shell). Pass: both return, `dmesg` shows no wedge, and the module unloads.
5. **Wedge, if it can be made to happen.** Only if a way to stall the engine without writing its
   registers from user space is known (for example powering its domain down from the kernel); else
   record that it was not tried.
6. **Back to the vendor driver.** Unload `lemnos.ko`, clear the override, bind the vendor driver
   back, and confirm a display rotation still works.

Record each result here.
