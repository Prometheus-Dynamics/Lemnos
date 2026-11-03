# IO hardware blocks, per board

The rule (TODO.md, "IO hardware blocks, used in full"): every IO block a board has
is used for everything it can do. This page is the capability table for each block,
what Lemnos uses it for now, and the plan for the rest. A capability is **used**
(code runs it on the board), **written, untested on board** (code is in the tree,
host-tested only), **unused** (the block can do it, nothing does yet), or
**impossible** here (the block or its kernel interface cannot do it).

Sources: the Linux drivers' register and ioctl layouts in `lemnos-linux-sys`
(`rp1_pio.rs` takes its constants from the kernel's `rp1_pio_if.h`), the RP2040-layout
PIO register fields used there, and the board files and code in this repo. The RP1
peripherals datasheet is not in the tree, and nothing here was downloaded. Items marked
**verify on board** are facts this repo does not establish.

## BCM2712 board (RP1)

### RP1 PIO (`/dev/pio0`, the `rp1_pio` driver)

| Item | Value and source |
| --- | --- |
| Instruction slots | 32 per block (`INSTRUCTION_COUNT`, from `RP1_PIO_INSTRUCTION_COUNT`) |
| State machines | 4 per block, RP2040 layout; claimed with `SM_CLAIM` (any free one). Verify on board: `ls /dev/pio*` (only `pio0` is opened here) |
| Clock | 200 MHz (`PIO_CLOCK_HZ` in `lemnos-linux/.../pio/bus.rs`) |
| GPIOs | 28 (`GPIO_COUNT`), the pins the PIO can address |
| FIFO transfers | `XFER_DATA32` over DMA, both directions |

Program map, resident on `/dev/pio0` (checked by the test
`resident_programs_fit_the_32_slot_block`):

| Slots | Program | Owner | Used |
| --- | --- | --- | --- |
| 0..3 | `ws2812-pio-rp1` (4 instructions) | kernel `ws2812-pio` overlay, driven through `/dev/ledsN` | used (LED ring) |
| 4..30 | PIO I2C master, 27 instructions (`transport/i2c/pio/program.rs`) | lemnosd, `pio-i2c:sda=8,scl=7` | used (BMI088 IMU at up to 400 Hz) |
| 31 | free | none | unused |

Allocation: the I2C program loads at the first origin that fits (`load_program`), 4 here.
Capacity: 31 of 32 slots are used. A second program that needs two slots or more
cannot be resident beside them. `first_fit` in `lemnos-linux-sys` gives the same
placement the loader makes.

| Capability | Status | Used for / plan |
| --- | --- | --- |
| Bit-banged I2C | used | BMI088 on GPIO 8/7 (not on an RP1 hardware controller). |
| WS2812 / SK6812 drive | used | LED ring, through the kernel's `ws2812-pio` (not a Lemnos PIO program). |
| Free slot (31) | unused | Only a one-instruction program fits. |
| Fan tach counter | impossible now | Any edge-counting loop has a wait and a jump, so it needs two slots or more. One slot is free, so it needs a second block or a program swap. |
| Edge timestamps in PIO | not used | The kernel edge path already stamps events (see GPIO edges below). PIO timestamping would need a cycle-counter program, which does not fit. Revisit only if on-board jitter measurements say the kernel path is too slow. |
| Program swapping | unused | Possible for programs that run in bursts (ws2812 writes are bursts). The I2C master runs continuously, so swapping cannot free its slots. |
| Second PIO block | verify on board | If `/dev/pio1` exists on the BCM2712 board, the tach counter and any future PIO program go there. Not wired in Lemnos (the DEFAULT_DEVNODE is `/dev/pio0`). |

### RP1 PWM

| Capability | Status | Used for / plan |
| --- | --- | --- |
| PWM output (sysfs `pwm`) | used | The fan (`pwm-fan`, `pwm1`; the fan controller and hand-back write it, see composite-devices.md). Also PWM examples in board files. |
| PWM input / counter mode (count edges on the B pin) | impossible from userspace | The sysfs PWM interface does not expose it. Needs a kernel driver change (a module outside this tree, or the stock `pwm` driver), and the BCM2712 board fan's tach line is not in this repo. |

### RP1 GPIO (edges, timestamps)

| Capability | Status | Used for / plan |
| --- | --- | --- |
| Level read/write | used | Raw GPIO and GPIO devices (`pinctrl-rp1` lines). |
| Edge events (cdev uAPI v2, `open_gpio_edge_stream`) | used, now on the readings' clock | Edge events are stamped on `CLOCK_MONOTONIC` by the kernel. Since 1e0c504, Lemnos converts each stamp to `CLOCK_BOOTTIME` microseconds (`GpioEdgeEvent::observed_at_us`), the clock of `Reading::timestamp_us`. Written, untested on board. |
| Kernel-side clock choice | used: monotonic, converted | uAPI v2 offers monotonic (the default), realtime, and hardware timestamps (HTE). Lemnos requests none of them: realtime would not share the readings' clock, and HTE needs a hardware timestamp engine (verify on board). The boot/monotonic offset changes only at suspend. |
| Edge streams on the IMU data-ready line (BMI088 INT1) | unused | Plan: stamp each IMU sample at acquisition, not at read. Needs the INT line wired and named in the board file (not in this repo). |
| Edge streams on the camera strobe / frame-valid line | unused | Plan: frame-sync with the IMU on one clock. Needs the camera's strobe output wired to a GPIO (verify on the board's PCB). |

