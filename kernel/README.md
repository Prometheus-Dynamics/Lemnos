# The Lemnos kernel module

`lemnos.ko` gives one process, through `/dev/lemnos`, what a driver in user space needs from a
platform block: register regions, DMA buffers as dma-bufs, dma-bufs mapped to device addresses, and
interrupts as reads. The interface is `lemnos.h`.

The node has one opener at a time. Closing it, however the process ended, masks every interrupt and
releases every import.

## Claiming a block

The module has no match table. It binds a device only when `driver_override` names it and one of
the device's compatibles is in the `owned` parameter. It never replaces or patches a stock driver.

```sh
insmod lemnos.ko owned=vendor,block
echo lemnos > /sys/bus/platform/devices/$DEVICE/driver_override
echo $DEVICE > /sys/bus/platform/drivers/lemnos/bind
```

An override stays set after an unbind, so clear it before reloading the module, or the device binds
as soon as the module loads.

## Parameters

| Parameter | Meaning |
| --- | --- |
| `owned` | Compatibles the module may claim, as comma-separated `vendor,name` pairs. Empty by default: nothing is claimed. |
| `confined` | Compatibles whose DMA their IOMMU really confines. Registers of any other block can't be mapped. Empty by default. |
| `test_map_unconfined` | Test only. Allows mapping the registers of a block not in `confined`. |
| `test_irq_inject` | Test only. Enables `LEMNOS_IOC_TEST_IRQ`. |
| `pisp_be` | Compatibles driven as the PiSP back end (`lemnos_pisp_be.c`), as comma-separated `vendor,name` pairs. Must also be in `owned`. Empty by default. |
| `test_pb_fake_regs` | Test only. The back end's register window is ordinary memory, for the fake block in QEMU. |
| `test_stream_source` | Test only. Enables `LEMNOS_STREAM_IOC_TEST_SOURCE`, a source that fills frames through a kernel mapping. |
| `test_pisp_fe` | Test only. Enables the PiSP front end's write log and post-copy overwrite ioctls. |

## Claims

`LEMNOS_IOC_CLAIM` takes a resource by name and returns a file. The claim lasts as long as that file:
a second claim on the name gets `EBUSY`, and closing the file, or the process dying, releases it. The
module doesn't interpret names. At most 4096 claims exist at once.

## Frame streams

`LEMNOS_IOC_STREAM_CREATE` returns a file for one stream of a block, with its pool of frame buffers
named by a dma-buf the module maps and pins for the block. `mmap` it for the ring, the same ring
the socket protocol's streams use (`docs/wire.md`, `lemnos.h`), with the module as its producer. The
consumer reads frames from the pool and posts jobs on the ring; the module publishes frames, marks
them and rings the eventfd given at creation (`poll` on the file also waits). Frame buffers and every
device-address range a job names are checked against what the module holds, with `check_add_overflow`
and a state for each buffer that lives only in the kernel. Anything else ends the stream with an
errno, and only that stream. Closing the file, or unbinding the block, stops the frame path and
releases every dma-buf it held; at most 64 streams exist at once, with up to 16 dma-bufs each.

A job names a buffer and up to four device-address ranges (`struct lemnos_stream_job`). A range must
lie wholly in that buffer's bytes, or wholly in a dma-buf registered on the stream with
`LEMNOS_STREAM_IOC_REGISTER`, for the full length. The test source (`test_stream_source`, test only)
fills frames from a kernel mapping and writes each range, so the checks are observable from user
space. A block's interrupt path calls the same frame-path functions (`lem_stream_next`,
`lem_stream_complete`).

Refusals and what they mean are listed in `lemnos.h`.

## The rp1-csi2 receiver

`lemnos_rp1_csi2.c` is the camera receiver: the block's D-PHY, CSI-2 host and one DMA channel,
delivering into a frame stream. It's a second platform driver in this module, `lemnos-rp1-csi2`,
bound the same way as a block (`driver_override`, and a compatible in `owned`). It has its own node
for each bound device, `/dev/lemnos-rp1-csi2-<device>`, one opener at a time; interface in
`lemnos_rp1_csi2.h`. Sensor bring-up is the sensor driver's: nothing here touches a sensor.

`LEMNOS_RP1_CSI2_IOC_START` names the link, the layout and a stream made on this node. There is no
ioctl carrying an address and no access to the registers. The receiver only arms buffers it gets
from `lem_stream_next()`: a device address computed in the stream code from the pool it mapped, the
end address checked against the buffer with overflow-safe arithmetic, the frame's stride, height and
size checked against the buffer at `START`, and the buffer held as the block's until the frame
completes or the channel is stopped.

Stopping is one path, whichever of `STOP`, closing the stream's file, closing this node or unbinding
gets there first. It writes FORCE, flushes it with a read, waits out any running handler, and then
polls the channel's line counter until it holds still, up to a bounded time. Only then do the buffers
go back. If the counter never holds still, the DMA can't be shown to have stopped: nothing is
released, every pin the stream holds is kept until reboot, and the receiver refuses `START` until it
is bound again (`LEMNOS_RP1_CSI2_INFO_WEDGED`).

Interrupt status bits are only tested, never trusted as counts or lengths: a frame's bytes on the
ring are what `START` fixed, and a frame the block didn't finish (a frame start with no frame end
before it) is flagged bad and, on a job-driven stream, comes back with 0 bytes.

GPL-2.0-only. The D-PHY bring-up, the channel's registers and the frame start and end accounting
are ported from the stock `rp1_cfe` driver, Copyright (C) 2021-2024 Raspberry Pi Ltd., through the
dev branch's receiver; the notice is kept in the file header. See `drivers/rp1-csi2/` for the driver
that uses this.

