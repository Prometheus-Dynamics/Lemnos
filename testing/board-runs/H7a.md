# H7a board run: the VIP9000 NPU through `lemnos.ko`

Status: untested on board. In QEMU, `lemnos.ko` drives a fake block (`lemnos-test-vip`, a page of
memory the module is told is registers, which runs the command buffer the module built through the
tables the module built, as the block's MMU would, and copies only through what they map). There,
`lemnos_vip_check` shows every command a program may not hold refused at load with its offset,
14 job refusals with their exact errno and the block untouched, copies byte for byte, nine MMU
faults (past a slot, before the first, into a slot not named, into the job before's slot, into
the program's own commands, at address 0, at an address nothing maps) each published empty with
nothing outside the job's slots changed and the next job running, a hang stopped by a reset, both
MMU set-ups (page directory and command), unknown chips and a chip without an MMU refused at bind,
and a block that won't reset wedged with its memory held. `lemnos-module-check vip` drives the
driver's claim over its Linux session through the same node. Nothing has run on the real block.

Run in a local session on an A733 board (Cubie A7A, or another `sun60iw2` board) with the vendor
kernel the Radxa `allwinner-bsp` tree builds. Take `/run/lock/board-gpu.lock` for the whole run.
Build on NVMe or SSD, never on the Github HDD. Module loads, unbinds and reboots are allowed;
report each. Never write the block's registers from user space.

## Setup

1. Build `lemnos.ko` against the board kernel's build tree (`make -C kernel KDIR=... ARCH=arm64
   CROSS_COMPILE=aarch64-linux-gnu-`), and `kernel/test/lemnos_vip_check.c` for the board
   (`-I kernel`, statically).
2. Find the block: `ls /sys/bus/platform/devices/ | grep npu` (expect `3600000.npu`). Record which
   driver holds it (`readlink /sys/bus/platform/devices/3600000.npu/driver`, expect `vipcore`) and
   its compatible (`cat /sys/bus/platform/devices/3600000.npu/of_node/compatible`). Stop anything
   using the NPU, then unbind the vendor driver from it.
3. `insmod lemnos.ko owned=<compatible> vip9000=<compatible>`, then
   `echo lemnos > /sys/bus/platform/devices/3600000.npu/driver_override` and bind it to `lemnos`.
   Pass: `dmesg` shows no error from `lemnos`, and `/proc/interrupts` lists the block's interrupt
   under `lemnos`. If the bind fails with "not one whose MMU is known", record the chip id the log
   gives: the chip table in `lemnos_vip.c` needs it (from `vip_feature_database.h`).

## Checks

1. **The chip and its set-up.** Record the chip id and MMU set-up `lemnos_vip_check` prints at
   stream creation (`LEMNOS_VIP_IOC_STATUS` has it too), and the vendor driver's own idea of them
   (`dmesg` from an earlier vipcore load). Pass: they agree.
2. **Refusals and a job of NOPs.** `lemnos_vip_check /dev/lemnos 3600000.npu
   /dev/dma_heap/<cma heap> board`. Pass: `lemnos_vip_check board: 0 failed`. This shows every load
   refusal with its offset, every job refusal with its errno and the block's finished count
   unchanged, then a job of NOP and STALL finishing (the module's event arriving with the block
   idle). If the NOP job comes back with 0 bytes, record `last_fault` and the resets from the
   status: the reset, set-up or trigger sequence differs on the real block.
3. **A fault writes nowhere and recovers.** A program of one state load to a state the block
   writes memory from, pointed outside every slot, needs a known state; until one is known from a
   public source, run a vendor-compiled program (below) with one slot left out of the job, and
   record that the run comes back with 0 bytes, `faults` counts it, `last_fault` has bit 30, and
   the next job finishes. Fill a separate dma-buf with a pattern beforehand and confirm it is
   unchanged.
4. **Vendor-compiled programs.** Take a command stream the vendor's runtime builds for a small
   network (captured from a vipcore run with its `ioctl` trace, or emitted by the vendor tools on
   the PC). Load it with `LEMNOS_VIP_IOC_LOAD`. Record whether the scan passes it and, if not, the
   refused offset and command: a program needing LINK, CALL or a control-range state would need a
   ruling on what the module may take. Then rewrite its buffer addresses to slot addresses
   (`(n + 1) << 28`) with the caller's tooling and run it: compare the output with the vendor
   runtime's.
5. **Timing.** Kick to frame for the NOP job and for the network: median and 99th percentile.
   Record whether the module's 5 s limit is far above the longest real job.
6. **The daemon.** A board file with `driver = "vip9000"`, `config = { route = "lemnos", device =
   "3600000.npu", heap = "<cma heap>" }`, a `jobs` stream. Load, attach and run through
   `lemnos-ctl`; kill the caller and confirm its program is unloaded.
7. **Unbind while idle and while busy.** Unbind with no stream; then with the network running from
   another shell. Pass: both return, `dmesg` shows no wedge, and the module unloads.
8. **Back to the vendor driver.** Unload `lemnos.ko`, clear the override, bind `vipcore` back, and
   confirm the vendor's demo still runs.

Record each result here.
