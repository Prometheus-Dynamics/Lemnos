# lemnos-drivers-bmi088

`#![no_std]`, allocation-free driver for the Bosch BMI088 6-axis IMU over any embedded-hal
1.0 I2C bus, blocking (`Bmi088`) and async (`asynch::Bmi088`).

The BMI088 is two dies on one bus: the accelerometer (0x18/0x19) and the gyroscope
(0x68/0x69). `init` checks both chip IDs before writing anything, soft-resets both dies,
takes the accelerometer out of its power-on suspend mode, and applies the ranges and data
rates in `Config` (defaults match the chip's reset values: ±6 g at 100 Hz, ±2000 °/s at
2000 Hz). `read` returns acceleration in m/s² and angular rate in rad/s, alongside the raw
counts; `temperature_c` reads the accelerometer die temperature. `resume` rebuilds a driver
around an already initialized chip, which is how a Lemnos runtime adapter drives it over a
borrowed `lemnos_bus::hal::HalI2cBus`.

```rust
use lemnos_drivers_bmi088::{Bmi088, Config};

fn gravity<I, D>(i2c: I, delay: &mut D) -> Option<[f32; 3]>
where
    I: embedded_hal::i2c::I2c,
    D: embedded_hal::delay::DelayNs,
{
    let mut imu = Bmi088::new(i2c);
    imu.init(delay, Config::default()).ok()?;
    Some(imu.read().ok()?.accel_mps2)
}
```
