# lemnos

The one Lemnos kernel module, `lemnos.ko` (GPL-2.0). It claims a platform block through
`driver_override` and hands it to a driver in user space. It is a fast path, never a requirement:
the module has no `of_match_table`, binds only where a device's `driver_override` names one of its
drivers after the stock driver is unbound, and never replaces or patches a stock driver. Stock
V4L2 stays the fallback.

It has three platform drivers:

| Driver | Node | For | uAPI |
| --- | --- | --- | --- |
| `lemnos` | `/dev/lemnos-<device>` | any block Lemnos owns: the generic core | `lemnos.h` |
| `lemnos-pisp-be` | `/dev/lemnos-pisp-be` | the Raspberry Pi 5 PiSP back end | `lemnos_pisp_be.h`, [pisp-be.md](pisp-be.md) |
| `lemnos-rp1-csi2` | `/dev/lemnos-rp1-csi2` | the RP1 CSI-2 receiver | `lemnos_rp1_csi2.h`, [rp1-csi2.md](rp1-csi2.md) |

The back end and the receiver are personalities: their DMA reaches any address a register names,
so the module checks their jobs and writes every address itself. Each keeps the driver name,
character device and ioctls it had as a module of its own, so `lemnos-drivers-pisp`'s
`be-module` and `csi2-module` work unchanged. Their test parameters are now `lemnos.*`
(`lemnos.test_unmap_plane`, `lemnos.test_raw_enables`).

## The generic core (`lemnos.h`)

- **`LEMNOS_IOC_INFO`**: the ABI, register regions, interrupts, DMA address width, and whether
  the registers may be mapped.
- **`LEMNOS_IOC_REGION`**: a register region's physical range.
- **Register mapping.** `mmap()` at offset `index * page size` maps region `index`. It is allowed
  only when the block's DMA cannot reach memory the module did not map (behind an IOMMU, and not
  in the module's unconfined list), and only for `CAP_SYS_RAWIO`. The back end and the receiver
  are in the unconfined list.
- **`LEMNOS_IOC_ALLOC`**: a coherent DMA buffer, exported as a dma-buf (close-on-exec), with its
  device address. Map the dma-buf for the CPU.
- **`LEMNOS_IOC_IMPORT` / `LEMNOS_IOC_RELEASE`**: a dma-buf mapped on the block, contiguous in its
  address space or refused, held until released or the file closes.
- **Interrupts**: `read()` returns a `struct lemnos_irq_event` per interrupt that fired, and
  `poll()` waits for one. An interrupt is masked from when it fires until
  `LEMNOS_IOC_IRQ_ENABLE`: the driver clears the cause, then enables it. All interrupts start
  masked and are masked again at close. Interrupts are not shared.
- **One open file at a time.** A second open is refused with `EBUSY`.

**Only blocks Lemnos owns** ([docs/driver-ownership.md](../../docs/driver-ownership.md)). The
`owned` parameter lists their compatibles as comma-separated pairs, by default
`raspberrypi,pispbe,raspberrypi,rp1-cfe`. The generic driver refuses any other device with
`ENODEV`. Never point `driver_override` at a block whose stock driver a stock-path consumer
relies on, and keep the module out of those images (see [../README.md](../README.md)).

## Build

Against the configured and built kernel tree of the image, with its toolchain:

```sh
make KDIR=<the image's linux build tree> ARCH=arm64 CROSS_COMPILE=<its toolchain prefix>
```

With `W=1` it builds with no warnings against the board's 7.2.9-v8-16k tree.

