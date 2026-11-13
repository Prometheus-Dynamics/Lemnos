# The kernel module

`lemnos.ko` is optional. Without it the daemon reaches hardware through the kernel's stock
interfaces (`i2c-dev`, `spidev`, the GPIO character device, sysfs PWM, `usbfs`, V4L2). With it, the
daemon gets what user space cannot do safely by itself.

This page says what the module is for and how to use it. [`kernel/README.md`](../kernel/README.md)
is the reference for every part, and [`kernel/lemnos.h`](../kernel/lemnos.h) is the interface.

## What it gives

Through one node, `/dev/lemnos`, with one opener at a time:

- **Claims the kernel enforces.** A resource is taken by name and held by a file. A second claim on
  the name gets `EBUSY`. The claim ends when the file closes, however its holder ended, so a killed
  daemon leaves nothing held.
- **Buses through the kernel's own cores.** I2C and SPI transfer lists, GPIO lines with their
  edges, and PWM channels, each under a claim.
- **DMA memory for a block**, as dma-bufs, and dma-bufs mapped to a block's device addresses.
- **Register regions and interrupts of a block**, for a driver in user space. Registers can be
  mapped only for a block whose IOMMU really confines its DMA, or one the module is told to map
  though nothing confines it (`unconfined`, below).
- **Clocks, supplies and resets** of a block, by the names its firmware description gives them.
- **One event stream**: interrupts, blocks bound and unbound, supply faults, temperature limits.
- **Frame streams** with the module as the producer, for blocks whose DMA must be checked in the
  kernel. Every address a job names is checked against memory the module has pinned.
- **Five blocks driven inside the module**, because their DMA cannot be left to user space: the
  CSI-2 receiver and both halves of the ISP on the BCM2712 with RP1, the K230's graphics DMA
  engine, which takes bus addresses with no MMU in front of it, and the VIP9000 neural
  accelerator, whose own MMU the module keeps, giving each job tables that map only that job's
  memory.
- **Register jobs** for blocks programmed by writing registers that take bus addresses: the
  caller gives the values and its dma-bufs, and the module writes every address itself, refuses a
  range shorter than what the job's own geometry makes the block reach, runs the job and resets
  the block if it hangs. A block that runs continuously, like an ISP writing every frame, switches
  to a job's buffers at a frame's end, in the module's interrupt handler. A second session in a
  configuration role can write the same block's other registers and take its events while frames
  run, with the registers both write merged bit by bit under the module's lock. The A733 video
  engine's encoder and the K230 ISP run this way.
- **PIO state machines** on the RP1, through a table of allowed operations.

## What the daemon uses today

Loaded, the module is opened by `lemnosd` at start. The Linux backend then takes these through it,
and everything else stays on the stock path:

| Board-file name | Through the module |
|---|---|
| `i2c-N` | Yes, when the adapter exists |
| `spidevB.C` | Yes, when that SPI device is bound to the module |
| `gpiochipN:LINE` | Yes, when the module can take the line |
| `pwmchipN:CHANNEL` | Yes, when the module can take the channel |
| `pio-i2c:sda=N,scl=N` | Yes, when the module was built with PIO: a claim on `pio-0`. The stock PIO node must exist either way |
| `dma:BLOCK` | Only through the module. There is no stock path |
| `regs:BLOCK/N` | Only through the module, and only when it says the block's registers may be mapped |
| `irq:BLOCK/N` | Only through the module |
| `clocks:BLOCK/NAME,...`, `resets:BLOCK/NAME,...` | Only through the module |

