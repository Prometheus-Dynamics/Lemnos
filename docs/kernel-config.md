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

### In-tree build, checked in QEMU

The harness kernel (`testing/qemu/build-kernel.sh`, pinned 7.3.3, the configuration in
`testing/qemu/kernel.config`) was built again with `lemnos` built in and modules still on, then
booted with no `lemnos.ko` in the image. Commands, in order (`<work>` is a scratch directory,
`<src>` the unpacked tree, `<out>` its build directory):

```text
cp -r kernel <src>/drivers/misc/lemnos
# drivers/misc/Kconfig:   source "drivers/misc/lemnos/Kconfig"
# drivers/misc/Makefile:  obj-$(CONFIG_LEMNOS) += lemnos/
echo CONFIG_LEMNOS=y >> <out>/.config
make -C <src> O=<out> ARCH=x86_64 olddefconfig
make -C <src> O=<out> ARCH=x86_64 -j6 bzImage modules
KERNEL=<out>/arch/x86/boot/bzImage testing/qemu/run-builtin.sh
```

`run-builtin.sh` boots with `lemnos.owned=lemnos,test-block,lemnos,test-spi lemnos.bus_buffer=8192`
on the command line. Its guest (`testing/qemu/init-builtin.c`) checks that the module is built in
(`/sys/module/lemnos` exists) and no `lemnos.ko` is in the image, that the control node exists
before any daemon starts, that both command-line parameters read back from
`/sys/module/lemnos/parameters`, and that `lemnos_claim_check` passes. Result, KVM, one run:
`lemnos-builtin: passed (0 failed)`, with no kernel warnings.

The guest suite (`testing/qemu/init.c`) is not run on the built-in kernel: it loads `lemnos.ko`
many times with different parameters, which a built-in module cannot do. Its parameters come from
the command line instead (below).

Not in CI: the in-tree build needs a second kernel build, a full harness-kernel build each run.
It is run by hand as above, and the `run-builtin.sh` check is for this kernel only.

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

## Cross-check against the reference SoC's configuration

The reference SoC's `.config` was read, read-only, and every row of the `lemnos.ko` table has the
value below. Every required symbol is set, and `CONFIG_CPU_BIG_ENDIAN` is unset, so the list
does not disagree with a configuration that worked on a board. `CONFIG_LEMNOS` does not exist in
that configuration: it is the new switch.

| Symbol | Reference |
| --- | --- |
| `CONFIG_MODULES`, `CONFIG_PM`, `CONFIG_OF`, `CONFIG_I2C`, `CONFIG_SPI`, `CONFIG_SPI_MASTER`, `CONFIG_GPIOLIB`, `CONFIG_GPIOLIB_LEGACY`, `CONFIG_PWM`, `CONFIG_REGULATOR`, `CONFIG_THERMAL`, `CONFIG_RESET_CONTROLLER`, `CONFIG_COMMON_CLK`, `CONFIG_DMA_SHARED_BUFFER`, `CONFIG_EVENTFD` | `y` |
| `CONFIG_CPU_BIG_ENDIAN` | unset (absent) |
| `CONFIG_RP1_PIO`, `CONFIG_FIRMWARE_RP1`, `CONFIG_MBOX_RP1` | `m` (so the board's PIO part is a module, which a no-modules image cannot use) |
| `CONFIG_IOMMU_SUPPORT`, `CONFIG_IOMMU_API`, `CONFIG_CMA`, `CONFIG_DMA_CMA` | `y` |

Where the reference's values differ from what a no-modules image needs: `CONFIG_RP1_PIO`,
`CONFIG_FIRMWARE_RP1` and `CONFIG_MBOX_RP1` are `m`, and the spidev, i2c-dev and configfs
functions are `m`, so an image without modules must set them to `y`.
