# lemnos-drivers-ina2xx

`#![no_std]`, allocation-free drivers for Texas Instruments current/power monitors over any
embedded-hal 1.0 I2C bus, blocking (`Ina`) and async (`asynch::Ina`):

| Chip | Shunt | Readings |
|---|---|---|
| INA226 | external, ±81.92 mV | bus V, shunt V, current, power |
| INA238 | external, ±163.84 mV or ±40.96 mV (picked automatically) | as INA226, plus die temperature |
| INA260 | integrated 2 mΩ | bus V, current, power |

`init` checks the manufacturer and device IDs, then programs continuous conversion and the
current calibration from the shunt resistance and the largest expected current
(`current LSB = max_current_a / 2^15`). Readings are in volts, amperes, watts and °C.

```rust
use lemnos_drivers_ina2xx::{Config, DEFAULT_ADDRESS, Ina, Model};

fn power<I: embedded_hal::i2c::I2c>(i2c: I) -> Option<f32> {
    let mut ina = Ina::new(i2c, DEFAULT_ADDRESS, Model::Ina238, Config::new(0.010, 2.0)).ok()?;
    ina.init().ok()?;
    Some(ina.read().ok()?.power_w)
}
```