The receiver and ISP drivers (`rp1-csi2`, `pisp-be`, `pisp-fe`), the graphics DMA library
(`k230-gsdma`), `a733-video` with `route = "module"` and the `vip9000` driver open the module's
nodes themselves. The K230 ISP's drivers (`k230isp`, `k230-capture`, `k230isp-frames`) reach
the ISP through its register jobs, when the board file gives them the block (see
[Drivers and contracts](drivers.md#which-drivers-take-a-blocks-own-resources)).

Interrupts reach the backend on the module's one event stream, which it reads for every interrupt
a process holds and files each event under its own. Supplies, thermal watches and PIO are complete
in the module and tested in QEMU; the driver interface has no supply or thermal resource, so the
backend hands those two to no driver.

## The module's own streams

A stream the module makes (`LEMNOS_STREAM_*`) has its own ring. By default it is the module's own
layout (`LEMNOS_RING_*`), which the four sys crates read; the driver that runs the block reads it,
with its fences and job ranges. With `LEMNOS_STREAM_CREATE_SHM` the frame ring is the shared-memory
ring of [`shm/README.md`](../shm/README.md) instead: one field, one sample per frame, each sample's
entry naming its buffer and ticket, and the pool page after the ring. A reader maps it read-only and
reads it with the shm crates alone (`testing/crates/shm-ring-check` does, in the QEMU harness). In
this form a buffer is not returned through a words queue: a consumer gives the module its marks page
(`shm-ring-holds`) with `LEMNOS_STREAM_IOC_READER`, and the module pins that page read-only. A buffer
goes back to the frame path only when no reader marks it, and the module checks that when it needs the
buffer, so a held buffer is never reused. `LEMNOS_STREAM_IOC_READER` also returns `reader_fd`, a file that
holds the registration: when it closes (a reader that exits closes it), the reader's slot is freed and its
marks stop holding at that moment, so the next frame that needs the buffer can take it. `LEMNOS_READER_IOC_UNREGISTER`
on that file does the same at once, and a second one fails with `ENOENT`. The table has 64 slots, so a
reader must not leak its registration. `testing/crates/shm-holds-check` `release` proves the release
on the pinned kernel: a reader that holds and exits without unregistering has its buffer reused and its
slot free, 200 arrive-and-leave cycles all register, and the release hook disabled makes it fail. The jobs words still follow the old layout in this form. [`docs/streams.md`](streams.md#streams-made-by-the-kernel-module) says how it fits,
and [`TODO.md`](../TODO.md) lists the port.

## What it never does

- It has no match table. It binds nothing unless told to, device by device.
- It never replaces or patches a stock driver.
- It never takes an address from user space on trust. A job names a buffer and a range, and the
  module resolves and checks them against memory it has pinned. The receiver has no request that
  carries an address at all.
- It never clamps a bad request into a good one. A request that fails a check is refused, and a
  stream that is given one ends with an error. Only that stream ends.
- It never interprets a claim's name.
- It never touches a sensor, the GPU, USB, the network or storage.
- It never frees memory a block may still be writing. If a block's DMA cannot be shown to have
  stopped, the module keeps every pin until reboot and refuses to start that block again.

## Building

The module builds out of tree against a built kernel tree. Building a copy keeps the source tree
clean:

```sh slow
cp -r kernel "$TMPDIR/kernel"
make -C "$TMPDIR/kernel" KDIR="/lib/modules/$(uname -r)/build"
ls "$TMPDIR/kernel/lemnos.ko"
```

For a board, give `KDIR` the board kernel's build tree, and `ARCH` and `CROSS_COMPILE` when
building on another machine. `make modules_install` installs it stripped. The PIO part is built
only for a kernel that has the RP1 PIO driver.

### For an image builder

What an image builder needs to build `lemnos.ko` against a board kernel's build tree (`KDIR`):

- **Invocation.** The module's `Makefile` wraps the kernel's module build. Run it from a copy of
  `kernel/` or from the checkout, with the board kernel's built tree:

  ```text
  make -C kernel KDIR=/path/to/board-kernel-build ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
  ```

  `ARCH` and `CROSS_COMPILE` are passed to the kernel's build system. Use the values that tree was
  built with: `ARCH=arm64` (or `arm`, `riscv`), and the cross prefix of its toolchain. The tree must
  be a built one, with `Module.symvers`, `.config` and `include/generated`. Its `.config` decides
  the Kconfig rules below.
- **Output.** `lemnos.ko`, in the directory the make ran in. `make modules_install` installs it
  stripped (`INSTALL_MOD_STRIP=1`, the default).
- **Make variables.** `KDIR` (required, the built tree; defaults to this machine's headers).
  `ARCH`, `CROSS_COMPILE`, `INSTALL_MOD_STRIP` (default 1), and `CONFIG_RP1_PIO` (set to `m` or `y`
  to build the RP1 PIO part, see below). `KBUILD_EXTRA_SYMBOLS` names the `Module.symvers` of an
  out-of-tree module the PIO part links against, when the board kernel builds the RP1 PIO driver
  as a module. `LEMNOS_EXTRA_CFLAGS` is for the test harness only; leave it unset.
- **Warnings.** The module is built with `-Werror`. A kernel that warns about the module's code
  fails the build.

Kconfig the board kernel must have is in [Kernel configuration](kernel-config.md): every symbol the
module links against with the option that provides it, the stock interfaces the daemon uses
without the module (I2C, SPI, GPIO, PWM, USB, V4L2, dma-heaps, the slot path and the rest), and the
SoC driver for each. Its `lemnos.ko` table is checked against the sources by
`scripts/check-kernel-config.py`, which the gate runs.

A kernel with no loadable modules needs the module built in: `CONFIG_LEMNOS=y` with the kernel tree
described in that page, not `make -C kernel`.

Module parameters (`modinfo lemnos.ko`; set at load with `insmod lemnos.ko name=value` or from the
image's modprobe.d). All are read-only at runtime (mode 0444). Empty means none:

- `owned=` (compatibles the module may claim; required for any binding)
- `confined=` (compatibles whose IOMMU really confines their DMA)
- `unconfined=` (compatibles whose registers may be mapped with no IOMMU confinement)
- `pisp_be=`, `gsdma=`, `vip9000=`, `a733_ve=`, `k230_isp=` (compatibles driven by the module, each
  also listed in `owned=`)
- `bus_buffer=` (largest bus transfer, bytes each way; default 4096)
- Test only, never set on a board: `test_irq_inject`, `test_pb_fake_regs`, `test_stream_source`,
  `test_pisp_fe`, `test_gsdma_fake_regs`, `test_gsdma_reach`, `test_rj_fake_regs`,
  `test_vip_fake_regs`, `test_vip_timeout_ms`.

Verified here: a native build against this machine's kernel tree (`7.2.9-200.nobara.fc44.x86_64`)
with `make -C <copy of kernel> KDIR=/lib/modules/$(uname -r)/build modules` exits 0 and produces
`lemnos.ko` (GPL, the parameter list above), and it still does after the in-tree switch was added.
A cross build, the in-tree build and a board kernel have not been built here: they are untested.

## Loading and claiming

The module claims a platform device only when two things agree: the module was loaded with the
device's compatible in `owned`, and the device's `driver_override` names `lemnos`.

```sh needs:root-and-a-board
insmod lemnos.ko owned=vendor,block
echo lemnos > /sys/bus/platform/devices/$DEVICE/driver_override
echo $DEVICE > /sys/bus/platform/drivers/lemnos/bind
```

`owned` is a list of `vendor,name` pairs separated by commas. If a stock driver holds the device,
unbind it from that driver first. An override stays set after an unbind, so clear it before
loading the module again, or the device binds as soon as the module loads.

Other parameters:

| Parameter | Meaning |
|---|---|
| `confined` | Compatibles whose IOMMU really confines their DMA. Their registers can be mapped |
| `unconfined` | Compatibles whose registers can be mapped though no IOMMU confines their DMA. The driver in user space alone then keeps that DMA inside the block's memory, so list only a block whose driver checks every register it writes and points every address register only at memory the module allocated for that block, and whose stock driver already gives user space the same access. The kernel log says so when such a block binds |
| `pisp_be` | Compatibles driven as the ISP back end. Must also be in `owned` |
| `gsdma` | Compatibles driven as the graphics DMA engine. Must also be in `owned` |
| `a733_ve` | Compatibles driven as the A733 video engine's encoder through register jobs. Must also be in `owned` |
| `vip9000` | Compatibles driven as the VIP9000 neural accelerator. Must also be in `owned` |
| `k230_isp` | Compatibles driven as the K230 ISP through register jobs: frames in one session, configuration and statistics in another beside it. Must also be in `owned` |
| `bus_buffer` | The largest I2C or SPI transfer, in bytes each way. 4096 by default |

The parameters whose names start with `test_` are for the QEMU harness. Leave them unset.

With no parameters the module claims no block. Loading it still gives the daemon kernel-enforced
claims on I2C, GPIO and PWM.

## Claim names

A claim is a name. The module refuses a bus request that does not come with the claim on its
target:

| Claim | Covers |
|---|---|
| `i2c-<bus>:0x<addr>` | One address on one I2C adapter. An address a kernel driver holds is refused |
| `spi-<bus>.<cs>` | One SPI device |
| `gpio-<chip>:<offset>` | One GPIO line. Closing the claim leaves the output as it was |
| `pwm-<provider>:<channel>` | One PWM channel, by the chip's parent platform device |
| `pio-0` | The PIO block |

At most 4096 claims exist at once. The Linux backend builds these names from the board file's
names. A board file never spells them.

## Unbinding and unloading

Requests for a block fail with `ENODEV` from the moment its unbind starts, and its register
mappings fault. The unbind waits until every DMA buffer made for the block is closed, so stop the
daemon first. A block that was wedged (its DMA could not be shown to stop) keeps the module loaded
until reboot, and the kernel log says so.

## Testing

The module is tested in a virtual machine, on blocks with nothing behind them, with the kernel's
lock and memory checking on. See [Testing](testing.md#the-qemu-harness). It has not been loaded on
a board in this form: see [Hardware status](hardware-status.md).

The module is GPL-2.0-only. The rest of the repository is MIT or Apache-2.0.
