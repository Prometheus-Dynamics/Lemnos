//! The hardware vocabulary of Lemnos, without `std` or allocation.
//!
//! - Bus traits come from [embedded-hal] 1.0 and [embedded-hal-async] 1.0,
//!   re-exported here ([`i2c`], [`spi`], [`digital`], [`delay`], [`pwm`],
//!   [`asynch`]). Lemnos device drivers are written against them, so they run
//!   on Linux (`lemnos-linux`) and on microcontrollers alike.
//! - [`register`] adds register maps over I2C and SPI (blocking and async).
//! - [`power`] adds regulators and clock outputs.
//! - [`ErrorKind`] and [`HalError`] classify failures portably.
//! - [`mock`] (feature `mock`) provides in-memory doubles for tests.
//!
//! [embedded-hal]: https://docs.rs/embedded-hal
//! [embedded-hal-async]: https://docs.rs/embedded-hal-async

#![no_std]
#![forbid(unsafe_code)]

mod error;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod power;
pub mod register;

pub use embedded_hal;
pub use embedded_hal::{delay, digital, i2c, pwm, spi};
pub use embedded_hal_async;
pub use error::{ErrorKind, HalError};
pub use power::{ClockOutput, FixedClock, GpioRegulator, Regulator};
pub use register::{
    AddressWidth, Endian, I2cRegisters, RegWrite, RegisterBus, RegisterError, SpiRegisters,
};

/// The async bus traits ([`embedded_hal_async`]).
pub mod asynch {
    pub use crate::register::asynch::RegisterBus;
    pub use embedded_hal_async::{delay, digital, i2c, spi};
}
