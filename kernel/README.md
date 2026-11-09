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

## Claims

`LEMNOS_IOC_CLAIM` takes a resource by name and returns a file. The claim lasts as long as that file:
a second claim on the name gets `EBUSY`, and closing the file, or the process dying, releases it. The
module doesn't interpret names. At most 4096 claims exist at once.

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
`test/lemnos_check.c`, `test/lemnos_irq_check.c`, `test/lemnos_unbind_check.c` and `test/lemnos_claim_check.c` on a block with
nothing behind it. CI runs this against the kernel `testing/qemu/build-kernel.sh` builds, with lock
and memory checking on.
