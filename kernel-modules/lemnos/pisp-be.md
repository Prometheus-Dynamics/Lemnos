# lemnos-pisp-be (a personality of `lemnos.ko`)

The Raspberry Pi 5 PiSP back end as a plain character device, `/dev/lemnos-pisp-be`. A fast path
beside the stock `pispbe` V4L2 driver, never a requirement: the module has no `of_match_table`,
so it binds only where the device's `driver_override` names it, after `pispbe` is unbound. Its
user-space side is `lemnos-drivers-pisp` with the `be-module` feature
(`device::be_module::BeModule`), over `lemnos_linux_sys::lemnos_pisp_be`.

GPL-2.0. The register sequence, the interrupt's job accounting and the probe check are ported
from the stock driver (`pisp_be.c`, copyright Raspberry Pi Ltd, notice kept in the source). The
uAPI header `lemnos_pisp_be.h` is `GPL-2.0-only WITH Linux-syscall-note`. The reasoning is in
`docs/kernel-module.md`.

## What it does

- **A job** is a `struct pisp_be_tiles_config` (uAPI `pisp_be_config.h`) and one dma-buf plane
  per back end address (13: input, TDN and stitch inputs and outputs, two image outputs of three
  planes each), each with a 16-byte-aligned offset. `LEMNOS_PISP_BE_IOC_SUBMIT` copies the
  config into the module's own coherent memory and checks it as `pispbe` does: exactly one input,
  1 to 64 tiles, each used plane present and large enough for its format, every tile offset inside
  its plane. Wallpaper formats and negative strides are refused. The enables are sanitised as
  `pispbe_xlate_addrs` does: a stage without its buffer is turned off, and with no Bayer input
  the whole Bayer pipe is off.
- **Every address is the module's.** The block's IOMMU translates only its 40-44 GiB aperture;
  an address below it reaches DRAM untranslated. So user space never gives an address: the
  module maps each dma-buf on the block's device and writes the address itself.
- **Mappings are kept between jobs**, 16 buffers per open file, as vb2 keeps an imported buffer.
  A job on buffers already seen maps nothing; the least recently used idle buffer is unmapped
  when a new one needs its place, and all are unmapped at close. Cache maintenance stays with
  the caller's dma-buf sync ioctls (`DMA_BUF_IOCTL_SYNC`) around CPU access, as for any importer.
  A buffer is never unmapped while a job holds it.
- **Up to 4 jobs** are submitted and not yet read; the block holds two of them at a time. A
  finished job is a `struct lemnos_pisp_be_done`, read from the descriptor (`poll` for
  `POLLIN`). It carries the `ktime_get_ns()` of the interrupt that ended the job.
- **A stopped block.** A job unfinished after 1 s ends with `-ETIMEDOUT` and the block is marked
  dead: nothing more is queued, and `SUBMIT` fails with `EIO`. The stopped jobs' buffers stay
  mapped (leaked) because the block may still hold their addresses. Only a reboot clears it
  (below).
- One open file at a time.

## Build

Against the configured and built kernel tree of the image, with its toolchain:

The back end is a personality of `lemnos.ko`; build it as [README.md](README.md) says:

```sh
make KDIR=<the image's linux build tree> ARCH=arm64 CROSS_COMPILE=<its toolchain prefix>
```

The tree must be the one that built the running kernel: with `MODVERSIONS`, a tree of the same
release but another config is refused at load (`struct module` size). `W=1` builds without
warnings.

## Load and bind

As root, with no `pispbe` user running (the bind takes the block from it):

```sh
D=1000880000.pisp_be
echo $D > /sys/bus/platform/drivers/pispbe/unbind
echo lemnos-pisp-be > /sys/bus/platform/devices/$D/driver_override
insmod lemnos.ko    # binds, the override naming this personality
# and back:
rmmod lemnos        # or unbind it, if another block is on lemnos too
echo > /sys/bus/platform/devices/$D/driver_override
echo $D > /sys/bus/platform/drivers/pispbe/bind
```

## Test parameters

Both are for the board tests only and are off by default.

- `test_unmap_plane=N`: unmap plane `N` right after queueing each job (the IOMMU test).
- `test_raw_enables=1`: queue the caller's enables unsanitised.

## Measured on a board

Board #1 (BCM2712, kernel 7.2.9-v8-16k, 4 KiB pages), 2026-11-06, with
`lemnos-drivers-pisp`'s `be_module_check`: NV12 640x480 downscaled by 2 to GREY on output 1,
CMA-heap buffers, one job at a time.

| path | p50 | p99 | max | jobs |
|---|---|---|---|---|
| stock `pispbe` nodes, queue to dequeue | 505 µs | 1237 µs | 5351 µs | 2000 |
| the same, `SCHED_FIFO` 50 | 505 µs | 1162 µs | 2733 µs | 2000 |
| module, submit to wake-up | 510 µs | 1062 µs | 1794 µs | 2000 |
| the same, `SCHED_FIFO` 50 | 511 µs | 1056 µs | 2077 µs | 10000 |
| module, interrupt to wake-up | 2 µs | 30 µs | 411 µs | 2000 |
| the same, `SCHED_FIFO` 50 | 2 µs | 20 µs | 76 µs | 10000 |

- Every run's output matched the CPU's 2x2 mean (truncated, within 1) on every sample.
- The round trip is the block's own time; the module matches `pispbe` there. A first version
  that mapped and unmapped every buffer per job took p50 560 µs (submit to wake-up) and p50
  29 µs (interrupt to wake-up): the mapping cache is what closes the gap.
- **IOMMU violation:** no fault and no stop. The bcm2712 IOMMU has no fault reporting; an
  unmapped page translates to its "default" page. With the input unmapped while its job ran,
  the job finished with status 0 and 71456 of 76800 output samples wrong; with the output
  unmapped, 74568 of 76800 samples were never written. The block's status stayed 0 and it kept
  working. So a buffer freed under a running job fails silently, and an address reused by
  another mapping would be written: the module never unmaps a buffer a job holds.
- **A stopped block and the two soft recoveries:** the TDN and stitch inputs enabled on an RGB
  input (enables 0x5b2e5c, with `test_raw_enables`) stopped the block: status 0x2, started one
  ahead of done. The watchdog ended the job at 1.0 s. Rewriting the AXI register with the
  interrupts cleared left status 0x2, and so did halving the ISP clock's rate and restoring it.
  Neither revived the block; `pispbe` then refused to probe ("HW is stuck or busy") until a
  reboot. Two smaller sets (0x000404, 0x5b060c, Bayer stages with no address of their own) did
  not stop it.

Untested: the Bayer path, TDN and stitch buffers, two outputs in one job, several jobs in
flight, and the 6.12 kernels.