## The PiSP back end

A block listed in `pisp_be` is driven by the module: its registers can't be mapped and its
interrupts aren't forwarded. `LEMNOS_IOC_PISP_BE_CREATE` makes a frame stream whose jobs are
`struct lemnos_pisp_be_job` (`lemnos_pisp_be.h`), one stream at a time, with one configuration slot
per job slot, mapped shared at `config_mmap_offset`. It is a producer on the stream like the
receiver above, attached before the stream's fd exists, and reads its own jobs from the ring with
`lem_stream_job_peek()` and `lem_stream_job_take()`.

Each job and its configuration are read once from memory userspace can write, into memory it
can't. That copy is what's checked and what the block runs. Every address is proved against the
job's pool buffer or a registered dma-buf for the bytes the block touches, or the job is refused
and the stream ends; `lpb_validate` lists each address the configuration can name. Nothing is
clamped.

A job that doesn't finish within a second ends the stream. A block that can't then be shown idle,
or whose counters say more than its jobs explain, is wedged: its buffers and the stream's memory
are held until a reboot, `lemnos.ko` can't be unloaded, new streams are refused with `EIO`, and the
kernel log says so. Closing a stream or unbinding waits for what's on the block first.

`lemnos_pisp_be.c` keeps the stock driver's copyright for its register sequence and job
accounting; see its header.

## The PiSP front end

`lemnos_pisp_fe.c` drives the PiSP front end, the block's other half. It has no device of its own:
it shares the receiver's platform device, node and interrupt, and adds `LEMNOS_PISP_FE_IOC_*`
(`lemnos_pisp_fe.h`). A stream is made on the node as for the receiver and handed to `START` along
with the receiver's link and frame; the channel then feeds the front end instead of memory, so the
receiver's own `START` is refused with `EBUSY` while the front end runs.

A job is one frame on the stream's ring. Its pool buffer is output 0, and its ranges name, in order,
a configuration in a registered dma-buf, the statistics buffer when a statistics block is on, and
output 1 when it is on. The configuration is copied once, from a kernel mapping of the dma-buf, into
kernel memory; only that copy is checked and only that copy is written to the registers. Every
address field and `ilines` must be zero in it: the module fills them in from the frame's slot and
the job's ranges, which the stream resolved to its pinned dma-bufs. Sizes are the ones the block
will use, derived from the crop, downscale and format. An output line is 16 bits a sample, or a
compressed mode at 8 bits with its compress block on in that mode; every other format bit, and the
decompress block (`EOPNOTSUPP`; the input is always 16 bits), is refused. Statistics crops and grids
the block could lock up on are refused, and what's disabled inside a branch is blanked in the copy.
A refusal, of any kind, leaves the registers alone and ends the stream with its errno.

Sequence, from the stock driver: at `START` the first job is taken and checked before any register
is written. Then the channel in front end mode, the front end's reset and interrupts, the first job
and `QUEUE`, and the link last. Each frame's interrupt programs the next job when the block isn't
holding one. Stopping (`STOP`, closing the stream or node, unbinding) is one path: channel force,
interrupts off, `ABORT`, waits out any running handler, then `STATUS` must read zero eight times in
a row. If it doesn't within a bounded time, nothing is released, every pin stays until reboot, and
`START` is refused until the block is bound again (`LEMNOS_PISP_FE_INFO_WEDGED`).

GPL-2.0-only. Register offsets and the start, submit, interrupt and stop sequences are ported from
the stock `rp1_cfe` driver's `pisp-fe.c` and `cfe.c`, Copyright (C) 2021-2024 Raspberry Pi Ltd.; the
notice is kept in the file header. The driver is `drivers/pisp/` (`PispFe`, the `device` feature).

## Buses

I2C and SPI transfers go through the kernel's own cores. Every transfer names a claim on its target,
`i2c-<bus>:0x<addr>` or `spi-<bus>.<cs>`, and is refused without it. An I2C address a kernel driver
holds gets `EBUSY`. SPI devices are bound like blocks, through `driver_override` and `owned`. The
`bus_buffer` parameter sets the largest transfer, in bytes each way (default 4096).

## GPIO and PWM

Lines and channels go through gpiolib and the PWM core. Each belongs to a claim, `gpio-<chip>:<offset>`
or `pwm-<provider>:<channel>`, where the provider is the PWM chip's parent platform device. Closing
the claim lets go and leaves the output as it was. A line's edges arrive through `read()` and `poll()`
on the claim's file. Each carries a sequence number, so a gap shows a lost edge.

## Unbinding

Requests for a block fail with `ENODEV` from the moment its unbind starts, its register mappings
fault, and its imports are released. The unbind then waits until every DMA buffer made for the block
is closed, because a buffer can only be freed while the device still has its DMA setup. Killing the
holder ends the wait. Block numbers aren't reused while the module is loaded.

## Building and testing

```sh
make KDIR=<a built kernel tree>
```

`testing/qemu/run.sh` builds the module and boots a kernel in QEMU with it loaded, then runs
`test/lemnos_check.c`, `test/lemnos_irq_check.c`, `test/lemnos_unbind_check.c`, `test/lemnos_claim_check.c`, `test/lemnos_bus_check.c`, `test/lemnos_gpio_check.c`, `test/lemnos_stream_check.c`, `test/lemnos_rp1_csi2_check.c` and `test/lemnos_pisp_fe_check.c` on blocks with
nothing behind them. CI runs this against the kernel `testing/qemu/build-kernel.sh` builds, with lock
and memory checking on.
