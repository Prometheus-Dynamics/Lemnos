# Board files

A board file says which devices a board has and how each is wired. It is data. Nothing in Lemnos
knows a board: change the file and the same daemon runs a different one.

The daemon reads `/etc/lemnos/board.toml` unless `--board` or `LEMNOSD_BOARD` names another file.
Unknown keys are refused by name, so a typing mistake is an error and never a silent default.

```sh
lemnosd --check --board docs/examples/sim-board.toml
lemnosd --check --board docs/examples/full-board.toml
lemnosd --check --board testing/boards/guest.toml
lemnosd --check --board testing/bench/board.toml
lemnos-ctl validate docs/examples/full-board.toml
```

`--check` parses the file and exits, 0 when it is well formed. `lemnos-ctl validate` checks it the
same way with no daemon, and also prints the host settings the file gives, defaults filled in, and
the devices that opted in to auto-off. Both check the keys on this page, and both run every built-in
driver's check of its own `config` keys, with no hardware and no device made. Every board file in the repository is checked by both,
and each one's expected outcome is listed in `service/ctl/tests/board_files.rs`. `lemnos-ctl validate`
refuses a board file with a setting a driver refuses, and names the device and the key:

```
board.toml: device "lights": setting "brightnes" is not a setting this driver reads (did you mean "brightness"?)
board.toml: device "temp": setting "description" is not a chip description: a file's path goes in description_file
```

