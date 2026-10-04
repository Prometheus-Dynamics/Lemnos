# lemnos-hal

`#![no_std]`, allocation-free hardware vocabulary for Lemnos and the drivers built on it.

- Re-exports [`embedded-hal`](https://docs.rs/embedded-hal) 1.0 and
  [`embedded-hal-async`](https://docs.rs/embedded-hal-async) 1.0: the bus traits every Lemnos
  device driver is written against.
- `register`: register maps over I2C and SPI (8/16-bit addresses, 1-4 byte values, big- or
  little-endian, burst/auto-increment writes, read-back verification, read-modify-write),
  blocking (`RegisterBus`) and async (`asynch::RegisterBus`), with no allocation.
- `power`: `Regulator` and `ClockOutput` traits plus `GpioRegulator` and `FixedClock`.
- `ErrorKind` / `HalError`: one portable classification of failures, shared with the rest of
  Lemnos (`lemnos_core::ErrorKind` is this type).
- `mock` (feature): in-memory I2C targets, SPI, pins, delay, regulators and clocks for tests,
  implementing both the blocking and the async traits.

```rust
use lemnos_hal::register::{AddressWidth, I2cRegisters, RegisterBus};

fn chip_id<I: lemnos_hal::i2c::I2c>(i2c: I) -> Option<u32> {
    let mut regs = I2cRegisters::new(i2c, 0x60, AddressWidth::Bits16);
    regs.read(0x300a, 2).ok()
}
```

Builds for `thumbv7em-none-eabihf`, `riscv32imac-unknown-none-elf` and `wasm32-unknown-unknown`.
