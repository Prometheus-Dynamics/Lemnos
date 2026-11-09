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
| `test_stream_source` | Test only. Enables `LEMNOS_STREAM_IOC_TEST_SOURCE`, a source that fills frames through a kernel mapping. |

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
space. A real block's interrupt path will call the same frame-path functions.

Refusals and what they mean are listed in `lemnos.h`.

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
`test/lemnos_check.c`, `test/lemnos_irq_check.c`, `test/lemnos_unbind_check.c`, `test/lemnos_claim_check.c` and `test/lemnos_stream_check.c` on a block with
nothing behind it. CI runs this against the kernel `testing/qemu/build-kernel.sh` builds, with lock
and memory checking on.
