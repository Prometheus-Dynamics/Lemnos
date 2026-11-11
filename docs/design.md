# Lemnos: the design

This is the design Lemnos is built to: what it is, where its lines are drawn, and why. The other
documents say how to use it; start from the [README](../README.md) for those.

## What Lemnos is

Lemnos is only the engine. It talks to hardware and it does not know what the hardware is. It is
given a driver, the driver says what it needs to talk to, and the driver describes the device to
whoever needs it.

**Lemnos knows:**

- How to speak: I2C, SPI, GPIO, PWM, USB, registers, interrupts, DMA, clocks.
- Who holds what (claims).
- How to carry a call, a value and a stream of buffers.
- How drivers live: start, health, retries, containment.

**Lemnos does not know:**

- What a camera, IMU, fan or light is. Those are tags, and it only matches the strings.
- What any device's methods or values mean.
- Any board. A board is a data file it is handed.

## The system

```text
   an application   another one     lemnos-orion      lemnos-ctl, tools
        |                |                |                  |
        +----------------+-------+--------+------------------+
        |                        v
        |              One Lemnos client
        |   open by name or tag, then call, read values, or take a stream
        |                        |
        |                        | call, values, stream setup
        |                        v
        |   +--------------------------------------------------------+
        |   | The host (lemnosd on Linux, static on an MCU,          |
        |   |           a library on a PC)                           |
        |   |                                                        |
        |   |  Engine: board file, bind order, claims, health,       |
        |   |          retries, containment                          |
        |   |  Drivers: chosen by the board file                     |
        |   |  Backend interface: what every driver is written       |
        |   |          against                                       |
        |   +--------------------------------------------------------+
        |              |                |            |          |
        |     Linux through      Linux stock        MCU     Simulated
        |       lemnos.ko         interfaces    embedded-hal  QEMU, CI
        |      (standard)      guest boards, PC
        |              |
        |          lemnos.ko
        |              |
        +--------------+
   frames in, next job back: a stream lemnosd set up. This is the only bypass, and it is for speed.
```

Every application uses the same client to reach the host. On Linux, lemnosd runs all drivers and
is the only process that opens `lemnos.ko`. The frame stream is created by lemnosd, and after setup
no frame crosses a socket.

## Repository layout

```text
engine/      abi, hal, core, macros, host, wire, client, capi, measure, package, host-static
backends/    linux-sys, linux, linux-stream, v4l2, usb, mcu, sim
service/     lemnosd, ctl, orion
kernel/      lemnos.ko source
contracts/   one small crate per tag family
drivers/     one flat folder per driver (and drivers/<name>-sys for its unsafe layer)
testing/     crates/ (test tools), boards/, bench/, qemu/, boundary/, board-runs/
docs/        this file, the protocol specification, and the guides
```

Rules:

- `engine/` never imports `contracts/`, `drivers/`, `backends/` or `service/`.
- No folders by kind of hardware. `drivers/` is flat.
- Drivers and contracts live in this repository for now. Because the engine never imports them,
  they can be lifted out later.
- Every directory directly inside `engine/`, `backends/`, `service/`, `contracts/`, `drivers/` and
  `testing/crates/` must be a crate, and no plain files go at that level. Cargo fails otherwise.

## Drivers, tags and contracts

A driver states what it **needs** and what it **provides**. A need is either something to speak to
or another device. There is no special kind of driver for a device built from other devices.

Each driver tags its device with strings such as `camera.sensor/1`, `camera.receiver/1`,
`camera.isp/1`, `isp.pisp/1`, `camera.frames/1`, `accelerometer/1`, `gyroscope/1`, `magnetometer/1`,
`orientation/1`. Lemnos only matches strings. Each tag has a small contract in `contracts/`.

```rust
// In contracts/. The engine never sees what this means.
#[contract("accelerometer", version = 1)]
pub trait Accelerometer {
    #[method(1)]
    fn read(&mut self) -> Result<Vector3>;
}

// A driver states what it needs and provides. `Lifecycle` holds what its author writes.
#[driver("bmi088", provides = [Accelerometer, Gyroscope])]
pub struct Bmi088 { #[needs] bus: I2c, .. }

// A device built from others needs nothing new.
#[driver("orientation", provides = [Orientation])]
pub struct OrientationDriver {
    #[needs] accel: Dev<dyn Accelerometer>,
    #[needs] gyro:  Dev<dyn Gyroscope>,
    #[needs(optional)] mag: Option<Dev<dyn Magnetometer>>,
}
```