### RP1 DMA

| Capability | Status | Used for / plan |
| --- | --- | --- |
| RP1 DMA for PIO FIFO transfers | used, by the kernel driver | `XFER_DATA32` moves PIO FIFO data through the driver's DMA (`rp1_pio.rs`). |
| RP1 DMA for other peripherals (SPI, UART) | unused | The transfer interface (`lemnos-bus::transfer`) has the seam; no RP1 DMA engine is implemented. Board #2: the DW AXI DMAC (`dma2`, 8 channels, RP1 `0x1f00188000`) is reachable read-only through `/dev/mem`, root only; the kernel's PIO driver owns channels 0 to 2 and channels 3 to 7 are free. The blockers (no user-space interface, channel ownership, bus-address view) are in docs/dma.md, "Planned, not implemented". |

### RP1 I2C, SPI, UART (hardware blocks)

| Block | Status | Used for / plan |
| --- | --- | --- |
| I2C controllers (`i2c@74000` and others) | used where wired | Hardware buses the drivers bind to (board file `i2c:` selectors). The BMI088 is on the PIO bus because its pins (8/7) are not on one. |
| SPI controllers | available, not on the BCM2712 board board file in this repo | spidev drivers exist (`lemnos-drivers-spi`); verify the BCM2712 board's wiring. |
| UART | available, not on the BCM2712 board board file in this repo | `lemnos-drivers-uart`; verify the BCM2712 board's wiring. |

## Plan, in order

1. Done (1e0c504): edge stamps on the readings' clock, microseconds.
2. Written, untested on board: nothing new on the PIO block. Its one free slot stays free.
3. Needs board wiring (the board file is in the consuming app's
   `devices/`, not this repo): BMI088 INT1 and the camera strobe as edge streams, so each IMU sample and
   camera frame has an acquisition stamp on the boot clock.
4. Fan tach: blocked. The BCM2712 board's fan tach line is not in this repo's board files, and
   the kernel's PWM counter mode is not reachable from userspace. If the tach line is
   confirmed: first check `/dev/pio1` (second block) and then decide between a second
   block and a PWM counter driver. Do not free the I2C program's slots for it.
5. Done, untested on board: the K230D, Rockchip and RV1106 capability tables below, as code in
   the `lemnos-soc` crate (`CHIPS`). Board files for those chips are not written (the boards'
   pinouts are not here), so nothing runs on them yet.

## Other chips (descriptors, untested on board)

Each table below matches an `Entry` list in `crates/lemnos-soc/src/chips.rs` (`CHIPS`), where
the addresses are. **Used** means code in the tree runs the capability on that chip (none yet);
**Unused** means the block can do it and nothing does; **Impossible** means the block or its
kernel interface cannot do it. Every chip here is untested on board, and nothing was booted or
probed.

Sources. The Rockchip and K230 rows come from the mainline device trees at torvalds/linux master
`6628e7d4` (fetched 2026-11-02): `k230.dtsi`, `rk3588-base.dtsi` with `rk3588s.dtsi`,
`rk3576.dtsi`, `rk356x-base.dtsi` with `rk3566.dtsi`, `rk3562.dtsi`. Register layouts come from
the mainline drivers `pwm-rockchip.c` (the `rk3328-pwm` data), `timer-rockchip.c` (the rk3288
layout) and `timer-clint.c`. A row the description does not give says so; the TRM is named in
the chip's sources and was **not checked here** (no datasheet was downloaded).

