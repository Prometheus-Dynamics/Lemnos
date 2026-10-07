# lemnos-device

The compact Lemnos device model: `#![no_std]`, no allocation, no `String`s.

A driver describes itself with a `'static` `DeviceInfo` (a `DeviceClass` such as `Imu`,
`PowerMonitor`, `Fan` or `Lens`, the `Channel`s it reads and the `ControlInfo`s it accepts)
and implements `Device` plus `Sensor` and/or `Control`. Values are fixed-point `i32`s: a
channel reads `raw × 10^exponent` in its quantity's canonical unit (m/s², rad/s, T, V, A, W,
°C, rpm, ...), so a reading means the same thing whichever driver or backend produced it.
`NO_VALUE` (`i32::MIN`) marks a channel without a valid reading.

```rust
use lemnos_device::{DeviceRef, MAX_CHANNELS};

fn log_all(devices: &mut [DeviceRef<'_>]) {
    let mut values = [0i32; MAX_CHANNELS];
    for device in devices {
        if device.read(&mut values).is_ok() {
            for (channel, raw) in device.info().channels.iter().zip(values) {
                // channel.name, raw, channel.exponent, channel.unit().symbol()
                let _ = (channel, raw);
            }
        }
    }
}
```

- `erased`: `DynSensor`, `DynControl`, `DynSensorControl`, `DeviceRef` (borrowed) and,
  with `alloc`, `BoxedDevice` (owned), for tables and services that hold many drivers.
- `asynch`: the same traits over embedded-hal-async.
- `kernel`: how a chip's channels map onto its mainline Linux IIO or hwmon driver, as data,
  so a generic binding serves the same device without per-chip code.
- `fixed`: `rescale` between decimal exponents; with `float`, `to_f32`/`from_f32`.
- `DeviceStatus`: the status every Lemnos layer reports (`lemnos_core::DeviceStatus` is
  this type).

The built-in drivers implement it: `lemnos-drivers-bmi088` (`Imu`, 6 channels),
`lemnos-drivers-bmm150` (`Magnetometer`, 3), `lemnos-drivers-ina2xx` (`PowerMonitor`, 4 or
5), `lemnos-drivers-vcm` (`Lens`, a `position` control). See `docs/compact-model.md`.
