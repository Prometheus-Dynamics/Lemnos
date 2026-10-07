# lemnos-lite

A static device table over the compact device model (`lemnos-device`), for firmware and
small Linux images that know their hardware at build time. `#![no_std]`, no allocation.

```rust
use lemnos_lite::{Devices, MAX_CHANNELS, sensor};

let mut imu = lemnos_drivers_bmi088::Bmi088::new(i2c_imu);
let mut power = lemnos_drivers_ina2xx::Ina::new(i2c_power, 0x40, Model::Ina238, cfg)?;
let mut devices = Devices::new([
    sensor("imu", &mut imu).every(10),       // poll every 10 ms
    sensor("power", &mut power).every(100),
]);
devices.init_all(&mut delay);
let mut buf = [0i32; MAX_CHANNELS];
devices.read("power", &mut buf)?;              // by name or by index
let status = devices.status(1);                // the same DeviceStatus as the runtime
devices.poll(now_ms, &mut delay, &mut buf, |reading| {
    // reading.name, reading.info.channels, reading.values
});
```

- Entries hold `&mut dyn` devices (`DeviceRef`), so code size stays flat as devices are
  added. Names are `&'static str`.
- Each entry tracks a `DeviceStatus` (`missing` until its first successful `init`), the
  last `ErrorKind`, and an optional polling period. `poll` re-initializes devices that are
  not up on their schedule, so a sensor that was absent at boot or dropped off the bus comes
  back by itself.
- `sensor`, `control` and `device` (both) build entries; `read`, `set`, `get`, `set_named`
  address them by index or name.

No discovery, driver matching, event log or allocation. The same code runs on a
microcontroller (buses from its HAL) and on Linux (buses from `lemnos_linux::hal`):
`cargo run -p lemnos-lite --example linux_sensors -- <i2c bus>` reads the Raze's IMU,
magnetometer and power monitor. `scripts/check-sizes.sh` tracks a firmware image (`lite`)
and a Linux image (`sensors-lite`) built on it.