Shared facts, checked against the drivers:

- Rockchip PWM (RK3562, RK3566, RK3588): the `rockchip,rk3328-pwm` layout, one channel per
  0x10 register block. Counter 0x00, period 0x04, duty 0x08, control 0x0c. The mainline driver has
  no capture operation, and sysfs exposes none, so **PWM input capture is impossible from the
  kernel interfaces here**. A `/dev/mem` path would bypass the kernel and is not planned.
- Rockchip timers (RK3576, RK3588): the rk3288 layout (control at 0x10). Their kernel driver owns
  them. Lemnos reads time through the arch timer, which is what every timestamp uses.
- Lemnos reads a Rockchip SAR ADC through IIO sysfs and a TSADC through the thermal zones, both
  with the generic bindings (`lemnos-drivers-linux`), once a board names them. Buffered reads
  need driver support that was not checked.

### K230D (`k230.dtsi`; the K230D shares the K230 map as far as that description shows)

| Block | Capability | Status | Software path it would replace |
| --- | --- | --- | --- |
| CLINT | CPU timer (`mtime`, `mtimecmp`), 27 MHz timebase | unused | the kernel's clocksource; layout `clint::` in `lemnos-soc` |
| PLIC | interrupt routing, 208 sources | unused | kernel IRQs (no Lemnos path) |
| UART x5 (`dw-apb-uart`, 0x91400000 + 0x1000 n) | serial | unused | `lemnos-drivers-uart` |
| pinctrl, clk, rst | pin mux, clock tree, resets | unused | the board file's pin names (no runtime path) |
| GPIO, PWM (output, input capture), timers, I2C, SPI, I2S, ADC, DMA, WDT | the vendor's IO | unused | the rows are not in `k230.dtsi`; the K230 TRM gives them (not checked here) |

The K230's single C908 (`thead,c908`) and its RVV are in `k230.dtsi`. riscv64 lemnosd and lemnos-ctl
already build (the `cross32` gate).

### RK3588S (`rk3588-base.dtsi` with `rk3588s.dtsi`)

The S description is `rk3588-base.dtsi` alone. `rk3588-extra.dtsi` adds PCIe 3.0 x4, x2 and
PCIe 2.0, the GMAC, the HDMI RX, USB host 1, and I2S6, I2S7, I2S8 and I2S10. The S description
leaves all of them out. Whether the S die has them is a TRM check.

| Block | Capability | Status | Software path it would replace |
| --- | --- | --- | --- |
| GPIO x5 (`rockchip,gpio-bank`) | levels and edges | unused | gpio cdev (`lemnos-linux`), as on RP1 |
| PWM x16 (`rk3588-pwm`, `rk3328-pwm` layout) | output | unused | sysfs pwm (`SysfsPwm`) |
| PWM x16 | input capture | **impossible** (no capture in the driver or sysfs) | none; a kernel capture interface |
| timer0 (`feae0000`, one channel) | bus timer | unused | the kernel's rockchip timer (no Lemnos path) |
| arm generic timer | clocksource | used (by the kernel, under every Lemnos timestamp) | `clock_gettime(CLOCK_BOOTTIME)` |
| WDT (`feaf0000`, DW) | watchdog | unused | `/dev/watchdog` |
| I2C x9 | controllers | unused | `i2c-dev` (`/dev/i2c-N`) |
| SPI x5 (sfc left out) | controllers | unused | `spidev` |
| UART x10 | serial | unused | `lemnos-drivers-uart` |
| I2S/TDM x7 (S description) | audio | unused | ALSA (none in Lemnos) |
| SARADC (`fec10000`) | ADC | unused | IIO sysfs through `KernelDevice` |
| TSADC (`fec00000`) | temperature | unused | `ThermalZone` |
| DMA PL330 x3 | DMA | unused | dmaengine in the kernel; no user-space path |

Cores as in `rk3588-base.dtsi`: 4 x Cortex-A76 and 4 x Cortex-A55.

### RK3576 (`rk3576.dtsi`)

The description has **no PWM and no I2S node** at this commit; those rows follow the TRM (not
checked here). The rest are in the table.