`make ... INSTALL_MOD_PATH=<root> modules_install` installs it stripped (`INSTALL_MOD_STRIP=1`
is the Makefile's default).
- Shipped module: 59 416 B. That tree has no debug information, so stripping saves only 704 B.
- Of the file, 11 856 B is relocations and 6 464 B modversions CRCs, both needed to load it.
- Loaded: text 30 329 B, data 1 868 B, bss 16 B.

Two test programs (build them with the image's C compiler):
- `test/lemnos_check.c` exercises the generic core on a bound block and leaves its registers
  alone.
- `test/lemnos_irq_check.c` exercises the two test-only paths. Load the module with
  `test_map_unconfined=1 test_irq_inject=1` for it.

## Test parameters

Off by default and settable only at load. Each logs a warning when set.

- `test_map_unconfined=1`: allows mapping the registers of a block whose DMA is not confined.
- `test_irq_inject=1`: enables `LEMNOS_IOC_TEST_IRQ`, which sets an interrupt pending at its
  controller (`irq_set_irqchip_state`). The forwarding path runs without the block raising
  anything. Without the parameter the ioctl is refused with `EPERM`.

## Load and bind (generic)

As root, with no user of the stock driver running:

```sh
D=<device>                     # e.g. 1000880000.pisp_be
echo $D > /sys/bus/platform/drivers/<stock driver>/unbind
insmod lemnos.ko
echo lemnos > /sys/bus/platform/devices/$D/driver_override
echo $D > /sys/bus/platform/drivers/lemnos/bind
# and back:
echo $D > /sys/bus/platform/drivers/lemnos/unbind
echo > /sys/bus/platform/devices/$D/driver_override
echo $D > /sys/bus/platform/drivers/<stock driver>/bind
```

## Measured on a board (board #1, 2026-11-06, 7.2.9-v8-16k)

**The generic core on the PiSP back end's node.** `lemnos_check` passed 20 of 20:
- 1 region (0x1000880000, 16 KiB), 1 interrupt, 36-bit DMA, registers not mappable (`EPERM`);
- a 1 MiB buffer at device address 0xaffe00000, written and read back through its dma-buf;
- its import is a second IOMMU mapping (0xaffd00000), same size;
- a 4 MiB `linux,cma` heap buffer imported (0xaff800000) and released at close;
- interrupt 0 enabled on the idle block raised nothing;
- `ENOENT`, `EINVAL`, `EAGAIN` and `EBUSY` where expected.

Before `owned` named the right compatible, the node (`raspberrypi,pispbe`) was refused with
`ENODEV`. On a normal load `LEMNOS_IOC_TEST_IRQ` is refused with `EPERM`. `pispbe` and the camera
service were restored after each run.

**The test-only paths on the same node** (`test_map_unconfined=1 test_irq_inject=1`).
`lemnos_irq_check` passed every check.
- **Registers:** region 0 mapped read-only reads 0x02252701, the back end's version register.
- **Interrupts**, injected at the GIC (GICv2 SPI 104, level):
  - an enabled one reaches `read()`;
  - one injected while masked is not delivered;
  - on enable it is not resent either: genirq resends edge interrupts only, since a real level
    device still holds its line and fires again;
  - the next one after enable arrives.
- **Latency**, 2000 rounds under `SCHED_FIFO` 10: 2000 of 2000 read, 0 lost.

| Run | Injection to handler p50 / p99 / max | Injection to user space p50 / p99 / max |
| --- | --- | --- |
| 1 | 0.7 / 0.8 / 4.6 µs | 1.5 / 3.3 / 9.0 µs |
| 2 | 0.6 / 0.8 / 14.8 µs | 1.5 / 3.4 / 16.9 µs |
| 3 | 0.6 / 0.8 / 6.0 µs | 1.4 / 3.4 / 8.0 µs |

**The personalities against the modules they replace.** CSI-2 receiver to PiSP back end
(`csi2_module_check --be --bars`): OV9782 at 1280x800 RAW10, 120 fps, 9000 frames per run. The
runs were interleaved old, new, new, old, each pair under one hold of the board lock.

| Run | Frames | fps | Frame starts without a buffer | FS to FE median | Frame end to BE interrupt p50 / p99 / max | To wake-up p50 / p99 / max |
| --- | --- | --- | --- | --- | --- | --- |
| old modules | 9000, 0 failed | 120.63 | 0 | 7556 µs | 755 / 962 / 3337 µs | 757 / 969 / 3363 µs |
| `lemnos.ko` | 9000, 0 failed | 120.62 | 0 | 7556 µs | 755 / 942 / 3315 µs | 757 / 951 / 3393 µs |
| `lemnos.ko` | 9000, 0 failed | 120.62 | 0 | 7556 µs | 754 / 962 / 3079 µs | 757 / 969 / 3103 µs |
| old modules | 9000, 0 failed | 120.62 | 0 | 7556 µs | 753 / 923 / 3059 µs | 756 / 934 / 3110 µs |

Every run had exact colour bars at the receiver and the back end. The two builds are the same
within run-to-run noise. The module is 58 264 B unstripped; the two it replaces were 27 248 B and
23 184 B.
