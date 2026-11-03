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
5. Not in this pass: the K230D, Rockchip (RK3588S, RK3576, RK3566, RK3562) and RV1106
   (Flame) IO and timer blocks. Each gets its own table when its board file lands.