The daemon does not refuse the whole board for that. It starts without the refused device: the
device is listed as failed, with its key and the reason in its state (`lemnos-ctl list` shows it as
`retrying  failed`, with the reason in brackets), the daemon says so at start, and every other device
runs. A device that uses or is linked to the refused one behaves as it does when that device fails
to bind. `lemnosd --check` refuses the board, as validate does. Only a file that is not a board at all
(not TOML, or no `[board]`) stops the daemon from starting. The same holds for a device whose own
entry is wrong (a duplicate id, a `config` value in no readable form, a stream its driver cannot take)
and for a loop of `uses`, which refuses the device that closes it. The full list of what stops the
daemon at start is in [Start-up](daemon.md#start-up). A check that needs the hardware (a
register the chip answers, a device node that opens, the board's serial) is not made here: it is
checked when the driver binds, and a device that fails to bind is listed with the reason.

## The keys

At the top:

| Key | Meaning |
|---|---|
| `format` | Always `"lemnos.board"` |
| `schema_version` | `1`. A file with a higher number than the daemon knows is refused |
| `[board]` `id` | The board's name. The daemon gives it to clients as its own name |
| `[host]` | How the host treats every driver (below). Optional |
| `[[devices]]` | One table per device, in any order |
| `[[each]]` | One table per kind of hardware that gets a device of its own wherever it is found. See [One device for each that fits](#one-device-for-each-that-fits) |

In a device:

| Key | Meaning |
|---|---|
| `id` | The device's name. Unique in the file |
| `label` | The name a person reads for the device, shown in place of its id. Optional |
| `driver` | The driver's name, as in [Drivers and contracts](drivers.md). An unknown name leaves the device `retrying` with that as its reason |
| `bus` | The resource this device sits on, by the backend's name for it (below) |
| `uses` | A table from one of the driver's need names to a device `id` or a resource name |
| `link` | The `id` of another device this one is wired to. The host only hands it over, and `lemnos-ctl validate` refuses a `link` to a device the board doesn't have. `lemnos-ctl chain` follows it. A driver that needs the link takes it at bind (a `#[link]` field) and confirms it from the hardware side: see [Drivers and contracts](drivers.md#contracts) |
| `config` | The driver's settings: numbers, booleans and strings. Each driver documents its own |
| `disabled` | `true` starts the device off. `lemnos-ctl on` turns it on |
| `auto_off_after` | Opts this device in to being turned off after this many failed binds in a row, at least 1. Left out, it is retried forever. `lemnos-ctl on` brings it back |
| `owner` | `"lemnos"` (the default) or `"host"`. With `"host"` the operating system owns the hardware and Lemnos only reads it. See [Concepts](concepts.md#who-owns-the-hardware) |
| `[devices.streams.NAME]` | A frame stream this device offers, by the name its driver needs and a consumer asks for |


In a stream:

| Key | Meaning |
|---|---|
| `buffers` | How many buffers. At most 4096, or 128 with `memory = "dma-buf"` |
| `working` | The producer's working set: the buffers it holds in its own hands when it asks for its next one (a device's queue, `K - 1` for a queue of `K`). Left out, 0. Less than `buffers`. The reader's margin is `buffers - 1 - working` frames: [`streams.md`](streams.md) |
| `kind` | Refused, by name, whatever its value. The driver that serves the stream declares who makes its frames (`#[needs(free)]` or `#[needs(job)]`), and the host takes it from there: [`streams.md`](streams.md), "The kind of stream" |
| `buffer_bytes` | The size of each. At most 256 MiB, and 16 GiB for the whole stream |
| `job_slots` | How many jobs a consumer can have waiting. 4 when left out, at most 64, rounded up to a power of two |
| `job_bytes` | The largest job. 256 when left out, at most 4096 |
| `memory` | `"shared"` (the default): memory shared with the consumer, filled by the driver in software. `"dma-buf"`: one dma-buf per buffer, so hardware can fill it or read it |
| `heap` | Only with `memory = "dma-buf"`: the dma-heap the buffers come from, by its name under `/dev/dma_heap`. `"system"` when left out |
| `planes` | Only with `memory = "dma-buf"`: the lengths of a buffer's memory planes, for hardware that writes one frame into several (a node of the multi-plane V4L2 API). Each plane is its own dma-buf, 1 to 8 of them, and they add up to `buffer_bytes`. `buffers` times the plane count is at most 128. Left out, a buffer is one plane |
| `standby` | `true`: the stream has memory while no consumer is attached, so hardware keeps filling it and a consumer that attaches is handed the newest frame at once, with nothing copied. `false` (the default): memory is made only for a consumer |

[Frame streams](streams.md) says which to choose.

In `[host]`, every key optional. Each one left out keeps the default:

| Key | Default | Range | Meaning |
|---|---|---|---|
| `call_deadline_ms` | 2000 | 1 to 60000 | How long one driver call may run before it is `TIMED_OUT` and the device is retried once the call lets go |
| `retry_first_ms` | 1000 | 1 to the cap | The wait before a device that failed is bound again. It doubles with each failure in a row |
| `retry_cap_ms` | 8000 | 1 to 8000 | The longest wait, and how long a driver must stay up before its wait starts again from the first |
| `dma_heaps` | `["linux,cma"]` | heap names | The dma-heaps the daemon's own drivers allocate from, by name under `/dev/dma_heap`: each is one file name. Each dma-buf stream's heap is added to them. Gives `lemnos-ctl tree --udev` and `--mdev` their lines, and the daemon's start check its nodes. `[]` names none |

The cap can be shorter than 8 s but never longer: that is [the failure rule](concepts.md#the-failure-rule),
and a longer value is refused. There is no host-wide auto-off: only a device's own
`auto_off_after` turns a device off.

## Boot sources (`system-slots`)

A board that boots from a disk must give `boot_sources` in its slot device's `config`: one
`source=kernel path` entry per medium the board boots from. The boot disk is the one whole disk under
that controller, and nothing else on the boot chain is read from another disk. `boot_device` is
removed (refused by name). [Slots: the boot source](slots.md#the-boot-source) has the rules and the
U-Boot chain line by line.

| Board | Family | Boot chain | Medium | Entry | Status |
|---|---|---|---|---|---|
| Radxa ZERO 3E | RK35xx (RK3566) | U-Boot | microSD | `/mmc@fe2b0000=/mmc@fe2b0000` | the path is the kernel's; the U-Boot alias number is decided by U-Boot's live tree, not verified on a board |
| Radxa ROCK 5C | RK3588S | U-Boot | microSD | `/mmc@fe2c0000=/mmc@fe2c0000` | kernel path from `rk3588-base.dtsi`; U-Boot path unverified |
| Radxa ROCK 5C | RK3588S | U-Boot | eMMC | `/mmc@fe2e0000=/mmc@fe2e0000` | kernel path; U-Boot path unverified |
| Radxa ROCK 4D | RK3576 | U-Boot | microSD | `/soc/mmc@2a310000=/soc/mmc@2a310000` | the RK3576 node sits under `/soc`; the U-Boot path is unverified |
| Radxa CUBIE A7A, A7S, A7Z | A733 (sun60i) | U-Boot | microSD, eMMC | `/soc@0/mmc@4020000=…`, `/soc@0/mmc@4022000=…` | from the patch series; the Radxa U-Boot tree is not read; UNVERIFIED |
| Raspberry Pi compute module with the BCM2712 (the reference board) | BCM2712 | Pi firmware, no U-Boot | on-board eMMC | `rpi-boot-mode-1=/soc@107c000000/mmc@fff000` | code 1 is the SD card or the eMMC on the Pi family (measured on the reference board); other codes are from documentation, unverified |

Unsupported: a scsi or UFS boot device on a U-Boot board is refused by name (the path form cannot
name it). A raw NAND board (`record = "ubi"`) has no disk and no `boot_sources`.

## Trials of a slot (`system-slots`)

The `system-slots` driver's `config` (see [Slots](slots.md)) takes these trial settings. Each is
optional and keeps its default when left out:

| Key | Default | Range | Meaning |
|---|---|---|---|
| `auto_confirm` | `true` | `true`, `false` | The board confirms a healthy trial itself. `false`: only a `confirm` call confirms, and nothing restarts the board by itself |
| `confirm_uptime_s` | `30` | 0 up | How long the daemon must have run in the trial before it may confirm |
| `confirm_deadline_s` | `600` | at least `confirm_uptime_s` | The time, from the daemon's start, by which every condition must hold |
| `reboot_at_deadline` | `true` | `true`, `false` | At the deadline with a condition unmet: record why, and restart into the default. Ignored with `auto_confirm = false` |
| `trial_attempts` | `3` | 1 to 3 | Boots in a row that may fail to reach a running daemon before a trial falls back. A boot that reaches the daemon gives its attempt back |
| `health` | `[]` | a list of strings | Extra conditions for a trial to confirm. Each is `device:<id>` (that device's health is Ok), `file:<path>` (the file exists and was modified since this boot began) or `socket:<path>` (the Unix socket accepts a connection, checked without waiting: a full backlog is busy, and busy for 2 s is not good). A checked path must be absolute |

`health` is an array of strings in the device's `config` (`health = ["device:x", "file:/run/y"]`).
Only the built-in drivers see it; other arrays in a device's `config` are refused.

## Boot history (`system-slots`, `system-reboot`)

These keys are optional. [Slots](slots.md#boot-history) says what the history holds.

| Driver | Key | Default | Range | Meaning |
|---|---|---|---|---|
| `system-slots` | `boots_keep` | `16` | at least 1 | How many boots the boot history keeps (`lemnos/boots`), newest kept |
| `system-slots` | `reset_reason` | unset | an absolute path | A file the board writes its reset reason to. Read at each boot for the history; unset, the first watchdog's `bootstatus` is read |
| `system-reboot` | `data` | `/data` | an absolute path | The data partition: where a restart through `system.reboot` leaves the request the next boot's history names as `requested` |

## `[board]` identity

`[board] id` is the board's family, and the driver `system-identity` reads the rest of the
identity from its device's `config`: the serial source, the display name and the revision. See
[Identity facts](identity.md#serial-source-settings).

## The device tree

Three keys describe the hardware for the device tree generator (`lemnos-ctl tree`, see
[the device tree](device-tree.md)). The daemon reads them and does not use them. They are optional:
a board that has none runs as before.

| Key | Meaning |
|---|---|
| `[board] soc` | The name of the SoC map the board is built for (`socs/<soc>.toml`) |
| `[board] base` | The base tree the board boots, by its name in that map. It says which holders a `[[lines]]` entry frees, and the VBUS supply of each device port |
| `[[hardware]]` | One piece of hardware a device uses: `controller` (the map's name), `name` (the runtime name a device uses for it, if any), `pins` (the map's pin set, where the controller has several). A device port (a `usb-peripheral` controller) also takes how its VBUS is handled: `vbus` with `vbus_off` (a GPIO line that switches VBUS, and the level that is off), or `vbus_undriven = true` (this port is not driven by the board), each with `vbus_reason` |
| `[[lines]]` | A GPIO line the board gives to Lemnos, as `gpio:LABEL:LINE`. `boot` (`"low"` or `"high"`) holds it from power-on, with the `reason` it is held: in the overlay as a `gpio-hog` unless a device uses the line or a pin set takes its pad, and in the boot and U-Boot fragments (see [Holds from power-on](device-tree.md#holds-from-power-on)) |

## `bus` and `uses`

A driver names what it needs: `bus`, `line`, `accel`, `frames`. The host fills each need in this
order:

1. A stream of that name under the device's `streams`.
2. The entry of that name in `uses`. If it is the `id` of another device, the driver gets that
   device, and that device is bound first. Otherwise it is a resource name for the backend.
3. `bus`, for any need the driver does not mark optional.

So a driver with one need takes `bus`, and a driver with several names each under `uses`. An
optional need is filled only by its own name, under `streams` or `uses`. When nothing names it, the
driver runs without it, whatever `bus` says.

A device that uses another is bound after it, wherever it is in the file, and waits for it to be
running. A device may not use itself.

## Resource names

A name means what the backend says it means.

On Linux (`lemnosd` with no `--sim`):

| Name | What it is |
|---|---|
| `i2c-N` | The I2C adapter `/dev/i2c-N`. Every device on a bus opens it; the kernel serialises them |
| `pio-i2c:sda=N,scl=N` | An I2C bus made from two GPIO lines by the RP1 PIO block. `,hz=N` sets the clock |
| `pio-out:gpio=N` | One state machine of the RP1 PIO block driving GPIO N, running the program its driver gives (a `ws2812` strip takes this as its `bus`) |
| `spidevB.C` | The SPI device `/dev/spidevB.C` |
| `gpiochipN:LINE` | One line of a GPIO chip |
| `pwmchipN:CHANNEL` | One channel of a PWM chip |
| `watchdog:NODE` | The hardware watchdog whose device-tree node is `NODE`, named in a device's `uses` as `watchdog = "watchdog:NODE"`. The stable form; the generator enables the node |
| `watchdogN` | The hardware watchdog `/dev/watchdogN`, by its number. The number can change on reboot |
| `ttyNAME` | A serial port, such as `ttyAMA0`. No built-in driver takes one yet; a package can |
| `usb:BUS-PORT` | The USB device plugged into a port, such as `usb:1-1.4`. A port comes and goes with what is plugged in, so a device on it is hotplug to a driver that watches the board: absent is not failed |
| `usb:VVVV:PPPP` | The USB device with this vendor and product, in hex. Refused when more than one fits |
| `usb:class:CC`, `usb:class:CC:SS`, `usb:class:CC:SS:PP` | The USB device whose device descriptor or one of whose interfaces has this class, subclass and protocol, two hex digits each. A part left out fits any. Refused when more than one fits |
| `udc:NAME` | This board as a USB device, through the controller of that name under `/sys/class/udc` |
| `dma:BLOCK` | DMA memory for a block bound to the kernel module, by the block's device name |
| `regs:BLOCK` or `regs:BLOCK/N` | Register region `N` (0 when left out) of a block bound to the module, mapped |
| `irq:BLOCK` or `irq:BLOCK/N` | Interrupt `N` (0 when left out) of a block bound to the module |
| `clocks:BLOCK/NAME,NAME` | Clocks of a block bound to the module, by the names its firmware description gives them; index 0 is the first. `clocks:BLOCK` alone is the block's only clock |
| `resets:BLOCK/NAME,NAME` | Reset lines of a block bound to the module, named the same way |

When the [kernel module](kernel-module.md) is loaded, the daemon opens it and reaches I2C adapters,
GPIO lines, PWM channels and the SPI devices bound to the module through it, under claims the
kernel enforces. Without the module the same names go through the stock interfaces.

A block's own resources (`dma:`, `regs:`, `irq:`, `clocks:`, `resets:`) exist only with the module:
there is no other source of them, and nothing stands in. Each is refused with `NOT_SUPPORTED`
without the module and `NO_DEVICE` when the module has no block of that name bound. Register
regions are mapped only when the module says the block's registers may be (its DMA is confined by
an IOMMU); otherwise they are refused with `ACCESS`. A clock or reset name the block doesn't have is
refused when the device binds. Each is held by one device at a time.

A block the module was told to map though no IOMMU confines it (its `unconfined=` parameter, see
[the kernel module](kernel-module.md)) has its registers mapped too: the driver that holds them
then alone keeps the block's DMA inside its own memory. No built-in driver takes such a block now.

A block the module drives through register jobs is named by its platform device in the driver's
`config`, not under `uses`: the module checks every register the driver writes and writes every
address itself. For example, the K230 ISP serving the capture driver's frames and the ISP
driver's configuration at once:

```toml
[[devices]]
id = "capture"
driver = "k230-capture"
config = { block = "90000000.isp", mipi = "/dev/vvcam-mipi.0" }

[devices.streams.frames]
buffers = 4
buffer_bytes = 4147200

[[devices]]
id = "isp"
driver = "k230isp"
config = { block = "90000000.isp" }
# Its `config` and `statistics` streams, as on its vendor's way.
```

On the simulated board (`lemnosd --sim`) there are no buses. A device that needs one is listed as
`retrying`. Tests that need a simulated bus build a `SimBoard` in code and add named resources to
it. On a microcontroller the names are whatever the firmware calls its buses.

## One device for each that fits

A device entry names one piece of hardware. An `[[each]]` entry names a kind: the host makes a
device for each resource the backend finds that fits its `match`, when it starts and whenever
more comes. A USB camera plugged into any port then gets its driver with no entry of its own:

```toml
[[each]]
id = "camera"
driver = "uvc"
match = "usb:class:0e"
need = "usb"
config = { frame_bytes = 1048576 }

[each.streams.frames]
buffers = 4
buffer_bytes = 1048576
```

| Key | Meaning |
|---|---|
| `id` | The entry's name. Each device it makes is `ID@RESOURCE`, such as `camera@usb:1-1.4` |
| `driver` | The driver of each device it makes |
| `match` | What a resource must fit, as a [resource name](#resource-names) says it. On Linux a USB name: `usb:class:...` or `usb:VVVV:PPPP`, which here may fit any number of devices |
| `need` | The need the resource found fills. Left out, it is the device's `bus` |
| `except` | A list of names. A resource any of them reaches is never taken by this entry |
| `uses`, `config`, `owner`, `auto_off_after`, `[each.streams.NAME]` | As in a device, for each device made |

A resource is found by a name that stays the same for as long as it is wired the same way. On
Linux that is the USB port (`usb:1-1.4`). So the device made for it keeps its id, and its index,
when it leaves and comes back to the same port, and two cameras alike are two devices with ids
of their own. A device whose hardware left stays listed, retrying, as any device does; it is not
taken away while the service runs.

What a board entry names is its own. A resource that a `[[devices]]` entry names by any name
(its port, its vendor and product, a class that fits it alone) is never taken by an `[[each]]`
entry, so a device entry pins one device to settings of its own, and a device entry with
`disabled = true` keeps one from being bound at all. A resource is taken by one `[[each]]` entry
only: the first in the file that fits it.

The host looks again whenever the backend hints that hardware came or went, and every retry cap
(8 s unless `[host]` sets it) without a hint. On the simulated board nothing is found.

## A board that runs with no hardware

`docs/examples/sim-board.toml` is the board the other pages run their commands on:

```toml file=docs/examples/sim-board.toml
# A board for `lemnosd --sim`: devices with no hardware behind them. The fake drivers come from
# testing/crates/fake-devices and are in a lemnosd built with `--features fake-devices`.
format = "lemnos.board"
schema_version = 1

[board]
id = "sim"

# Three fakes that each serve one vector, read from `config`.
[[devices]]
id = "accel"
driver = "fake-accelerometer"
config = { x = 0.0, y = 0.0, z = 9.81 }

[[devices]]
id = "gyro"
driver = "fake-gyroscope"
config = { x = 0.0, y = 0.0, z = 0.0 }

[[devices]]
id = "mag"
driver = "fake-magnetometer"
config = { x = 0.00002, y = 0.0, z = 0.00004 }

# A real driver built from other devices: it needs no hardware of its own.
[[devices]]
id = "orientation"
driver = "orientation"
uses = { accel = "accel", gyro = "gyro", mag = "mag" }
config = { sample_rate_hz = 100 }

# A contract the tools have no code for: a value, a control and an action.
[[devices]]
id = "counter"
driver = "fake-counter"
link = "accel"
config = { start = 41 }

# A device whose bus the simulated board does not have. It is listed, retrying, with the reason.
[[devices]]
id = "absent"
driver = "bmm150"
bus = "i2c-9"
```

## A complete example

`docs/examples/full-board.toml` uses every key and every kind of name. The wiring is made up.

```toml file=docs/examples/full-board.toml
# Every key a board file has, on made-up wiring. `lemnosd --check` accepts it, and `lemnosd --sim`
# starts on it. The simulated board has none of these buses, so a device that needs one is listed
# as retrying with its reason, which is also what a real board shows for hardware that is not
# there. (A driver that opens its block only when it is started shows as running until then.)
format = "lemnos.board"        # always this text
schema_version = 1             # a file newer than the service is refused

[board]
id = "example"                 # the name the service gives itself to clients

# `[host]`: how the host treats every driver. Each key may be left out; these are the defaults.
[host]
call_deadline_ms = 2000        # how long one driver call may run: 1 to 60000
retry_first_ms = 1000          # the first wait before a failed device is bound again
retry_cap_ms = 8000            # the longest wait: at most 8000, the failure rule's cap
dma_heaps = ["linux,cma"]       # the dma-heaps the drivers allocate from, by name: [] for none

# `bus`: the one resource this device sits on. Here an I2C adapter, by its kernel name.
[[devices]]
id = "power0"
driver = "ina2xx"
bus = "i2c-1"
config = { chip = "ina238", address = 0x40, shunt_ohms = 0.010, max_current_a = 2.0, bus_warn_below_v = 10.8, bus_warn_above_v = 13.5 }

# An I2C bus made from two GPIO lines by the RP1's PIO block. `hz` is optional.
[[devices]]
id = "imu"
driver = "bmi088"
bus = "pio-i2c:sda=8,scl=7,hz=400000"
config = { accel_rate = 200, gyro_rate = 200, fifo = false }

# `disabled`: listed and left alone until someone turns it on.
[[devices]]
id = "mag"
driver = "bmm150"
bus = "i2c-1"
disabled = true

# `uses`: need name to device id. A device built from other devices has no bus.
[[devices]]
id = "orientation"
driver = "orientation"
uses = { accel = "imu", gyro = "imu", mag = "mag" }
config = { sample_rate_hz = 200 }

# `uses` also names a resource for one need, when a driver has more than one.
# A GPIO line is `gpiochipN:LINE`.
[[devices]]
id = "port-power"
driver = "gpio-power-switch"
uses = { line = "gpiochip0:16", fault = "gpiochip0:17" }
config = { default_on = true }

# An SPI device is `spidevBUS.CS`, a PWM channel `pwmchipN:CHANNEL`, a serial port its tty name.
[[devices]]
id = "ring-pixels"
driver = "ws2812"
bus = "spidev0.0"
config = { count = 16, wire = "rgb", brightness = 255 }

# The same strip on the RP1 PIO block: a `pio-out` name instead of an SPI device. Needs the PIO
# block's node (and the kernel module, or the stock node).
[[devices]]
id = "ring-pixels-pio"
driver = "ws2812"
bus = "pio-out:gpio=13"
config = { count = 16, wire = "rgb", brightness = 255 }

# `uses` names an `indicator`'s pixels: the indicator shows named states (`receiving`, `failed`, ...)
# on the strip, each drawn with the pattern the board gives it. `brightness` scales every pattern.
[[devices]]
id = "status-indicator"
driver = "indicator"
uses = { pixels = "ring-pixels" }
config = { brightness = 128, "state.failed" = "blink #ff0000 period=400" }

# A first-flash target: a flasher's board names its block device here and sets
# `allow_first_flash = true`. A board that is not a flasher leaves it false, and a write is refused.
# `uses` names the indicator that shows the write's states (an optional need).
[[devices]]
id = "first-flash"
driver = "system-flash"
uses = { indicator = "status-indicator" }
config = { path = "/dev/mmcblk0", allow_first_flash = false, max_dict_mb = 8 }

# `auto_off_after`: this device alone opts in to being turned off after 5 failed binds in a
# row. Every other device is retried forever.
[[devices]]
id = "bench"
driver = "raw-bus"
uses = { i2c = "i2c-1", pwm = "pwmchip0:1" }
auto_off_after = 5

# `owner = "host"`: the operating system's own driver has this hardware. Lemnos reads it and
# changes nothing.
[[devices]]
id = "fan"
driver = "hwmon-fan"
owner = "host"
config = { name = "pwmfan", max_rpm = 3000, warn_below_rpm = 200 }

# The daemon's keep-alive: a hardware watchdog, set to 15 s. The daemon feeds it only while it is
# healthy; see docs/daemon.md, "If the daemon dies".
[[devices]]
id = "watchdog"
driver = "hw-watchdog"
keepalive = true
config = { device = "watchdog0", timeout_s = 15 }

# A fan on a PWM channel, driven directly: no kernel fan driver in between. `tach` is optional.
# The duty is 0 to 1000; `min_duty` is the lowest duty the fan turns at.
[[devices]]
id = "channel-fan"
driver = "pwm-channel-fan"
uses = { pwm = "pwmchip0:1", tach = "gpiochip0:20" }
config = { frequency_hz = 25000, min_duty = 200, kick_ms = 500, pulses_per_rev = 2, max_rpm = 3000, warn_below_rpm = 200 }

# A USB device by the port it is plugged into, or by vendor and product (`usb:1a2b:3c4d`).
# `[devices.streams.NAME]` gives the device a frame stream of shared memory.
[[devices]]
id = "webcam"
driver = "uvc"
uses = { usb = "usb:1-1.4" }
config = { frame_bytes = 1048576 }

[devices.streams.frames]
buffers = 4                    # how many buffers the stream has
buffer_bytes = 1048576         # the size of each

# This board as a USB device, through the controller `udc:NAME` names. Its `data` stream carries
# jobs: `job_slots` is how many can wait, `job_bytes` the largest one.
[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc:fe980000.usb" }
config = { vendor = 0x1209, product = 0x0001, manufacturer = "Example", product_name = "Example", serial = "0001", interfaces = "function ffs\ninterface 0xff 0 0 example\nendpoint in bulk\nendpoint out bulk\nfunction ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=lemnos%d\nfunction acm\n" }

[devices.streams.data]
buffers = 8
buffer_bytes = 69632
job_slots = 16
job_bytes = 18

# A camera: sensor, receiver and ISP are three devices. `link` says what each is wired to.
[[devices]]
id = "cam0-sensor"
driver = "ov9782"
bus = "i2c-10"
link = "cam0-receiver"

# `memory = "dma-buf"`: one dma-buf per buffer, so the block writes straight into the memory the
# consumer reads. `heap` names the dma-heap they come from ("system" when left out).
[[devices]]
id = "cam0-receiver"
driver = "rp1-csi2"
link = "cam0-isp"
config = { device = "1f00110000.csi", embedded_bytes = 256 }

[devices.streams.frames]
buffers = 8
buffer_bytes = 2097152
memory = "dma-buf"
heap = "linux,cma"
standby = true                 # capture with nobody attached; a consumer gets the newest frame

# `embedded`: the sensor's embedded data lines, on the same device (a stream of the name the driver
# asks for). One buffer per line, the frame's sequence on it; `embedded_bytes` is a line's length,
# a multiple of 16 from 16 to 65520. Asked for only where the sensor sends embedded lines.
[devices.streams.embedded]
buffers = 4
buffer_bytes = 256
memory = "dma-buf"
heap = "linux,cma"

[[devices]]
id = "cam0-isp"
driver = "pisp-be"
config = { device = "1000880000.pisp_be" }

[devices.streams.config]
buffers = 4
buffer_bytes = 8192

[devices.streams.input]
buffers = 4
buffer_bytes = 2097152
memory = "dma-buf"
heap = "linux,cma"

[devices.streams.output0]
buffers = 4
buffer_bytes = 4194304
memory = "dma-buf"
heap = "linux,cma"

# A receiver whose node writes each frame in two memory planes: one dma-buf per plane.
[[devices]]
id = "cam1-receiver"
driver = "sunxi-vin"
config = { capture = "vin_video0" }

[devices.streams.frames]
buffers = 4
buffer_bytes = 3110400
memory = "dma-buf"
planes = [2073600, 1036800]    # the node's planes, in order: they add up to buffer_bytes

# DMA memory for a block bound to the kernel module: `dma:` and the block's device name.
[[devices]]
id = "capture"
driver = "k230-capture"
uses = { dma = "dma:90009000.isp" }
config = { isp = "/dev/vvcam-isp.0", mipi = "/dev/vvcam-mipi.0" }

[devices.streams.frames]
buffers = 4
buffer_bytes = 4194304

# `[[each]]`: a device for each resource that fits `match`, here each USB camera plugged in that
# no device above names, whatever port it is in. Each is named `camera@` and its port.
[[each]]
id = "camera"
driver = "uvc"
match = "usb:class:0e"
need = "usb"
except = ["usb:1-1.3"]           # never this one
config = { frame_bytes = 1048576 }

[each.streams.frames]
buffers = 4
buffer_bytes = 1048576

# The board's self-test: its members run in the order named. `include_intrusive = false` leaves
# the intrusive tests out of `all`, which runs the rest.
[[devices]]
id = "selftest"
driver = "board-self-test"
config = { members = ["power0", "imu"], include_intrusive = false }
```


The `ffs` function is the vendor interface a program uses, and its two bulk endpoints can be lent to
one client at a time: the client reads the OUT endpoint and writes the IN endpoint itself, with the
daemon still owning the gadget, its control endpoint and the bus. A function with other than one
bulk OUT and one bulk IN endpoint is not lent (`NOT_SUPPORTED`). See [the wire's Endpoints
section](wire.md#endpoints) for the hand-off, its refusals and what a reconnect or a gadget restart
means for a holder. `LEMNOS_ORION_LINKS=gadget=serial` on the Orion bridge offers the pair to the comm core as
the link `serial`.

The gadget's `interfaces` text (`interfaces_file`, a path to a file holding it, is read only by a build with a
filesystem; giving both is refused by name) lists its functions in configfs link order, one per line: `function ffs`
(the FunctionFS link the daemon serves; the default, first, when no line names it), `function ncm
host=MAC device=MAC [ifname=TEMPLATE]` or `function ncm derive [ifname=TEMPLATE]` (a CDC network
function; both MACs unicast and different), and `function acm` (a serial console, `ttyGS0` for the
first). The `interface`, `alternate` and `endpoint` lines describe the FunctionFS function. Each kind
is listed at most once. `host=MAC device=MAC` gives the board's own addresses. `derive` takes both
from the board's identity, so every board of a type gets its own pair (see below).

A board file that gives no `interfaces` setting gets the default composition, the one the managed link is
defined on: the link (FunctionFS, interface 0, class 0xff, interface string `orion-link`), then CDC NCM with
the addresses derived from the board's identity, then one CDC ACM:

```text
function ffs
interface 0xff 0 0 orion-link
endpoint in bulk
endpoint out bulk
function ncm derive ifname=lemnos%d
function acm
```

That text is `DEFAULT_INTERFACES` in `drivers/usb-gadget` and `[composition] default` in
`contracts/usb-link.toml`; a test checks both. A board file that gives `interfaces` deviates from it, and
must keep the link first: its first `interface` line is the vendor-class (0xff) link, interface 0. A text
whose first interface is anything else drops the link or moves it off interface 0, and is refused
(`LinkNotFirst`). A second `function ffs` is refused as a function listed twice.

`ifname=TEMPLATE` (optional, once per NCM line) names the network interface the host side gets. The
template is one to 15 bytes of printable ASCII with exactly one `%d`, which the kernel replaces with
the first free number: `lemnos%d` gives `lemnos0`. A fixed name such as `lemnos0`, a template with
two numbers, a `/`, a `:` or a space is refused when the board file is read. The kernel's own rule
is the same (`gether_set_ifname` in `f_ncm`). Without `ifname` the kernel names the interface `usb<N>`,
as it always has. The name needs Linux 5.12 or later: before it, `ifname` is read-only (3.17 to
5.11) or missing (before 3.17), and the privileged setup refuses a board that names an interface
with `Unsupported` and the reason `this kernel's NCM function can't be given an interface name`.
Nothing else is set up for that gadget.

Once the gadget is bound the driver reads the name the kernel gave back, and describes it as the
item `ifname` (text), and the `usb.function` contract's method 11, `ifname`, returns the same text. It is absent while the gadget is down, and absent when the gadget has no NCM function.

### The link's identity and its Windows descriptors

Every gadget presents one link: the FunctionFS function, whose first interface (interface 0) is the
vendor link, with the CDC NCM and ACM functions of the setup after it. The link is always in the
function list: `description::functions` adds it when the board file does not name it. So the
descriptors that bind it on Windows are not a board option; the backend writes them for every gadget
it builds, and the same code builds the daemon's gadget, the QEMU harness's and any other.

- **Link GUID.** `{CF13388F-BE34-4A1E-94C2-9AAFEF9A04E9}` is the Lemnos link GUID, the device
  interface GUID the link registers. It is the constant `usb_descriptors::ms_os::LINK_GUID`, the same
  on every product, and it does not change.
- **Microsoft OS 1.0 descriptors.** The kernel sends string 0xEE (`MSFT100`, vendor code `0x51`) and,
  for the device, the extended compatible ID with `WINUSB` for interface 0 and `WINNCM` for the NCM
  control interface. For interface 0 it sends the extended property `DeviceInterfaceGUID` (REG_SZ)
  with the link GUID. The kernel can send only these: MS OS 2.0 and its BOS descriptor are not emitted
  by Linux 7.3, and a REG_MULTI_SZ property (the plural `DeviceInterfaceGUIDs`) has no wire encoding
  in the composite core. Windows applies WinUSB to the link interface only, not to the device, so the
  network and serial functions keep their own drivers.
- **Release (`bcdDevice`).** Windows keeps OS-descriptor state per vendor, product and release, so a
  device that changes its descriptors under the same release keeps the old answer. The release is
  therefore tied to the link: `drivers/usb-gadget/src/revision.rs` lists each release with the
  fingerprint of the link it presented, and the default release is the last entry. A test fails when
  the link changes without a new entry with a higher release. A board that sets `release` keeps its
  own number; a product that does so takes the same care.
- **Composite class.** The default class triple is 0xEF, 0x02, 0x01 (interface association
  descriptors). With class 0 the device has no class, and Windows binds each function of a composite
  only through that triple.
- **Vendor and product.** A board file without `vendor` or `product` gets `LINK_VENDOR` and
  `LINK_PRODUCT` in `drivers/usb-gadget/src/lib.rs`. Both are placeholders: 0x1d6b is the Linux
  Foundation's id, and 0x0104 is its "Multifunction Composite Gadget" entry, which every composite
  gadget built on the Linux framework uses too. Windows keys its OS-descriptor cache, and any INF or
  udev rule, on these numbers, so a product must choose its own pair once, before any image leaves the
  bench. Its choices are a vendor id it owns or a product id from an open allocation (pid.codes assigns
  PIDs only under an open-source licence, and its VID 0x1209 has PID 0x0001 listed as a test id).

Untested on Windows and macOS. The host test to run on real machines, and what it checks per
function, is [the R1h board-run record](../testing/board-runs/R1h.md). The Linux side is checked by
the QEMU harness, which requests the descriptors the way Windows does and compares the bytes.

### The managed identity: reservation, layout, builders and the byte stream

- **Reservation.** The managed identity is vendor 0x1d6b, product 0x0104, with `bcdDevice` reserved
  to 0x0100 through 0x0aff. The release it presents now is 0x0200, and it only ever rises. Another
  team's bring-up images use 0x1d6b:0103 with `bcdDevice` from 0x0b00. The numbers, the reservation
  and the link's identity are in `contracts/usb-link.toml` (`version = 1`). The tests of
  `drivers/usb-gadget` read that file and fail if the code's constants differ from it, and a reader
  refuses an unknown version or key. Other teams' tests and device data read the file rather than
  copying the numbers. The vendor and product numbers stay PLACEHOLDERS until the user settles a
  product id; that choice moves the managed range with it.
- **Interface layout.** The link is the first function of the composite, so its interface is
  interface 0, whatever order the board file lists the functions in
  (`description::functions` always puts `function ffs` first). The NCM and ACM functions follow it.
  The link is its own function: no interface association covers it, and a test checks that no
  descriptor of the link is an association (type 11). The Microsoft OS 1.0 compatible ID section
  names interface 0 (`bFirstInterfaceNumber` 0), and the extended property `DeviceInterfaceGUID` is
  requested for interface 0 (`wValue` 0, `wIndex` 5), which is how the host finds the link child.
- **Who builds a gadget.** A gadget of ours is built through the daemon only: `lemnosd`'s root
  phase (`service/lemnosd/src/gadget.rs`) calls `lemnos_linux::gadget::prepare`, so every gadget
  the daemon makes emits the same descriptors. Flasher and recovery images build theirs the same
  way, through the daemon. In this repository the daemon is the only builder; the flasher and
  recovery images are not in it and were not checked here. A gadget built by hand, outside the
  daemon, must not present the managed identity, because a Windows cache keyed on its numbers would
  then hold descriptors the hand-built gadget does not match.
- **The byte stream.** The peer's team states (not verified by us) that what travels on the link is
  their byte stream: COBS-encoded frames ending in 0x00, with a signed handshake, and that USB packet
  boundaries, short packets and zero-length packets carry no meaning. This driver never generates a
  zero-length packet: the FunctionFS endpoints are opened without `FUNCTIONFS_ENDPOINT_ENABLE_ZLP`
  (the kernel default is off, `f_fs.c` `zlp_enabled`), and a send of no bytes completes without a
  transfer (`drivers/usb-gadget/src/serve.rs`, `send`). A zero-length packet from the host is
  received as an empty read. Nothing here depends on one.
- **The lent endpoints after END.** Once END hangs up, the holder's copies of the endpoints go away
  within the completion of one in-flight read: the codes the gadget documents for a dead function
  (`EIDRM` and `ESRCH`, mapped to `Unavailable`) make that read return; that the read does return
  is to be tested on a board, not verified here. A remount or re-describe
  that still sees the endpoints busy for longer than that is a finding to report, not an expected
  state.

### Identity-derived addresses and the USB serial

The gadget names the identity device whose serial is the board's serial, and the daemon reads that
serial once, before anything is set up (`docs/identity.md`):

```toml
[[devices]]
id = "identity"
driver = "system-identity"
[devices.config]
serial = "device-tree"

[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc:fe800000.usb" }
config = { vendor = 0x1209, product = 0x0001, identity = "identity", interfaces = "function ffs\ninterface 0xff 0 0 pipe\nendpoint in bulk\nendpoint out bulk\nfunction ncm derive\nfunction acm\n" }
```

- `identity` (optional) names the `system-identity` device. Without it, or with a serial that cannot
  be read, the gadget has no board serial. A gadget that derives its NCM addresses (`function ncm
  derive`, which the default composition has) and names no `identity` device is refused by
  `lemnos-ctl validate` by name: the file cannot start its gadget. A board file that names a device that is not there, or
  one that is not a `system-identity` device, is refused: `lemnos-ctl validate` refuses the board, and
  the daemon starts without the gadget, which shows failed with the reason. The board file may not set
  `board_serial` or `board_serial_error` itself: the daemon writes them.
- `serial` (optional string) is the USB serial string the host shows. It defaults to the board
  serial. With neither, the device has no serial string and its health is `Degraded` with the
  reason. Nothing is invented.
- `function ncm derive` needs the board serial. Without one, the gadget does not set up, and the
  daemon names the reason (the board serial could not be read, or the board names no identity).
  There is no fixed fallback address.

The rule. The input is the ASCII bytes of `<family>:<serial>`: `<family>` is the board file's
`[board] id`, `<serial>` is the normalised serial (lowercase hex, 8 to 32 digits, leading zeros
kept). The digest is SHA-256 of the 15 bytes `lemnos-usb-ncm\0` followed by that input. Take the
first 6 bytes and set the first to `(b0 & 0xfc) | 0x02`: unicast, locally administered. The host's
address is those 6 bytes with the lowest bit of the last octet cleared; the device's is the same
with that bit set.

Vectors, to check another implementation against. They are computed with `hashlib`, and the digest
prefixes match `printf 'lemnos-usb-ncm\0%s' 'alpha:00000001' | sha256sum`:

| `[board] id` | serial | host MAC | device MAC |
|---|---|---|---|
| `lemnos-test` | `0123456789abcdef` | `aa:22:75:11:2a:72` | `aa:22:75:11:2a:73` |
| `alpha` | `00000001` | `b2:b5:ba:4c:1e:30` | `b2:b5:ba:4c:1e:31` |

The two addresses differ only in the last bit, and the derived pair is the same in the daemon's
driver and in its root phase, because both read the same settings.

The gadget's configfs tree and FunctionFS mount are made by the daemon in its root phase, before it
drops to user `lemnos` (see `packaging/README.md`). The daemon then binds the gadget and serves ep0.

The simulated board accepts it. Devices whose buses it does not have are listed as retrying:

```sh
export LEMNOSD_SOCKET="${TMPDIR:-/tmp}/lemnosd.sock"
lemnosd --sim --board docs/examples/full-board.toml &
```

```sh
lemnos-ctl list
lemnos-ctl chain cam0-sensor
kill %1
wait
```

## Other boards in the repository

- `testing/boards/guest.toml`: a machine whose operating system drives its own hardware. Every
  device is `owner = "host"`.
- `testing/bench/board.toml`: the reference board's bench, used by the measurements in
  [Testing](testing.md).

## The kernel module's configuration

`lemnos-ctl tree BOARD` writes the module's configuration in two forms, from the same data.
`--modprobe FILE` is a modprobe.d fragment (`options lemnos`, and a `blacklist` for each stock
driver the module takes over) for a loadable `lemnos.ko`. `--cmdline FILE` is the same parameters
as one line of `lemnos.<name>=<value>` words, for a module built into the kernel, which takes
them from the kernel command line and has no modprobe. The `blacklist` has no command-line form.
See [kernel-config.md](kernel-config.md#parameters-from-the-command-line) for the length limit.

## The daemon's device permissions

The daemon drops to user `lemnos` after its root phase, so each node it opens must give that group
read and write. `lemnos-ctl tree BOARD --udev FILE` writes the udev rules for the board's own list:
`/dev/lemnos` and `/dev/lemnos-*` (the module's nodes), and each dma-heap the board uses
(`/dev/dma_heap/<name>`, from `[host] dma_heaps` and the dma-buf streams). Install it as
`/usr/lib/udev/rules.d/71-lemnos-board.rules`, beside `70-lemnos.rules`. The daemon checks, after its
drop, each of those nodes that exists. One the group cannot read and write is a line on the error output,
naming it (`/dev/dma_heap/linux,cma: not readable/writable by group lemnos: install the generated udev
fragment`); the devices that open it fail at their bind with their own reason, and the daemon starts. The unit (`packaging/systemd/lemnosd.service`) has no `DeviceAllow=` or `DevicePolicy=`,
so systemd filters no device and the unit names no heap.

`lemnos-ctl tree BOARD --mdev FILE` writes the same permissions for a BusyBox image, as `/etc/mdev.conf`
lines, one per node, in BusyBox's env-match form: `SUBSYSTEM=^misc$;^lemnos$ root:lemnos 0660`, where
`SUBSYSTEM=^misc$;` is the udev `SUBSYSTEM=="misc"` condition and `^lemnos$` is the node's path under
`/dev` (`^dma_heap/linux,cma$` for a heap). Both outputs come from one list of facts in
`service/devtree/src/permissions.rs`, and the tests parse both back and compare them, subsystem included,
for every board file in the repository (`service/devtree/tests/permissions_agree.rs`).

The lines are run by BusyBox 1.37.0 and 1.38.0 in the gate (`testing/mdev/check.sh`): both mdevs are built
from their release tarballs, sha256 pinned in `testing/mdev/build-busybox.sh`, and each runs against the
generated file for every board with a SoC, in a namespace with a fake sysfs. Each node's owner, group and
mode is checked against the udev rule for the same device, and a decoy device in another subsystem must be
left alone. The check reports the one way the heap rule fails: a uevent without `DEVNAME`. mdev matches the
device's `DEVNAME` (the kernel writes `dma_heap/<name>` for a heap; this machine's
`/sys/dev/char/250:0/uevent` says so), and without it falls back to the basename, which the path rule does
not match. The packaged kinds (serial ports, buses, cameras, usbfs, the watchdog) are not generated from
the board file: they are in `packaging/udev/70-lemnos.rules` and `packaging/init/mdev.conf.fragment`, kept
by hand in both forms; they match by name, as before.

## The self-test of a board

`board-self-test` serves [`self-test/1`](drivers.md#self-tests) for the board. Its `config` names the
members, in the order they run:

| Key | Type | Meaning |
|---|---|---|
| `members` | a list of device ids, or the string `"all"` | the members to run; `"all"` takes every device whose driver serves `self-test/1`, in board order. At most 16 |
| `include_intrusive` | bool, default `false` | whether `all` runs the members' intrusive tests too |

Its tests are each member's, named `member:test`, and `all`. A member that is not running is skipped
as `ABSENT`. The board needs no member to be running to start.
