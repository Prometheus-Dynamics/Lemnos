# Concepts

Seven words carry the whole of Lemnos. This page defines each. The commands assume the daemon is
running on the simulated board, as in the [README](../README.md):

```sh
export LEMNOSD_SOCKET="${TMPDIR:-/tmp}/lemnosd.sock"
lemnosd --sim --board docs/examples/sim-board.toml &
```

## Device

A device is one entry in the board file: a name, the driver that runs it, and what it is wired to.
A chip is a device. So is something built from other devices, such as an orientation made from an
accelerometer and a gyroscope. A camera is three devices: a sensor, a receiver and an ISP.

Lemnos does not know what a device is. It knows its name, its driver, its tags and its state.

```sh
lemnos-ctl list
```

## Contract

A contract is a small set of methods with a name and a version, written as a tag: `fan/1`,
`camera.isp/1`. Each method has a fixed number. A program that knows a contract can use any device
that carries its tag, whatever chip is behind it.

Contracts live in `contracts/`, one crate each. The engine never imports them. On the wire a call is
a contract's position on the device, a method number and bytes. Nothing on the wire names a kind of
device. [Drivers and contracts](drivers.md) lists every contract.

## Driver

A driver is the code that talks to one part. It states what it **needs** (a bus, a line, another
device, a stream) and which contracts it **provides**. The host hands it what it needs when it
binds. A driver comes in one of three forms: a crate built into the daemon, a text file that
describes a simple chip, or a compiled package the daemon loads at start. See
[Writing a driver](writing-a-driver.md).

A driver knows how to talk to its part. It does not know what the caller's work means. An ISP
driver takes a configuration as bytes and checks every address in it. It does not do 3A. An NPU
driver takes a compiled program as bytes. It does not compile one.