| Block | Capability | Status | Software path it would replace |
| --- | --- | --- | --- |
| GPIO x5 | levels and edges | unused | gpio cdev |
| PWM (not in the description) | output, input capture | unused | sysfs pwm once a driver binds it; capture unknown (TRM) |
| timer0 (`2acc0000`) | bus timer | unused | the kernel's rockchip timer |
| arm generic timer | clocksource | used (by the kernel) | `clock_gettime(CLOCK_BOOTTIME)` |
| WDT (`2ace0000`) | watchdog | unused | `/dev/watchdog` |
| I2C x10 | controllers | unused | `i2c-dev` |
| SPI x5 (sfc left out) | controllers | unused | `spidev` |
| UART x12 | serial | unused | `lemnos-drivers-uart` |
| I2S (not in the description) | audio | unused | ALSA (TRM, not checked) |
| SARADC (`2ae00000`) | ADC | unused | IIO sysfs through `KernelDevice` |
| TSADC (`2ae70000`) | temperature | unused | `ThermalZone` |
| DMA PL330 x3 | DMA | unused | dmaengine in the kernel |

Cores as in `rk3576.dtsi`: 4 x Cortex-A53 and 4 x Cortex-A72.

### RK3566 (`rk356x-base.dtsi`, shared with the RK3568, with `rk3566.dtsi`)

| Block | Capability | Status | Software path it would replace |
| --- | --- | --- | --- |
| GPIO x5 | levels and edges | unused | gpio cdev |
| PWM x16 (`rk3568-pwm`, `rk3328-pwm` layout) | output | unused | sysfs pwm |
| PWM x16 | input capture | **impossible** | none; a kernel capture interface |
| arm generic timer | clocksource | used (by the kernel) | `clock_gettime(CLOCK_BOOTTIME)` |
| WDT (`fe600000`) | watchdog | unused | `/dev/watchdog` |
| I2C x6 | controllers | unused | `i2c-dev` |
| SPI x4 (sfc left out) | controllers | unused | `spidev` |
| UART x10 | serial | unused | `lemnos-drivers-uart` |
| I2S/TDM x4 | audio | unused | ALSA |
| PDM (`fe440000`) | digital microphones | unused | ALSA (a sensor board could use it) |
| SARADC (`fe720000`) | ADC | unused | IIO sysfs through `KernelDevice` |
| TSADC (`fe710000`) | temperature | unused | `ThermalZone` |
| DMA PL330 x2 | DMA | unused | dmaengine in the kernel |

Cores as in `rk356x-base.dtsi`: 4 x Cortex-A55. The description has no Rockchip timer node.

### RK3562 (`rk3562.dtsi`)

| Block | Capability | Status | Software path it would replace |
| --- | --- | --- | --- |
| GPIO x5 | levels and edges | unused | gpio cdev |
| PWM x16 (`rk3562-pwm`, `rk3328-pwm` layout) | output | unused | sysfs pwm |
| PWM x16 | input capture | **impossible** | none; a kernel capture interface |
| arm generic timer | clocksource | used (by the kernel) | `clock_gettime(CLOCK_BOOTTIME)` |
| WDT | watchdog | unused (no node in the description; TRM) | `/dev/watchdog` |
| I2C x6 | controllers | unused | `i2c-dev` |
| SPI x3 (sfc left out) | controllers | unused | `spidev` |
| UART x10 | serial | unused | `lemnos-drivers-uart` |
| I2S (not in the description) | audio | unused | ALSA (TRM, not checked) |
| SARADC x2 (`ff730000`, `ffaa0000`) | ADC | unused | IIO sysfs through `KernelDevice` |
| DMA PL330 x1 (`ff990000`) | DMA | unused | dmaengine in the kernel |

Cores as in `rk3562.dtsi`: 4 x Cortex-A53.

### RV1106 (no mainline description)

| Block | Capability | Status | Software path it would replace |
| --- | --- | --- | --- |
| MCU (on-chip RISC-V core, beside the Cortex-A7) | runs firmware | unused | none: firmware, a loader and a message path would be needed (a remoteproc path is the kernel's way in; none was checked) |

The rest of the RV1106 IO map is a datasheet check. `arch/arm/boot/dts/rockchip` has no RV1106
description at this commit (it has rv1103b, rv1108, rv1109 and rv1126), so the RV1106 rows come from
the Rockchip datasheet only.
