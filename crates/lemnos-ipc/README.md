# lemnos-ipc

The `lemnosd` socket protocol and its clients, modelled on Styx's `FrameClient` and
`ControlClient`:

```rust
use lemnos_ipc::{ClientEvent, ClientOptions, LedStatus, Update};

let mut devices = ClientOptions::new("/run/lemnos/lemnosd.sock", "helios")
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

let mut leds = ClientOptions::new("/run/lemnos/lemnosd.sock", "photonvision").leds()?;
leds.status(LedStatus::Warn)?;
leds.progress(0.4, None)?;
leds.set_leds(&[(0, 0xff0000)])?;              // logical index 0 is the ring's top
```

`wire` is the protocol: length-prefixed little-endian frames over a Unix stream socket, with
readings as fixed-point integers and their exponents in the device list.
