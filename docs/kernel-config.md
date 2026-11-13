# Kernel configuration

What a board kernel must have for Lemnos, for an image builder. It covers two things: `lemnos.ko`
(or the same code built into the kernel), and the stock interfaces the daemon uses when the module
is absent. [The kernel module](kernel-module.md) says what the module does and how to build it.

Every value below was read from the kernel sources of the reference SoC's tree (7.2, arm64), or
from its `.config`, or from mainline where the row says so. Nothing here was loaded on a board
(see [Hardware status](hardware-status.md)).

## Built in, for an image with no modules

An image with no loadable modules needs every symbol it uses at `=y`. Two routes build `lemnos.ko`:

- **Out of tree**, as a module, the route that already works: `make -C kernel KDIR=<board tree>`
  (see [The kernel module](kernel-module.md#building)). This needs `CONFIG_MODULES=y`, and it
  cannot be used for an image without modules.
- **In tree**, built into the kernel. `kernel/Kconfig` and the `Kbuild` in that directory are the
  pieces. In the board kernel's tree:

  ```text
  cp -r kernel drivers/misc/lemnos                    # the module's sources, unchanged
  # drivers/misc/Kconfig:   source "drivers/misc/lemnos/Kconfig"
  # drivers/misc/Makefile:  obj-$(CONFIG_LEMNOS) += lemnos/
  ```

  Then set `CONFIG_LEMNOS=y`, and `CONFIG_RP1_PIO=y` if the RP1 PIO part is wanted. A built-in
  `lemnos.o` cannot link to a module's symbols, so `CONFIG_RP1_PIO` must be `y` too when lemnos
  is built in. `Kbuild` takes the module route when `KBUILD_EXTMOD` is set (out of tree) and the
  `CONFIG_LEMNOS` route otherwise.

  Status: the out-of-tree build produces `lemnos.ko`, run here. The in-tree route is built and
  booted in QEMU (below), **untested on board**.

### The kernel with no loadable modules, checked in QEMU

This is the configuration a flasher image depends on: module support off, and `lemnos` built into
the kernel. It is built and booted in CI (job `qemu-modules-off` in
`.github/workflows/next-kernel.yml`, on every change), and by hand with these commands:

```text
testing/qemu/build-kernel.sh <dir> --modules-off   # CONFIG_MODULES unset, CONFIG_LEMNOS=y
KERNEL=<dir>/build/arch/x86/boot/bzImage CC="zig cc -target x86_64-linux-musl" testing/qemu/run-builtin.sh
```

`build-kernel.sh --modules-off` uses `testing/qemu/kernel-modules-off.config`. It copies `kernel/`
into the tree as `drivers/misc/lemnos`, adds the Kconfig source and the `obj-$(CONFIG_LEMNOS)` line
(the two lines an image builder adds), and builds only `bzImage`: there are no modules to build.
The copy is made each time, so with a warm cache only `kernel/` recompiles.

`run-builtin.sh` boots that image three times, each with a different command line, and each boot
must pass the guest's checks (`testing/qemu/init-builtin.c`):

| Boot | Command line (`lemnos.` parameters) |
|---|---|
| 1 | `owned=lemnos,test-block,lemnos,test-spi bus_buffer=8192` |
| 2 | `owned=lemnos,test-csi2 confined=lemnos,test-csi2 unconfined=lemnos,test-vip bus_buffer=4096` |
| 3 | none: every parameter takes its default |

The guest checks: the module is built in (`/sys/module/lemnos`), no `lemnos.ko` is in the image,
`init_module` answers ENOSYS (no module support), the control node exists at boot before any
daemon, every parameter reads back what its command line gave (or its default), and
`lemnos_claim_check` passes. Result, KVM, the three boots:
`lemnos-builtin: passed (0 failed)` three times, then `qemu: built-in passed on kvm, 3 parameter sets`.

What this proves: the module builds into a kernel with no modules, initialises at boot, takes its
parameters from the command line, and gives its control node and claims. It does not prove
anything that needs a device: no fake block is in this kernel, so nothing here reaches a bus,
a stream, a DMA engine or an interrupt. Those are covered only by the guest suite with the
modules-on kernel (`testing/qemu/run.sh`), which loads `lemnos.ko` with parameters at load time.

### Parameters from the command line

A module built into the kernel takes its parameters as `lemnos.<name>=<value>` on the kernel
command line. `lemnos-ctl tree BOARD --cmdline FILE` writes them as one line of such words,
taken from the `options lemnos` line of the modprobe fragment (`--modprobe FILE`), so the two
carry the same values. The `blacklist` lines of the fragment have no command-line form: a built-in
stock driver is not stopped by them, and the daemon reports it at start.

The kernel's command line is at most 2048 bytes. The longest parameter values the generator
writes are the compatible lists (`owned=`, `confined=`, `unconfined=`), so a board that owns many
blocks can exceed the limit. Then a built-in module cannot take the whole configuration from the
command line, and the image must keep `lemnos` as a loadable module (`lemnos.ko`, with the modprobe
fragment) for that board. For the shipped board files, `service/ctl/tests/kernel_cmdline.rs` checks
that the line is at most 2048 bytes; `lemnos-ctl` itself does not.

`bus_buffer` and the `test_*` flags are read at start like the rest. The `test_*` flags are for
the harness only.

## Symbols of lemnos.ko

The symbol list is from `nm -u` on `lemnos.ko`, built natively against an x86_64 kernel tree. Each
undefined symbol was matched to its exporting file in the reference tree and to the Makefile line
that builds that file, so each row is the option whose `=y` makes the symbol exist. The x86-only
symbols (`boot_cpu_data`, `cachemode2protval`, `cc_mkdec`, `__x86_*`, `__fentry__`, `__ref_stack_chk_guard`)
do not apply to arm64 and are left out. `scripts/check-kernel-config.py` checks this table against
the sources and the QEMU harness's configuration.

| Symbol | Value | Requirement | Needed for |
| --- | --- | --- | --- |
| `CONFIG_MODULES` | `y` | optional | Building and loading `lemnos.ko` as a module. Not needed for the in-tree route |
| `CONFIG_PM` | `y` | required | `devm_pm_runtime_enable`, `__pm_runtime_idle`, `__pm_runtime_resume` (`lemnos_vip.c`, `lemnos_power.c`) |
| `CONFIG_OF` | `y` | required | Every block is a device tree node: CSI-2 endpoint lookup (`of_graph_get_endpoint_by_regs`, `lemnos_rp1_csi2.c`) and PWM xlate (`of_pwm_xlate_with_flags`, `lemnos_pwm.c`) |
| `CONFIG_I2C` | `y` | required | `i2c_transfer`, `i2c_smbus_xfer`, `i2c_get_adapter` (`lemnos_bus.c`) |
| `CONFIG_SPI` | `y` | required | `spi_sync`, `spi_setup`, `__spi_register_driver` (`lemnos_bus.c`). `SPI_MASTER` follows from it (`default SPI`) |
| `CONFIG_GPIOLIB` | `y` | required | `gpiod_*`, `gpio_device_*` (`lemnos_gpio.c`), and `gpio_request` and `gpio_free`, which are in `gpiolib-legacy.c`; `GPIOLIB_LEGACY` is `def_bool y` in this tree |
| `CONFIG_PWM` | `y` | required | `pwm_apply_might_sleep`, `pwm_put`, `of_pwm_xlate_with_flags` (`lemnos_pwm.c`) |
| `CONFIG_REGULATOR` | `y` | required | `regulator_*` (`lemnos_power.c`) |
| `CONFIG_THERMAL` | `y` | required | `thermal_zone_get_temp`, `thermal_zone_get_zone_by_name` (`lemnos_events.c`) |
| `CONFIG_RESET_CONTROLLER` | `y` | required | `reset_control_*`, `__devm_reset_control_get` (`lemnos_power.c`, register jobs, VIP) |
| `CONFIG_COMMON_CLK` | `y` | required | `clk_prepare`, `clk_unprepare`, `clk_bulk_*`, `devm_clk_*` (`lemnos_power.c`, register jobs, CSI-2). `HAVE_CLK` is selected by it |
| `CONFIG_DMA_SHARED_BUFFER` | `y` | required | `dma_buf_*`: every stream and every register job (`lemnos_stream.c`, `lemnos_regjob.c`) |
| `CONFIG_EVENTFD` | `y` | required | `eventfd_ctx_fdget`, `eventfd_ctx_put`, `eventfd_signal_mask` (stream completion) |
| `CONFIG_CPU_BIG_ENDIAN` | unset | must be unset | `BUILD_BUG_ON(IS_ENABLED(CONFIG_CPU_BIG_ENDIAN))` in `lemnos_stream.c`. The module builds only for little-endian kernels |
| `CONFIG_LEMNOS` | `y` or `m` | optional | The in-tree switch in `kernel/Kbuild`; set by the image builder for the built-in route. Not in the out-of-tree build |
| `CONFIG_RP1_PIO` | `m` or `y` | optional | The RP1 PIO part (`lemnos_pio.c`, claim `pio-0`), compiled when set. Needs `<linux/pio_rp1.h>` from the board kernel. Must be `y` when lemnos is built in |
| `CONFIG_IOMMU_SUPPORT` | `y` | optional | `device_iommu_mapped()` in `lemnos_core.c`: without an IOMMU the `confined=` list maps nothing, and only `unconfined=` blocks can be mapped |
| `CONFIG_CMA` | `y` | optional | Large contiguous allocations that `dma_alloc_attrs` serves from CMA |
| `CONFIG_DMA_CMA` | `y` | optional | As `CONFIG_CMA`, for the DMA allocator |

Selected by the architecture rather than set by an image builder: `HAS_DMA` (the `dma_*` mapping
functions), `HAVE_CLK` (through `COMMON_CLK`), and `HAS_IOMEM` (`devm_ioremap_resource`,
`devm_memremap`). They are on for the reference SoC and for the QEMU harness's architecture.

Not referenced by `lemnos.ko`, so they need nothing for it: `DMABUF_HEAPS` and its heaps (the
daemon's own test allocations only, `LEMNOS_TEST_DMA_HEAP`), `HARDENED_USERCOPY`, and the
`MEDIA_*` and USB options (the module does not touch them).

## Per driver family

The stock route is what the daemon uses without the module. The module route is what
`lemnos.ko` needs in addition, from the table above. Reference values come from the reference
SoC's `.config`: `y`, `m`, `unset`, or `no symbol` when the option does not exist in this tree.

### I2C

Stock route: `i2c-dev` on `/dev/i2c-N`. Module route: `i2c_transfer` through the claim (the module
needs only `CONFIG_I2C`).

| Symbol | Stock | Module | Built in (no-modules image) | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_I2C` | required | required | `y` | `y` | the bus core, both routes |
| `CONFIG_I2C_CHARDEV` | required | not needed | `y` | `m` | `/dev/i2c-N` |
| `CONFIG_I2C_DESIGNWARE_PLATFORM` | required (per SoC, see below) | required (same adapter) | `y` | `m` | the reference SoC's RP1 I2C buses |

### SPI

Stock route: `spidev` on `/dev/spidevB.C`. Module route: `spi_sync` (the module needs only
`CONFIG_SPI`).

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_SPI` | required | required | `y` | `y` | the SPI core; `SPI_MASTER` follows |
| `CONFIG_SPI_SPIDEV` | required | not needed | `y` | `m` | `/dev/spidevB.C` |
| SoC controller (per SoC, see below) | required | required | `y` | see below | the controller |

### GPIO

Stock route: the character device on `/dev/gpiochipN`. Module route: `gpiod_*` through the claim.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_GPIOLIB` | required | required | `y` | `y` | the GPIO core |
| `CONFIG_GPIO_CDEV` | required | not needed | `y` | `y` | the `/dev/gpiochipN` ioctl interface (`GPIO_CDEV_V1` is also `y`) |
| SoC chip driver (per SoC, see below) | required | required | `y` | see below | the chip itself |

### PWM

Stock route: the sysfs class on `/sys/class/pwm/pwmchipN`. Module route: `pwm_apply_might_sleep`.
There is no `PWM_SYSFS` option in this tree: the `pwm` class is built with `CONFIG_PWM` (`drivers/pwm/core.c`).

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_PWM` | required | required | `y` | `y` | the PWM core and its sysfs class |
| SoC PWM driver (per SoC, see below) | required | required | `y` | see below | the channel |

### UART

Stock route: a tty device, through termios (`backends/linux/src/uart.rs`). Module route: none.
The reference SoC has no UART in its SoC map. Its `.config` has `SERIAL_AMBA_PL011=y`, but that is
not a SoC map entry, so it is not claimed here.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_TTY` | required | not needed | `y` | `y` | the tty layer |
| `CONFIG_SERIAL_8250` (+ `_DW` for DesignWare UARTs) | required, per SoC | not needed | `y` | `unset` | the SoC's UART; see the SoC table |

### USB host

Stock route: `usbfs` on `/dev/bus/usb`. Module route: none.
`usbfs` has no option of its own in this tree: `devio.o` is part of `usbcore`, so it exists
whenever `CONFIG_USB` is on. `USB_DEVICEFS` no longer exists, and naming it in a board
configuration does nothing.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_USB` | required | not needed | `y` | `y` | `usbcore` and `usbfs` |
| host controller (per SoC) | required | not needed | `y` | `y` (`XHCI_HCD`, `XHCI_PLATFORM`, `XHCI_PCI`, `DWC3`) | the controller the board uses |

### USB gadget

Stock route: configfs at `/sys/kernel/config/usb_gadget`, FunctionFS at `/dev/usb-ffs`, the UDC
under `/sys/class/udc`. The daemon's functions are `ncm.usb0`, `acm.GS0` and `ffs.udc0`. Module
route: none.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_USB_GADGET` | required | not needed | `y` | `y` | the gadget framework |
| `CONFIG_CONFIGFS_FS` | required | not needed | `y` | `y` | `/sys/kernel/config` |
| `CONFIG_USB_CONFIGFS` | required | not needed | `y` | `m` | the configfs gadget (selects `USB_LIBCOMPOSITE`) |
| `CONFIG_USB_LIBCOMPOSITE` | required | not needed | `y` | `m` | selected by `USB_CONFIGFS` |
| `CONFIG_USB_CONFIGFS_NCM` | required (for `ncm.usb0`) | not needed | `y` | `y` | the NCM function (needs `NET`) |
| `CONFIG_USB_CONFIGFS_ACM` | required (for `acm.GS0`) | not needed | `y` | `y` | the ACM function (needs `TTY`) |
| `CONFIG_USB_CONFIGFS_F_FS` | required (for `ffs.udc0`) | not needed | `y` | `y` | the FunctionFS function, `/dev/usb-ffs` |
| `CONFIG_USB_F_FS` | required | not needed | `y` | `m` | selected by `USB_CONFIGFS_F_FS` |
| UDC driver (per SoC, see below) | required | not needed | `y` | see below | the controller in peripheral mode |

### V4L2 capture and media controller

Stock route: `/dev/videoN`, `/dev/mediaN` and `/dev/v4l-subdevN`. The media request API used by the
stateless decoder is part of the media controller: there is no `MEDIA_CONTROLLER_REQUEST_API`
option in this tree, and `VIDEO_V4L2` is gone too. Module route: none; the module drives the
receiver and ISP itself on the reference SoC.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_MEDIA_SUPPORT` | required | not needed | `y` or `m` | `m` | the media subsystem |
| `CONFIG_MEDIA_CONTROLLER` | required | not needed | `y` (must not be `m`) | `y` | the media device and the request API: `drivers/media/Makefile` builds `mc/` only when it is `y` |
| `CONFIG_VIDEO_DEV` | required | not needed | `y` or `m` | `m` | the V4L2 core (`videodev`) |
| `CONFIG_VIDEO_V4L2_SUBDEV_API` | required | not needed | `y` | `y` | `/dev/v4l-subdevN` |
| `CONFIG_V4L2_FWNODE` | required | not needed | `y` or `m` | `m` | sensor graph from firmware nodes |
| `CONFIG_V4L2_MEM2MEM_DEV` | required (stateless decoder) | not needed | `y` or `m` | `m` | the memory-to-memory framework that the decoder uses |
| capture and decoder drivers (per SoC) | required | not needed | `y` or `m` | see below | the receiver and the decoder |

The reference `.config` also has `VIDEO_RP1_CFE=m` and `VIDEO_RASPBERRYPI_PISP_BE=m`. The
receiver's stock driver must not bind to a block the module drives (see the reference SoC row
below). `VIDEO_RPI_HEVC_DEC=m` is in the same configuration, but whether the reference board's
decoder is that driver is not established here.

### dma-heaps and CMA

Stock route: `/dev/dma_heap/system` and `/dev/dma_heap/linux,cma`, the allocations of
`lemnosd`'s stream buffers. Module route: none; the module exports its own dma-bufs.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_DMABUF_HEAPS` | required | not needed | `y` | `y` | the heap device nodes |
| `CONFIG_DMABUF_HEAPS_SYSTEM` | required | not needed | `y` | `y` | `/dev/dma_heap/system` |
| `CONFIG_DMABUF_HEAPS_CMA` | required for the CMA heap | not needed | `y` | `y` | `/dev/dma_heap/linux,cma`; needs `DMA_CMA` |
| `CONFIG_CMA` | required for the CMA heap | optional | `y` | `y` | the contiguous area |
| `CONFIG_DMA_CMA` | required for the CMA heap | optional | `y` | `y` | the DMA allocator's CMA |
| `CONFIG_CMA_SIZE_MBYTES` | sizing | not needed | any | `5` | the default CMA area size |

### PIO (RP1 PIO state machines)

Stock route: none. The PIO block is reached only through the module, which calls the stock RP1 PIO
driver's exports. Module route: `rp1_pio_*` plus the firmware interface under it.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_RP1_PIO` | not needed | required (for the PIO part) | `y` | `m` | the PIO driver (`drivers/misc/rp1-pio.c`, exports `rp1_pio_*`) |
| `CONFIG_FIRMWARE_RP1` | not needed | required (`RP1_PIO` depends on it) | `y` | `m` | the RP1 firmware interface |
| `CONFIG_MBOX_RP1` | not needed | required (`FIRMWARE_RP1` depends on it) | `y` | `m` | the mailbox to the RP1 firmware |
| `CONFIG_MFD_RP1` | not needed | required (`MBOX_RP1` depends on it) | `y` | `y` | the RP1 multi-function device |

### Watchdog

Stock route: `/dev/watchdog0`. Module route: none.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_WATCHDOG` | required | not needed | `y` | `y` | the watchdog menu |
| `CONFIG_WATCHDOG_CORE` | required | not needed | `y` | `y` | the core, which makes `/dev/watchdogN` (`watchdog_dev.o` is part of it). `WATCHDOG_DEV` does not exist in this tree |
| SoC driver (per SoC, see below) | required | not needed | `y` | see below | the hardware timer |

### hwmon and thermal

Stock route: `/sys/class/hwmon` and `/sys/class/thermal/thermal_zoneN`. Module route:
`thermal_zone_get_temp`, which needs `CONFIG_THERMAL` (see the table above).

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_HWMON` | required | not needed | `y` or `m` | `y` | the hwmon class |
| `CONFIG_THERMAL` | required | required | `y` | `y` | the thermal zones |
| `CONFIG_THERMAL_OF` | required (zones from the tree) | not needed | `y` | `y` | zones from device tree nodes |
| `CONFIG_THERMAL_HWMON` | optional | not needed | `y` | `y` | zones exposed through hwmon |

### Slot path

Stock route: the boot partition (`vfat`), the data partition (`ext4`), the root image (`erofs`
on a loop device, with dm-verity). On raw NAND, UBI volumes in place of partitions. Module
route: none. The slot-init program loads the device-mapper modules itself, so an image with no
modules needs them built in.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_BLK_DEV_LOOP` | required | not needed | `y` | `y` | the loop device for `rootfs.erofs` |
| `CONFIG_BLK_DEV_DM` | required | not needed | `y` | `m` | device mapper (`/dev/mapper/control`) |
| `CONFIG_DM_BUFIO` | required | not needed | `y` | `m` | selected by `DM_VERITY` |
| `CONFIG_DM_VERITY` | required | not needed | `y` | `m` | the verity target |
| `CONFIG_EROFS_FS` | required | not needed | `y` | `y` | the root image |
| `CONFIG_EXT4_FS` | required | not needed | `y` | `y` | the data partition |
| `CONFIG_VFAT_FS` | required | not needed | `y` | `y` | the boot partition (selects `FAT_FS`) |
| `CONFIG_MTD_UBI`, `CONFIG_MTD_UBI_BLOCK`, `CONFIG_UBIFS_FS` | required on raw NAND only | not needed | `y` | `unset` | UBI volumes, the read-only block device over a volume (`ubiblockN_M`), and `ubifs` for the boot and data volumes. The reference SoC has no NAND |

### Hotplug uevents

Stock route: a `NETLINK_KOBJECT_UEVENT` socket (`backends/linux-sys/src/netlink.rs`). Module
route: none.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_NET` | required | not needed | `y` | `y` | the netlink family and the uevent socket |
| `CONFIG_UNIX` | required | not needed | `y` | `y` | the daemon's control socket |
| `CONFIG_DEVTMPFS` | required | not needed | `y` | `y` | device nodes under `/dev` |

### eventfd and memfd (streams)

Stock route: eventfds and memfds for stream buffers (`backends/linux-sys/src/eventfd.rs`,
`mmap.rs`). Module route: `eventfd_*`, required because the module signals its streams with
eventfds.

| Symbol | Stock | Module | Built in | Reference | Feature |
| --- | --- | --- | --- | --- | --- |
| `CONFIG_EVENTFD` | required | required | `y` | `y` | `eventfd(2)`, and the module's stream completion |
| `CONFIG_MEMFD_CREATE` | required | not needed | `y` | `y` | `memfd_create(2)` (selected by `TMPFS`) |
| `CONFIG_SHMEM` | required | not needed | `y` | `y` | the shmem file system behind memfd and `tmpfs` |
| `CONFIG_TMPFS` | required | not needed | `y` | `y` | `tmpfs` |

## The SoC drivers

Per SoC, the driver behind each generic symbol, for the SoCs the tree has maps for
(`socs/*.toml`). "Verified" means the row was found in the Kconfig or Makefile of the named tree, or
(for mainline) in the driver's source, read on the web. "Fallback only" means the driver is
found but the SoC's own compatible string is not in it, so the SoC's device tree must name a
compatible the driver does match. "Not verified" means it was not found; the row is a candidate,
not a claim.

| SoC (map) | Function | Symbol | Driver | Compatible | Verified |
| --- | --- | --- | --- | --- | --- |
| reference SoC: `bcm2712-rp1` | I2C (RP1) | `CONFIG_I2C_DESIGNWARE_PLATFORM` | `i2c-designware-platdrv.c` | `snps,designware-i2c` | verified (reference tree source and `.config`) |
| reference SoC: `bcm2712-rp1` | GPIO | `CONFIG_PINCTRL_RP1` | `drivers/pinctrl/pinctrl-rp1.c` | the RP1 GPIO chip | verified (reference `.config`); the chip is `pinctrl-rp1` |
| reference SoC: `bcm2712-rp1` | PWM | `CONFIG_PWM_RP1` | `drivers/pwm/pwm-rp1.c` | `raspberrypi,rp1-pwm` | verified (reference tree source) |
| reference SoC: `bcm2712-rp1` | USB device | `CONFIG_USB_DWC2` | `drivers/usb/dwc2/` | `brcm,bcm2835-usb` (`params.c`) | verified (reference tree source); dual-role, `USB_DWC2_PERIPHERAL` unset |
| reference SoC: `bcm2712-rp1` | watchdog | `CONFIG_BCM2835_WDT` | `drivers/watchdog/bcm2835_wdt.c` | not read | verified (reference `.config`) |
| reference SoC: `bcm2712-rp1` | RP1 MFD (the bus the RP1 blocks hang on) | `CONFIG_MFD_RP1` | `drivers/mfd/Kconfig` | the RP1 endpoint | verified (reference tree source). Depends on `CONFIG_PCI_MSI`, which `bcm2712_defconfig` does not state: the whole RP1 chain drops out without it (see Dependencies) |
| reference SoC: `bcm2712-rp1` | PCIe host (the root port the RP1 sits behind) | `CONFIG_PCIE_BRCMSTB` | `drivers/pci/controller/pcie-brcmstb.c` | the SoC's PCIe root port | verified (reference `.config`, and its Kconfig depends on `PCI_MSI`). Not a Kconfig dependency of `MFD_RP1`, but the RP1 is not reached without it: state it in the board's fragment |
| reference SoC: `bcm2712-rp1` | PIO | `CONFIG_RP1_PIO` | `drivers/misc/rp1-pio.c`, `include/linux/pio_rp1.h` | the RP1 PIO node | verified (reference tree source); needs `FIRMWARE_RP1` and `MBOX_RP1` |
| reference SoC: `bcm2712-rp1` | SPI | none | none | no SPI controller in the map | not applicable |
| k230 | I2C | `CONFIG_I2C_DESIGNWARE_PLATFORM` | `i2c-designware-platdrv.c` | `snps,designware-i2c` | fallback only: the driver is verified, the K230 string is not checked |
| k230 | SPI | `CONFIG_SPI_DW_MMIO` | `spi-dw-mmio.c` | `canaan,k210-spi` only | not verified: mainline has no K230 string |
| k230 | GPIO | `CONFIG_GPIO_DWAPB` | `gpio-dwapb.c` | `snps,dw-apb-gpio` only | not verified: mainline has no `canaan,k230-apb-gpio` |
| k230 | PWM | none | none | the map's `pwm` is kind `block` | not applicable (no stock route) |
| k230 | watchdog | `CONFIG_DW_WATCHDOG` | `drivers/watchdog/dw_wdt.c` | `snps,dw-wdt` | verified (mainline source) |
| rk3566 | I2C | `CONFIG_I2C_RK3X` | `i2c-rk3x.c` | `rockchip,rk3399-i2c` (fallback) | fallback only: `rk3566` is not in the driver |
| rk3566 | SPI | `CONFIG_SPI_ROCKCHIP` | `spi-rockchip.c` | `rockchip,rk3066-spi` (fallback) | fallback only: `rk3566` is not in the driver |
| rk3566 | GPIO | `CONFIG_GPIO_ROCKCHIP` | `gpio-rockchip.c` | not read | not verified |
| rk3566 | PWM | `CONFIG_PWM_ROCKCHIP` | `pwm-rockchip.c` | `rockchip,rk3328-pwm` (fallback) | fallback only: `rk3566` is not in the driver |
| rk3566 | UART | `CONFIG_SERIAL_8250_DW` | not read | not read | not verified |
| rk3588 | I2C | `CONFIG_I2C_RK3X` | `i2c-rk3x.c` | `rockchip,rk3399-i2c` (fallback) | fallback only: `rk3588` is not in the driver |
| rk3588 | SPI | `CONFIG_SPI_ROCKCHIP` | `spi-rockchip.c` | `rockchip,rk3066-spi` (fallback) | fallback only: `rk3588` is not in the driver |
| rk3588 | GPIO | `CONFIG_GPIO_ROCKCHIP` | `gpio-rockchip.c` | not read | not verified |
| rk3588 | PWM | `CONFIG_PWM_ROCKCHIP` | `pwm-rockchip.c` | `rockchip,rk3328-pwm` (fallback) | fallback only: `rk3588` is not in the driver |
| rk3588 | UART | `CONFIG_SERIAL_8250_DW` | not read | not read | not verified |
| sun60i-a733 | I2C | `CONFIG_I2C_MV64XXX` | `i2c-mv64xxx.c` | `allwinner,sun4i-a10-i2c`, `allwinner,sun6i-a31-i2c` | not verified for A733: no A733 string |
| sun60i-a733 | SPI | `CONFIG_SPI_SUN6I` | `spi-sun6i.c` | `allwinner,sun55i-a523-spi` (the nearest) | not verified for A733 |
| sun60i-a733 | GPIO | `CONFIG_PINCTRL_SUNXI` | pinctrl for A733 | `allwinner,sun60i-a733-pinctrl` | not verified: only a 2025 RFC, not in mainline as read |
| sun60i-a733 | PWM | `CONFIG_PWM_SUN20I` | `pwm-sun20i.c` (D1, T113, R329 family) | not read | not verified for A733: the file could not be read, and no A733 PWM was found |
| sun60i-a733 | UART | `CONFIG_SERIAL_8250_DW` | not read | not read | not verified |

The rows that need a board, or a kernel that is not mainline, are candidates until a board run
confirms them. See [Hardware status](hardware-status.md).

## Dependencies

Each symbol this document names, with the Kconfig symbols it depends on that a board's
`bcm2712_defconfig` does not state (the reference SoC's defconfig, read from its tree). The rule is
about what a fragment or defconfig must state explicitly: a dependency that a `default` or a
`select` supplies when a full build resolves the configuration is still listed, because a merged
fragment does not get it. A dependency that is only a prompt-less symbol, set by the architecture
(`HAS_IOMEM`, `HAVE_PCI`, `HAS_DMA`, `IRQ_DOMAIN`), is not listed: no fragment sets those by hand.

Where a symbol depends on `FIRMWARE_RP1 || COMPILE_TEST` (`RP1_PIO`) or on another alternative, the
alternatives are followed, not listed, since the board may mean any of them. The table is
regenerated, from a defconfig file and a kernel tree, with
`scripts/check-kernel-config.py --print-table <tree> <defconfig> --arch arm64`. The gate checks that
every symbol this document names has a row, and that each dependency listed for a `lemnos.ko`
required symbol is enabled in both harness configurations. CI checks the same closure against the
harness kernel's own `.config`, after the build.

| Symbol | Section | Depends on (not stated in bcm2712_defconfig) |
|---|---|---|
| `CONFIG_ARCH_SUNXI` | Cross-check against the reference SoC's configuration | none |
| `CONFIG_BCM2835_WDT` | The SoC drivers | none |
| `CONFIG_BLK_DEV_DM` | Slot path | `CONFIG_BLOCK` |
| `CONFIG_BLK_DEV_LOOP` | Slot path | `CONFIG_BLK_DEV` `CONFIG_BLOCK` |
| `CONFIG_CMA` | Symbols of lemnos.ko | none |
| `CONFIG_CMA_SIZE_MBYTES` | dma-heaps and CMA | none |
| `CONFIG_COMMON_CLK` | Symbols of lemnos.ko | none |
| `CONFIG_CONFIGFS_FS` | USB gadget | none |
| `CONFIG_CPU_BIG_ENDIAN` | Symbols of lemnos.ko | none |
| `CONFIG_DEVTMPFS` | Hotplug uevents | none |
| `CONFIG_DMABUF_HEAPS` | dma-heaps and CMA | none |
| `CONFIG_DMABUF_HEAPS_CMA` | dma-heaps and CMA | none |
| `CONFIG_DMABUF_HEAPS_SYSTEM` | dma-heaps and CMA | none |
| `CONFIG_DMA_CMA` | Symbols of lemnos.ko | none |
| `CONFIG_DMA_SHARED_BUFFER` | Symbols of lemnos.ko | none |
| `CONFIG_DM_BUFIO` | Slot path | `CONFIG_BLOCK` |
| `CONFIG_DM_VERITY` | Slot path | `CONFIG_BLOCK` |
| `CONFIG_DW_WATCHDOG` | The SoC drivers | none |
| `CONFIG_EROFS_FS` | Slot path | `CONFIG_BLOCK` |
| `CONFIG_EVENTFD` | Symbols of lemnos.ko | none |
| `CONFIG_EXT4_FS` | Slot path | none |
| `CONFIG_FIRMWARE_RP1` | PIO (RP1 PIO state machines) | `CONFIG_PCI_MSI` |
| `CONFIG_GPIOLIB` | Symbols of lemnos.ko | none |
| `CONFIG_GPIO_CDEV` | GPIO | `CONFIG_GPIOLIB` |
| `CONFIG_GPIO_DWAPB` | The SoC drivers | `CONFIG_GPIOLIB` |
| `CONFIG_GPIO_ROCKCHIP` | The SoC drivers | `CONFIG_GPIOLIB` |
| `CONFIG_HWMON` | hwmon and thermal | none |
| `CONFIG_I2C` | Symbols of lemnos.ko | none |
| `CONFIG_I2C_CHARDEV` | I2C | none |
| `CONFIG_I2C_DESIGNWARE_PLATFORM` | I2C | none |
| `CONFIG_I2C_MV64XXX` | The SoC drivers | none |
| `CONFIG_I2C_RK3X` | The SoC drivers | `CONFIG_COMMON_CLK` `CONFIG_OF` |
| `CONFIG_IOMMU_SUPPORT` | Symbols of lemnos.ko | none |
| `CONFIG_LEMNOS` | Built in, for an image with no modules | `CONFIG_COMMON_CLK` `CONFIG_GPIOLIB` `CONFIG_OF` `CONFIG_RESET_CONTROLLER` `CONFIG_THERMAL` |
| `CONFIG_MBOX_RP1` | PIO (RP1 PIO state machines) | `CONFIG_PCI_MSI` |
| `CONFIG_MEDIA_CONTROLLER` | V4L2 capture and media controller | none |
| `CONFIG_MEDIA_SUPPORT` | V4L2 capture and media controller | none |
| `CONFIG_MEMFD_CREATE` | eventfd and memfd (streams) | none |
| `CONFIG_MFD_RP1` | PIO (RP1 PIO state machines) | `CONFIG_PCI_MSI` |
| `CONFIG_MODULES` | Built in, for an image with no modules | none |
| `CONFIG_MTD_UBI` | Slot path | none |
| `CONFIG_MTD_UBI_BLOCK` | Slot path | `CONFIG_BLOCK` |
| `CONFIG_NET` | Hotplug uevents | none |
| `CONFIG_OF` | Symbols of lemnos.ko | none |
| `CONFIG_PCIE_BRCMSTB` | The SoC drivers | `CONFIG_OF` `CONFIG_PCI_MSI` |
| `CONFIG_PCI_MSI` | The SoC drivers | none |
| `CONFIG_PINCTRL_RP1` | The SoC drivers | `CONFIG_OF` `CONFIG_PCI_MSI` `CONFIG_PINCTRL` |
| `CONFIG_PINCTRL_SUNXI` | The SoC drivers | `CONFIG_ARCH_SUNXI` |
| `CONFIG_PM` | Symbols of lemnos.ko | none |
| `CONFIG_PWM` | Symbols of lemnos.ko | none |
| `CONFIG_PWM_ROCKCHIP` | The SoC drivers | none |
| `CONFIG_PWM_RP1` | The SoC drivers | none |
| `CONFIG_PWM_SUN20I` | The SoC drivers | (not in this tree) |
| `CONFIG_REGULATOR` | Symbols of lemnos.ko | none |
| `CONFIG_RESET_CONTROLLER` | Symbols of lemnos.ko | none |
| `CONFIG_RP1_PIO` | Built in, for an image with no modules | `CONFIG_PCI_MSI` |
| `CONFIG_SERIAL_8250` | UART | none |
| `CONFIG_SERIAL_8250_DW` | The SoC drivers | none |
| `CONFIG_SHMEM` | eventfd and memfd (streams) | none |
| `CONFIG_SPI` | Symbols of lemnos.ko | none |
| `CONFIG_SPI_DW_MMIO` | The SoC drivers | none |
| `CONFIG_SPI_ROCKCHIP` | The SoC drivers | none |
| `CONFIG_SPI_SPIDEV` | SPI | none |
| `CONFIG_SPI_SUN6I` | The SoC drivers | `CONFIG_RESET_CONTROLLER` |
| `CONFIG_THERMAL` | Symbols of lemnos.ko | none |
| `CONFIG_THERMAL_HWMON` | hwmon and thermal | `CONFIG_THERMAL` |
| `CONFIG_THERMAL_OF` | hwmon and thermal | `CONFIG_OF` `CONFIG_THERMAL` |
| `CONFIG_TMPFS` | eventfd and memfd (streams) | `CONFIG_SHMEM` |
| `CONFIG_TTY` | UART | none |
| `CONFIG_UBIFS_FS` | Slot path | none |
| `CONFIG_UNIX` | Hotplug uevents | none |
| `CONFIG_USB` | USB host | `CONFIG_USB_SUPPORT` |
| `CONFIG_USB_CONFIGFS` | USB gadget | none |
| `CONFIG_USB_CONFIGFS_ACM` | USB gadget | `CONFIG_TTY` |
| `CONFIG_USB_CONFIGFS_F_FS` | USB gadget | none |
| `CONFIG_USB_CONFIGFS_NCM` | USB gadget | none |
| `CONFIG_USB_DWC2` | The SoC drivers | `CONFIG_USB_SUPPORT` |
| `CONFIG_USB_F_FS` | USB gadget | none |
| `CONFIG_USB_GADGET` | USB gadget | none |
| `CONFIG_USB_LIBCOMPOSITE` | USB gadget | none |
| `CONFIG_V4L2_FWNODE` | V4L2 capture and media controller | none |
| `CONFIG_V4L2_MEM2MEM_DEV` | V4L2 capture and media controller | none |
| `CONFIG_VFAT_FS` | Slot path | none |
| `CONFIG_VIDEO_DEV` | V4L2 capture and media controller | none |
| `CONFIG_VIDEO_V4L2_SUBDEV_API` | V4L2 capture and media controller | `CONFIG_MEDIA_CONTROLLER` `CONFIG_VIDEO_DEV` |
| `CONFIG_WATCHDOG` | Watchdog | none |
| `CONFIG_WATCHDOG_CORE` | Watchdog | none |

## Cross-check against the reference SoC's configuration

The reference SoC's real `.config` (read-only, from its tree) was checked against every symbol this
document names, with the same closure as the Dependencies table, on the real configuration (which
is complete, so nothing is implied by a default). Result:

- Every required symbol of the `lemnos.ko` table is set, except `CONFIG_CPU_BIG_ENDIAN`, which is
  unset, as required. `CONFIG_LEMNOS` is not set: it is the new in-tree switch.
- The RP1 chain is set: `CONFIG_MFD_RP1=y`, `CONFIG_PCI_MSI=y`, `CONFIG_PCIE_BRCMSTB=y`. The real
  configuration has `PCI_MSI` at `y`, although `bcm2712_defconfig` does not state it.
- Unset dependencies on the real configuration: `CONFIG_MTD_UBI` (needed by `CONFIG_MTD_UBI_BLOCK`
  and `CONFIG_UBIFS_FS`, NAND only; this SoC has no NAND), `CONFIG_ARCH_SUNXI` (needed by
  `CONFIG_PINCTRL_SUNXI`, another SoC), and `CONFIG_SERIAL_8250` (needed by `CONFIG_SERIAL_8250_DW`,
  a UART the reference board does not use). Nothing a `lemnos.ko` required symbol depends on is unset.
- Unset on the real configuration, not used by this SoC: `CONFIG_DW_WATCHDOG`, `CONFIG_GPIO_DWAPB`,
  `CONFIG_GPIO_ROCKCHIP`, `CONFIG_I2C_MV64XXX`, `CONFIG_I2C_RK3X`, `CONFIG_PINCTRL_SUNXI`,
  `CONFIG_PWM_ROCKCHIP`, `CONFIG_SPI_ROCKCHIP`, `CONFIG_SPI_SUN6I`, `CONFIG_SERIAL_8250`,
  `CONFIG_MTD_UBI`, `CONFIG_MTD_UBI_BLOCK`, `CONFIG_UBIFS_FS`, `CONFIG_LEMNOS`.

The reference's values that differ from what a no-modules image needs: `CONFIG_RP1_PIO`,
`CONFIG_FIRMWARE_RP1` and `CONFIG_MBOX_RP1` are `m`, and the spidev, i2c-dev and configfs
functions are `m`, so an image without modules must set them to `y`.
