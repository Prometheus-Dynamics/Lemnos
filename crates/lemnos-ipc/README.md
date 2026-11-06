# lemnos-ipc

The `lemnosd` socket protocol and its clients, modelled on the earlier stack's `FrameClient` and
`ControlClient`:

```rust
use lemnos_ipc::{ClientEvent, ClientOptions, LedStatus, Update};

let mut devices = ClientOptions::new("/run/lemnos/lemnosd.sock", "app")
    .reconnecting()
    .devices()?;
let imu = devices.read("imu")?;
let ax = imu.value("acceleration.x");          // f64, m/s²
devices.subscribe("imu", 10)?;
devices.set("fan", "duty", 0.8)?;              // the applied value, or Refused(..)
match devices.next_event()? {
    ClientEvent::Connected { reconnects } => {}  // subscriptions restored
    ClientEvent::Disconnected { error } => {}
    ClientEvent::Data(Update::Reading(r)) => {}
    ClientEvent::Data(Update::Event(e)) => {}
}

let mut leds = ClientOptions::new("/run/lemnos/lemnosd.sock", "vision").leds()?;
leds.status(LedStatus::Warn)?;
leds.progress(0.4, None)?;
leds.set_leds(&[(0, 0xff0000)])?;              // logical index 0 is the ring's top
```

Raw access through the service (claims end with the connection; see `docs/system-service.md`,
"Raw bus and line access"):

```rust
use lemnos_ipc::{LineTarget, raw::LineConfig};

let line = devices.claim_line(LineTarget::Name("aux-1".into()), LineConfig::output(true))?;
line.set(&mut devices, false)?;
let id = devices.i2c("1", 0x50).read_reg8(&mut devices, 0x00)?;
let rx = devices.spi(0, 0).xfer(&mut devices, &[0x9f, 0, 0], Default::default())?;
devices.restore("fan", None)?;                 // undo this client's writes (automatic on disconnect)
```

The advisory GPU lease (`docs/ipc.md` section 6.6): it orders the clients that ask and enforces
nothing; leases end with the connection.

```rust
use lemnos_ipc::{GpuAcquire, GpuClient, GpuMode};
use std::time::Duration;

let mut gpu = GpuClient::connect("/run/lemnos/lemnosd.sock", "bench")?;
if gpu.supported() {
    let request = GpuAcquire { mode: GpuMode::Exclusive, label: "bench".into(), ..Default::default() };
    let lease = gpu.acquire(request, Some(Duration::from_secs(60)))?; // queue for up to 60 s
    // ... the GPU work ...
    gpu.release(lease)?;
}
```

`ClientOptions::wait(timeout)` waits for a service that is not up yet; LED clients receive
events only with `ClientOptions::events(true)`. The compact-model types the protocol uses
(`DeviceClass`, `Quantity`, `Unit`, `Axis`, `DeviceStatus`, `ErrorKind`) and `lemnos_hal::raw`
are re-exported.

`wire` is the protocol: length-prefixed little-endian frames over a Unix stream socket, with
readings as fixed-point integers and their exponents in the device list.
