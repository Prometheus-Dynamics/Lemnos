# lemnos-drivers-bmm150

`#![no_std]`, allocation-free driver for the Bosch BMM150 geomagnetic sensor over any
embedded-hal 1.0 I2C bus, blocking (`Bmm150`) and async (`asynch::Bmm150`).

`init` leaves suspend mode, checks the chip ID, reads the factory trim registers, and starts
normal mode with a repetition `Preset` (low power, regular, enhanced, high accuracy) and a
`DataRate` (2-30 Hz). `read_fixed` returns the field in 1/16 µT using Bosch's integer
compensation (within about 0.35 µT, one sensor LSB, of the float version); with the
default `float` feature, `read` returns µT from the floating-point compensation. An axis
that overflowed reads as `None`. `resume` rebuilds a driver around an
already initialized chip from its saved `Trim`.

```rust
use lemnos_drivers_bmm150::{Bmm150, Config, DEFAULT_ADDRESS};

fn heading_inputs<I, D>(i2c: I, delay: &mut D) -> Option<(f32, f32)>
where
    I: embedded_hal::i2c::I2c,
    D: embedded_hal::delay::DelayNs,
{
    let mut mag = Bmm150::new(i2c, DEFAULT_ADDRESS);
    mag.init(delay, Config::default()).ok()?;
    let field = mag.read().ok()?;
    Some((field.x_ut?, field.y_ut?))
}
```