Each contract method declares a stable number with `#[method(N)]`.

Every device carries a self-describing description: values, units, controls and actions. So
`lemnos-ctl`, Orion and any other tool can show a device they have no code for.

The host starts children first, makes direct calls in process, reads on demand, and passes health
upward (a missing optional magnetometer means a degraded 6-axis device).

## Board files

A board is data, and it lives outside Lemnos in a device package. Test boards live in
`testing/boards/`.

```toml
format = "lemnos.board"
schema_version = 1

[board]
id = "example"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "pio-i2c:sda=8,scl=7"

[[devices]]
id = "orientation"
driver = "orientation"
uses = { accel = "imu", gyro = "imu", mag = "mag" }

[[devices]]
id = "cam0-sensor"
driver = "ov9782"
bus = "i2c-10"
link = "csi0"

[[devices]]
id = "csi0"
driver = "rp1-csi2"
link = "isp0"

[[devices]]
id = "isp0"
driver = "pisp-fe"
```

[Board files](boards.md) has every key.

## Cameras

There is no camera driver. A camera is a sensor driver (`ov9782`; any chip), a receiver
driver (`rp1-csi2`, `rkcif`, `sunxi-vin`; per chip family) and an ISP driver (`pisp-fe`, `rkisp`;
per chip family). The consumer finds a `camera.sensor/1` tag and follows `link` to its receiver and ISP.
On another board only the receiver and ISP drivers change, and only if the chip family is new. A USB
camera is one driver providing `camera.frames/1`, with a board entry that names its port or its
vendor and product, and no ISP. It binds when the camera is plugged in. The sensor is driven through Lemnos, so the standard path has no kernel sensor driver and no
device tree sensor entry.

3A, tuning and the camera pipeline are the camera stack's, not Lemnos's. A proof tool
(`testing/crates/camera-proof`) does what a camera stack does, as far as the contracts go.

## The block rule

