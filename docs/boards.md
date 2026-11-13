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
the devices that opted in to auto-off. Both check the keys on this page. Neither checks a driver's own `config` keys, or that the hardware is there: those are checked when
the driver binds, and a device that fails to bind is listed with the reason.

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

The cap can be shorter than 8 s but never longer: that is [the failure rule](concepts.md#the-failure-rule),
and a longer value is refused. There is no host-wide auto-off: only a device's own
`auto_off_after` turns a device off.

## `[board]` identity

The `[board]` table also names the board's identity: `name` and `revision` are the `model` fact, and
`[board.serial]` says where the serial comes from. The table and the four source kinds are in
[Identity facts](identity.md#board-file-syntax).

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
| `spidevB.C` | The SPI device `/dev/spidevB.C` |
| `gpiochipN:LINE` | One line of a GPIO chip |
| `pwmchipN:CHANNEL` | One channel of a PWM chip |
| `ttyNAME` | A serial port, such as `ttyAMA0`. No built-in driver takes one yet; a package can |
| `usb:BUS-PORT` | The USB device plugged into a port, such as `usb:1-1.4` |
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

# `bus`: the one resource this device sits on. Here an I2C adapter, by its kernel name.
[[devices]]
id = "power0"
driver = "ina2xx"
bus = "i2c-1"
config = { chip = "ina238", address = 0x40, shunt_ohms = 0.010, max_current_a = 2.0 }

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
config = { name = "pwmfan" }

# A fan on a PWM channel, driven directly: no kernel fan driver in between. `tach` is optional.
# The duty is 0 to 1000; `min_duty` is the lowest duty the fan turns at.
[[devices]]
id = "channel-fan"
driver = "pwm-channel-fan"
uses = { pwm = "pwmchip0:1", tach = "gpiochip0:20" }
config = { frequency_hz = 25000, min_duty = 200, kick_ms = 500, pulses_per_rev = 2 }

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
config = { vendor = 0x1209, product = 0x0001, manufacturer = "Example", product_name = "Example", serial = "0001", interfaces = "interface 0xff 0 0 example\nendpoint in bulk\nendpoint out bulk\n" }

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
config = { device = "1f00110000.csi" }

[devices.streams.frames]
buffers = 8
buffer_bytes = 2097152
memory = "dma-buf"
heap = "linux,cma"
standby = true                 # capture with nobody attached; a consumer gets the newest frame

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
```

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