**Movers don't know what they move.** What carries data between a driver and a program (a frame
stream, its job ring, a bus, the socket) carries opaque bytes with an identity, a time and a
memory location, and nothing else. It has no frame type, pixel format or codec. What the bytes
are is the contract's and the driver's business. A driver that programs a block, such as an ISP,
a scaler or a codec, still describes the layouts the block's registers need: that is the
hardware's configuration, not a type in the mover. Times are `CLOCK_MONOTONIC` nanoseconds on
Linux. See [the design](design.md#movers-dont-know-what-they-move).

## Description

Every device describes itself: its contracts and their methods, and a list of **items**. An item is
a **value** (something to read), a **control** (something to set) or an **action** (something to
do), each tied to a method, with its unit and range. This is how a tool shows and drives a device
it has no code for.

```sh
lemnos-ctl describe counter
lemnos-ctl read counter
```

## Resource

A resource is something a driver is given: an I2C bus, an SPI device, a GPIO line, a PWM channel, a
UART, a USB device, this board's USB device port, a register window, an interrupt, DMA memory,
clocks, a reset, a frame stream, or another device. The board file says which, by name. What the
name means is the backend's business: `i2c-1` on Linux, a name of the firmware's choosing on a
microcontroller, anything a test adds on the simulated board.

Most resources have one holder at a time. An I2C bus is shared by every device on it when it goes
through the stock `i2c-dev` node. With the [kernel module](kernel-module.md) the holder is an address,
not the bus: the first device to transfer to an address claims it (`i2c-<bus>:0x<addr>`) and keeps it
for as long as its bus handle lives, and a second device at the same address is refused with BUSY
until the first lets go. The claim goes back when that device is dropped, and also when the address
stops answering (the transfer fails with ENXIO or EREMOTEIO), so probing an empty address keeps
nothing. Devices at other addresses of the same bus are not affected. When a driver is dropped,
everything it held goes back.

The Linux backend hands out I2C, SPI, GPIO, PWM, UART, USB devices and the USB device port. For a
block bound to the [kernel module](kernel-module.md) it also hands out the block's DMA memory,
register windows, interrupts, clocks and resets, and nothing else stands in for them (see
[the resource names](boards.md#resource-names)). The block drivers built in today open their own
kernel nodes instead, through their `-sys` crates.

## State and health

A device is in one of three states:

| State | Shown as | Meaning |
|---|---|---|
| Running | `running` | Its driver is bound and takes calls |
| Backing off | `retrying` | Binding failed or the driver was dropped. The host will try again |
| Off | `off` | Someone turned it off, or the board file says `disabled = true` |

A running driver also reports its health: `ok`, `degraded` (working, with something missing, such
as a fused orientation without its magnetometer) or `failed` (not working now, still trying). A
reason in words comes with anything that is not `running` and `ok`.

```sh
lemnos-ctl off counter
lemnos-ctl list
lemnos-ctl on counter
```

## The failure rule

Hardware goes away and comes back. Lemnos never gives up on it, and never hides that it is gone.

- **A bind that fails is retried forever.** The wait starts at 1 s and doubles to a cap of 8 s. The
  wait resets once a driver has stayed up for 8 s, so a driver that fails straight after every bind
  is tried less and less often, never in a tight loop.
- **A driver retries its own hardware.** A chip that stops answering makes the driver report
  `failed`, and the next read tries again. A block driver closes and reopens its node, also with a
  wait capped at 8 s.
- **A driver whose hardware is gone for good is bound again.** A driver says so itself (the handle
  of an unplugged USB device), or its calls fail five times in a row the way a missing device
  fails. The host drops it, takes back its resources and binds it again under the rule above.
- **Nothing is switched off automatically** unless its board entry asks. `lemnos-ctl off` and `on`
  are the switch. A device with `auto_off_after = N` in the board file opts in to being turned off
  after N failed binds in a row; no setting turns that on for any other device. The waits and the
  call deadline are set in the board file's [`[host]`](boards.md#the-keys) section; the cap can be
  made shorter than 8 s, never longer.
- **A panic or a hang in one driver stays in that driver.** Each call runs behind a panic boundary
  and a deadline, 2 s unless the board file sets another. A driver that panics is dropped and bound again. Other devices carry on.
  Memory corruption inside the daemon's process cannot be contained.
- **Hotplug events are hints.** When the kernel announces a bus or a device that the board file
  names, the host tries the devices waiting on it at once, at most once every 250 ms for each
  device. A lost event costs only time.

What is retried is the connection to the hardware. **A caller's request is never redone and never
replaced.** A call that fails returns its failure to the caller. A job the hardware cannot do
fails. Lemnos does not run it again later, and does not compute the answer some other way.

A driver that panics shows the rule at work. The call fails, the device is `retrying`, and after
the wait it is `running` again:

```sh
lemnos-ctl set counter crash || true
lemnos-ctl list
```

## Who owns the hardware

By default Lemnos owns a device's hardware. A board file can say `owner = "host"` for hardware the
operating system's own driver already runs. Lemnos then only reads it. A method that would change
the hardware is refused with `NOT_PERMITTED`, the driver's own writes are refused before they reach
the bus, and a driver that has not said it can work this way is not bound at all.

## Callers

Every program reaches devices through the daemon's socket. A call is carried to the driver and its
answer comes back. A program can also **subscribe** to a method: the daemon then reads it at the
period asked for and sends each value, or only the values that changed. A driver can
ask to be told who is calling and when a caller has gone, which is how a lease ends with the
connection that held it.

Some work takes longer than a call should: writing a large file, a step that runs for seconds. A
contract marks such a method as a **long job**. Calling it starts the job and returns its number at
once. The daemon then sends the caller the job's progress and, when it ends, its result. The caller
can cancel it. A job belongs to the connection that started it: when that connection closes, the
job is cancelled. Drivers keep nothing across a restart of the daemon, so a job a restart cut short
ends as interrupted, and is not picked up again.

```sh
lemnos-ctl watch accel z --period-ms 50 --count 3
```

```sh
kill %1
wait
```