The BCM2712 board is the template for every hardware block. Each block gets a driver that knows how to talk to
it and takes work from whoever calls it. No vendor library lives in Lemnos. What runs on a block (a
compiled model, an encoder's settings, 3A) is the caller's business.

| Block on that board | How it works |
|---|---|
| Video decode | The kernel's stateless decoder, given to a caller as `codec.block/1`. The caller parses the bitstream and supplies the controls. No vendor library. |
| ISP (PiSP) | Lemnos exposes config and statistics. The camera stack does 3A and tuning. |
| A USB accelerator | Lemnos loads the firmware the board file names, and never reads it. |

An NPU driver takes a compiled program as bytes and runs it. Lemnos never reads or compiles it.
Because a program can name memory addresses, each NPU driver must confine a job to its own buffers
using the chip's memory protection. Where a chip gives user space no way to do that, the driver
refuses to run programs, and says what a kernel path would need. [Drivers and
contracts](drivers.md#library-crates) has the state of each.

## Backends and targets

| Target | Host | Backend | Board data |
|---|---|---|---|
| Boards we manage | lemnosd | Linux through `lemnos.ko` | board file in the device package |
| A board its own operating system runs | lemnosd as a guest | Linux stock interfaces, `owner = "host"` read-only | board file |
| MCU | static table, no allocator | embedded-hal, direct calls | compiled in |
| Other Linux boards | lemnosd | `lemnos.ko`, or the vendor kernel driver per block until module code exists | board file |
| PC and tests | library, no daemon | stock or simulated | test data |
| QEMU and CI | lemnosd | simulated | test data |

## The kernel module

On boards we manage, `lemnos.ko` is the way in. It exposes claims (enforced by the kernel and
released on close), buses by calling the kernel's own bus cores (I2C and SPI transfer lists, GPIO,
PWM, PIO), registers, interrupts, DMA and dma-buf, power, clocks, resets, thermal, hotplug and fault
events, and frame streams. Block-specific code exists where DMA needs checks.

Staying outside the module: the GPU (Mesa opens the render node; Lemnos holds only an advisory
lease), USB (`usbfs` is already the kernel's own interface; lemnosd still handles claims), network
and storage.

USB is in the backend interface on every target, in both roles: talking to something plugged in
(`UsbHost`, `UsbDevice`) and this board appearing as a USB device (`UsbFunction`).

## The frame stream

lemnosd creates buffers and a shared ring. The buffers are shared memory, or one dma-buf each when
hardware fills them. The producer (a driver, or the module for a block whose DMA it checks) marks
frames and wakes the consumer. The consumer posts the next job (an ISP config, an image to scale)
on the ring. Whoever hands a job to hardware checks every buffer it names. Lemnos carries an opaque
`u64` frame identity it never interprets. Image, codec and USB jobs use the same ring.
[Frame streams](streams.md) has the details.

## Driver forms

- **Built in:** a crate in the image.
- **Description:** a data file run by a generic sensor driver, with no code.
- **Package:** a compiled file lemnosd loads through `lemnos-abi`. Signed mode or open mode (give it
  a driver and it runs), with one switch between them.

In-process memory corruption can't be contained. Panics and hangs can.

## The failure rule

Never give up. Retry backoff is capped at 8 s and clears itself. No default auto-off. Manual off and
on are easy. Auto-off exists only as a configured policy. Backed-off and disabled state are visible.
Access paths (USB gadget, serial, network, SSH) are never backed off.

A driver whose resources are dead for good (an unplugged device's handle) reports `Health::Gone`,
and the host drops it and binds it again under this rule. The backoff clears once a driver has
stayed up for 8 s, so a driver that fails straight after every bind is bound less and less often,
never in a loop.

Hotplug events are hints, for every board-file name the kernel announces: a USB device, an I2C
adapter, a spidev node, a GPIO chip, a PWM chip, a tty, a video or media node. The backend turns
an event into the board-file names it may be about, and
the host tries the devices waiting on those names now and ticks the running ones so their drivers
can look. A hint only brings a retry forward, never closer than 250 ms to the device's last bind, so
a lost event costs time and a storm of them costs nothing. The host never learns what arrived.

Services are built with `panic = "unwind"`. With `panic = "abort"` a driver's panic could not be
contained.

## Shared interfaces

Everyone builds against these. `engine/core`, `engine/macros` and `engine/client` are the source of
truth; what follows is abridged.

```rust
// engine/core. A tag is "name/major".
pub struct Manifest { pub name: &'static str, pub needs: &'static [Need], pub provides: &'static [ContractInfo] }
pub struct Need { pub name: &'static str, pub kind: NeedKind, pub optional: bool }
pub enum NeedKind { Bus(BusKind), Device(Tag) }

pub trait Driver: Sized {
    const MANIFEST: Manifest;
    fn bind(res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<Self>;
    fn describe(&self, out: &mut Description);
    fn health(&self) -> Health { Health::Ok }
    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status; // from #[driver]
}

// What a driver's author writes when using #[driver].
pub trait Lifecycle: Sized {
    type Needs; // generated from the #[needs] fields
    fn start(needs: Self::Needs, cfg: &Config<'_>) -> Result<Self>;
    fn describe(&self, out: &mut Description) {}
    fn health(&self) -> Health { Health::Ok }
    const TICKS: bool = false; // true to be called below
    fn tick(&mut self, now_ns: u64) -> Option<u64> { None } // does what is due, returns when next
    const TRACKS_CALLERS: bool = false; // true to be told who is calling, and when they go
    fn caller(&mut self, caller: CallerId) {} // just before each method
    fn caller_gone(&mut self, caller: CallerId) {} // give up what it held
    const HOST_OWNED: HostOwned = HostOwned::Refused; // whether it may bind to a host-owned device
}

// A method is its contract's position in `provides`, plus the number the contract declared.
pub struct MethodId { pub contract: u8, pub method: u16 }

// A #[needs] field is anything implementing Resource: I2c, Spi, Line, Pwm, Uart, UsbDeviceHandle,
// UsbFunctionHandle, RegistersHandle, InterruptHandle, DmaHandle, ClocksHandle, ResetHandle,
// StreamHandle, Dev<dyn SomeContract>, or an Option of any of those.
impl Resources<'_> {
    fn i2c(&mut self, need: &str) -> Result<I2c>; // and one for each kind above
    fn device<C: Contract + ?Sized>(&mut self, need: &str) -> Result<Dev<C>>;
    fn link(&self) -> Option<&str>;
}

// engine/client. The one way in.
impl Client {
    fn connect(path: impl AsRef<Path>, name: &str) -> Result<Self, Error>;
    fn list(&self) -> Result<Vec<DeviceEntry>, Error>;
    fn find(&self, tag: &str) -> Result<Option<DeviceEntry>, Error>;
    fn open<C: Contract + ?Sized>(&self, name: &str) -> Result<Bound<C, RemoteCaller>, Error>;
    fn describe(&self, device: u32) -> Result<DeviceDescription, Error>;
}
```

Non-negotiable properties: 0 allocations on steady-state call, value and stream paths; nothing on
the wire names a kind of device; bounded queues.

## What came from the earlier version

Lemnos was rebuilt from an empty tree. The earlier version is the `dev` branch, which stays as it
was. Read it with `git show origin/dev:<path>`.

| | From the earlier version |
|---|---|
| **Ported and adapted** (proven on hardware) | `abi`, `linux-sys`, `v4l2`, `usb`, `uevent`, `capi`, chip drivers, `fusion`, `light`, `pisp` and other block drivers, register maps, module personalities, `soc` |
| **Cut down** | `hal`, `linux`, `mock`, the board parser |
| **Written new** | driver model, macros, host, containment, wire, client, `lemnosd`, `lemnos-ctl`, `lemnos-orion`, contracts, frame stream, PiSP front end |
| **Left behind** | the `lemnos` facade, `runtime`, `registry`, `driver-sdk`, `driver-manifest`, old `macros`, `core`, `bus`, `discovery`, `device`, `drivers-{gpio,i2c,spi,uart,usb,pwm}`, `sensor-bridge` and its module, `lemnos-ov9782.ko`, the look catalogue and presets, update-file polling, old CI and size baselines |

## Proof on hardware

The code is written and tested without boards, so board checks run afterwards, one per piece of
hardware-facing code, from the files in `testing/board-runs/`. Timings are quoted only from the
reference board, a BCM2712 compute module, using Lemnos's own tools with the board's other
services stopped, beside the earlier version's daemon on the same scene.
[Testing](testing.md#board-runs) says how a run is done, and [Hardware
status](hardware-status.md) what is still unproven.

The reference set (`testing/bench` is its harness):

- IMU at 200 Hz: delivered, misses, largest gap, read time. The earlier version: 9000 of 9000,
  5.1-5.2 ms gap, 555-576 µs reads.
- lemnosd CPU at idle, 50 Hz and 200 Hz. The earlier version: 0.7-0.8 %, 1.4-1.5 %, 3.1-3.2 %.
- Steady-state allocations (0 in the earlier version), memory, thread count and binary size of
  `lemnosd` and `lemnos-orion`.
- Light frame rate, fan steps, power readings, USB port power cycle.
- Camera chain at 120 fps: drops, end-to-end latency, 3A latch frames.

## Known risks

- **The PiSP front end** has never run through the module. The receiver was proven, in the earlier
  version, only at RAW10 60 fps. The back end round trip there was 511 µs against 505 µs stock, so
  the gain is ownership, not speed. A stuck back end still needs a reboot.
- **Exposure and gain at frame cadence** must hit latch frames. If posting values on the ring
  misses them, the module would have to run a prepared register list at frame start.
- **Bus calls through the module** are unmeasured against the stock paths on a real bus. A bus
  that loses stays on its stock path while it's fixed.
- **The module has to build for every kernel we ship,** including vendor kernels.
- **Blocks that user space cannot confine.** Some vendor kernel interfaces let any opener name any
  address. Those drivers refuse the work until the module has a path for the block.

## Settled decisions

- Lemnos is the engine; drivers and contracts are folders in this repository for now.
- The rebuild started from a clean slate; `dev` is a frozen reference.
- Breaking other repositories doesn't count against a change. The only outside test is whether the
  capability exists.
- The GPU stays with Mesa. Lemnos holds only an advisory lease.
- USB on Linux stays on `usbfs`.
- K230 capture is direct through `/dev/vvcam-isp.0`. The vendor daemon is never used.
- No vendor library lives in Lemnos (see the block rule). The dynamic loader is used for one thing:
  Lemnos's own driver packages.
- Hardware's work is never done in software in its place. A job a block cannot do fails.
- No sensor bridge. No look catalogue in Lemnos.
